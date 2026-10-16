//! Address-resolution core.
//!
//! `search()` is the one ranked-retrieval pipeline behind `/geocode`,
//! `POST /batch/geocode`, the `crabfinder-batch` CLI and eval batch mode.
//! It walks a fixed list of tiers (see `Tier`) and returns the candidates
//! of the first tier that matches, each labelled with a `Confidence`.
//! `resolve()` reduces that to the single decision the batch contract
//! needs (`design-batch-geocoding` §3.1), swapping in the street's median
//! coordinates for street-level outcomes.
//!
//! No HTTP, no CSV, no CLI plumbing — those belong to the façades.

use std::sync::Arc;

use std::collections::HashMap;

use tantivy::query::{BooleanQuery, Occur, TermQuery};
use tantivy::schema::{IndexRecordOption, Value};
use tantivy::{Index, IndexReader, TantivyDocument, Term};
use tracing::warn;

use crabfinder_core::house_number::leading_number;
use crabfinder_core::{Address, MultilingualName};
use crabfinder_indexer::index::CrabSchema;
use crabfinder_indexer::localities::{Locality, LocalityKind};
use crabfinder_parser::ParsedQuery;

use crate::abbreviation::AbbreviationMap;
use crate::addr_count::SharedAddressCountIndex;
use crate::dedup::{MatchType, RAW_HIT_CAP, RawHit, group_by_building, raw_limit};
use crate::focus::{FOCUS_HALF_LIFE_M, LOCALITY_DECAY_M, apply_decay};
use crate::hydrate::{HydratedHit, hydrate};
use crate::landmarks::{Landmark, SharedLandmarkIndex};
use crate::localities::{Province, SharedLocalityIndex};
use crate::mid_street::{MidStreetIndex, street_key_from_bgid};
use crate::query_builder::{
    Area, BBox, FilterParams, analyze, build_area_query, build_fuzzy_query, build_strict_query,
    build_typo_query, search_score_only, search_with_notability, searchable_field, slot_is_unknown,
};
use crate::spatial::haversine_m;
use crate::validate::Validation;

/// Structured address input. Both the HTTP free-text path (after parsing) and
/// the batch CSV path (already-structured columns) construct `AddressQuery`.
#[derive(Debug, Clone, Default)]
pub struct AddressQuery {
    /// Street tokens, *already analyzed* (lowercased, ascii-folded, split on
    /// hyphens). Free-text callers run `analyze()` first; batch callers run
    /// `analyze_street_input()` (the helper below) to apply the same
    /// normalization to a raw user string.
    pub street_tokens: Vec<String>,
    /// Numeric house number with optional alphabetic suffix (`"32"`, `"32B"`).
    /// Matched case-insensitively (`query_builder::house_number_clause`).
    pub house_number: Option<String>,
    /// Box / unit hint (`bte 3`). Not a filter: a unit whose box matches
    /// becomes its building's representative (`prefer_box`).
    pub box_number: Option<String>,
    /// 4-digit Belgian postal code as a string. Part of the address: when
    /// nothing matches strictly inside it, the resolver retries around it
    /// (`Confidence::AdjacentCommune`, v2 Phase 10) unless
    /// `postal_code_strict`.
    pub postal_code: Option<String>,
    /// Treat `postal_code` as a hard filter (`/geocode?postal_code=`).
    pub postal_code_strict: bool,
    /// Indexed ISO region form (`BE-BRU` / `BE-VLG` / `BE-WAL`). Optional
    /// hard filter — passed straight through to the Tantivy MUST clause.
    pub region_filter: Option<String>,
    /// Optional geographic hint as `(lat, lng)`. When set, results are
    /// re-scored after retrieval by exponential distance decay
    /// (`focus::apply_focus_decay`).
    pub focus_point: Option<(f64, f64)>,
    /// Decay length of `focus_point`, metres; `None` = `FOCUS_HALF_LIFE_M`.
    pub focus_decay_m: Option<f64>,
    /// Other readings of the same free text (parser alternatives, Phase 2),
    /// tried in order when this one has no strict-tier hit. Alternatives
    /// never carry alternatives of their own.
    pub alternatives: Vec<AddressQuery>,
    /// Hard geographic restriction. Set by the resolver itself on the
    /// locality-suffix reading (`locality_suffix_reading`, Phase 6).
    pub area: Option<Area>,
    /// Caller's hard rectangle (`/geocode?bbox=`, v2 Phase 10).
    pub bbox: Option<BBox>,
    /// How many trailing `street_tokens` came from a separate city field
    /// (`/geocode?city=`, the batch `city` column). Only `/validate`
    /// reads it, to compare those words with the address's place names
    /// (v3 Phase 11.2); the search treats them as street words.
    pub city_tokens: usize,
}

impl AddressQuery {
    /// Build from a parsed free-text query (street tokens analyzed with the
    /// `crab` analyzer), alternatives included.
    pub fn from_parsed(index: &Index, parsed: &ParsedQuery) -> Self {
        let one = |p: &ParsedQuery| AddressQuery {
            street_tokens: p
                .street_tokens
                .iter()
                .flat_map(|t| analyze(index, t))
                .collect(),
            house_number: p.house_number.clone(),
            box_number: p.box_number.clone(),
            postal_code: p.postal_code.clone(),
            ..Default::default()
        };
        AddressQuery {
            alternatives: parsed.alternatives.iter().map(one).collect(),
            ..one(parsed)
        }
    }

    /// Apply `f` to this reading and every alternative (shared filters such
    /// as region, focus point or an explicit postal code).
    pub fn for_each_reading(&mut self, f: impl Fn(&mut AddressQuery)) {
        f(self);
        for alt in &mut self.alternatives {
            f(alt);
        }
    }
}

/// Confidence taxonomy emitted by the resolver, per design §3.3.
///
/// Ordered roughly from "best possible match" to "no match". The string form
/// (snake_case) is the stable wire shape used in CSV / JSON outputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confidence {
    /// Street, postal code, and house number all matched strict.
    Exact,
    /// Street matched but house number was approximated to the nearest
    /// number on the same side of the street (parity-respecting).
    /// Produced by `Tier::Nearest` (Phase 4).
    NearestSameSide,
    /// Street matched but house number was approximated to the nearest
    /// number on the other side of the street, because that side has no
    /// numbers in range. Produced by `Tier::Nearest` (Phase 4).
    NearestAnySide,
    /// Street matched but the house number was missing or not found.
    /// `resolve()` places the point at the street's median number within
    /// the postal code; `/geocode` returns buildings on the street.
    MedianStreet,
    /// Only the OR-of-tokens fallback matched the street name. When the
    /// house number was used (`house_number_delta == Some(0)`) it matched
    /// exactly; otherwise the result is street-level.
    FuzzyStreet,
    /// Nothing matched inside the given postal code: found around it
    /// instead, nearest first (v2 Phase 10). `house_number_delta` tells
    /// whether the number matched (`0`), was approximated, or was ignored
    /// (absent).
    AdjacentCommune,
    /// The query only names a locality (village, hamlet, municipality…):
    /// candidates are the BeSt addresses around its centre, nearest first,
    /// each at its own BeSt position (v2 Phase 6).
    Locality,
    /// The query only names a landmark ("Atomium", "Gare du Midi") from
    /// `--pois`: candidates are the BeSt addresses around it, nearest
    /// first, each at its own BeSt position (v2 Phase 10).
    Landmark,
    /// No match.
    NotFound,
}

impl Confidence {
    pub fn as_str(self) -> &'static str {
        match self {
            Confidence::Exact => "exact",
            Confidence::NearestSameSide => "nearest_same_side",
            Confidence::NearestAnySide => "nearest_any_side",
            Confidence::MedianStreet => "median_street",
            Confidence::FuzzyStreet => "fuzzy_street",
            Confidence::AdjacentCommune => "adjacent_commune",
            Confidence::Locality => "locality",
            Confidence::Landmark => "landmark",
            Confidence::NotFound => "not_found",
        }
    }
}

/// Why we fell back from `Confidence::Exact` to a lower-confidence outcome.
/// `None` when `confidence == Exact`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FallbackReason {
    /// Input did not contain a house number.
    HouseNumberMissing,
    /// Input had a house number but it isn't in BeSt for this street/postal.
    HouseNumberOutOfRange,
    /// Strict query returned zero hits; fuzzy fallback succeeded.
    StreetSpellingFuzzy,
    /// Found outside the given postal code (`Confidence::AdjacentCommune`).
    PostalCodeMismatch,
    /// No match at all.
    NoMatch,
    /// The query names a locality but no street (v2 Phase 6).
    StreetMissing,
}

impl FallbackReason {
    pub fn as_str(self) -> &'static str {
        match self {
            FallbackReason::HouseNumberMissing => "house_number_missing",
            FallbackReason::HouseNumberOutOfRange => "house_number_out_of_range",
            FallbackReason::StreetSpellingFuzzy => "street_spelling_fuzzy",
            FallbackReason::PostalCodeMismatch => "postal_code_mismatch",
            FallbackReason::NoMatch => "no_match",
            FallbackReason::StreetMissing => "street_missing",
        }
    }
}

/// One decision per input. Always populated — never `Option<ResolvedAddress>`.
/// On no-hit, `address == None` and `confidence == NotFound`; the caller
/// formats the empty row.
///
/// `Clone` because the batch dédup-cache propagates one resolved decision
/// across every input row sharing the same `(street, hn, postal)` triplet
/// without re-running Tantivy.
#[derive(Debug, Clone)]
pub struct ResolvedAddress {
    /// The resolved address as it lives in BeSt. `None` only when
    /// `confidence == NotFound`.
    pub address: Option<Address>,
    /// The hit metadata for downstream serialization (lat/lon, payload JSON,
    /// match_type, building_group_id). `None` only when `confidence == NotFound`.
    pub hit: Option<HydratedHit>,
    /// See `Confidence`.
    pub confidence: Confidence,
    /// `Some(0)` for an exact-number match. `Some(delta)` for approximated
    /// numbers (Lot C+). `None` for MedianStreet, FuzzyStreet with no number,
    /// or NotFound.
    pub house_number_delta: Option<i32>,
    /// Raw BM25 score from the strict pass, or 0.0 if the result came from
    /// fuzzy / fallback. The HTTP `/geocode` consumer already exposes this
    /// as `Feature::score`; the batch consumer bucketises via the threshold
    /// config (Lot G).
    pub street_match_score: f32,
    /// `None` iff `confidence == Exact`.
    pub fallback_reason: Option<FallbackReason>,
    /// The runner-up is a near-tie on another street
    /// (`CandidateSet::ambiguity`).
    pub ambiguous: bool,
    /// For `Confidence::Locality` / `Landmark`: the place the query was
    /// understood to name. `address` / `hit` are the BeSt address nearest
    /// to it, with their own BeSt position.
    pub locality: Option<Locality>,
    /// Which reading matched (`CandidateSet::reading`).
    pub reading: usize,
    /// Input vs. official address, when asked for
    /// (`validate::resolve_and_assess`, v3 Phase 11.2).
    pub validation: Option<Validation>,
}

impl ResolvedAddress {
    pub fn not_found() -> Self {
        Self {
            address: None,
            hit: None,
            confidence: Confidence::NotFound,
            house_number_delta: None,
            street_match_score: 0.0,
            fallback_reason: Some(FallbackReason::NoMatch),
            ambiguous: false,
            locality: None,
            reading: 0,
            validation: None,
        }
    }
}

/// Resources shared across `resolve()` calls. Cheap to construct (it's just
/// references + an `Arc`), so each call passes one in by value.
///
/// Thread-safety: every field is either `&T: Sync` (Tantivy is `Sync` by
/// construction) or `Arc<T>`. No mutable state, no locks. A pool of N
/// `resolve()` calls can run concurrently against the same context.
pub struct ResolverContext<'a> {
    pub index: &'a Index,
    pub reader: &'a IndexReader,
    pub schema: &'a CrabSchema,
    pub abbrev: Arc<AbbreviationMap>,
    pub addr_counts: SharedAddressCountIndex,
    pub notability_enabled: bool,
    /// Schema-v7 side-table for `Confidence::MedianStreet`. When the index
    /// was built by a pre-v7 indexer (no side-file produced), this is
    /// `Self::empty()` and the resolver still works — `MedianStreet` simply
    /// falls back to the top hit's own coordinates on the missing-number
    /// path.
    pub mid_street: Arc<MidStreetIndex>,
    /// Named places (v8 `localities-v1.json`) behind the locality step.
    /// Empty when the index has no localities file: the step never fires.
    pub localities: SharedLocalityIndex,
    /// OSM landmarks (`--pois`) behind the landmark step; empty when not
    /// loaded.
    pub landmarks: SharedLandmarkIndex,
}

/// Per-call knobs for `search`.
#[derive(Debug, Clone, Copy)]
pub struct SearchOptions {
    /// Maximum candidates returned (buildings, or rows when `expand`).
    pub limit: usize,
    /// One candidate per BeSt row instead of one per building.
    pub expand: bool,
    /// When false, the street-level tiers (`Tier::Street`,
    /// `Tier::OrStreet`) are skipped: a query whose house number can't be
    /// matched yields no candidates instead of a street approximation
    /// (`--no-mid-street` / `mid_street_fallback=false`).
    pub mid_street_fallback: bool,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            limit: 10,
            expand: false,
            mid_street_fallback: true,
        }
    }
}

/// The retrieval tiers, tried in this order; the first one with hits wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    /// Strict street match + house-number filter.
    Exact,
    /// Unknown street words corrected by edit distance + house-number
    /// filter (Phase 3).
    Typo,
    /// Strict street match, requested number absent: the closest existing
    /// number on the street, same side first (Phase 4).
    Nearest,
    /// Strict street match, house number ignored (missing or not found).
    Street,
    /// Typo-corrected street match, house number ignored.
    TypoStreet,
    /// OR-of-tokens street match + house-number filter.
    OrWithNumber,
    /// OR-of-tokens street match, house number ignored.
    OrStreet,
    /// The query names a locality: strict text match inside the area
    /// around its centre (v2 Phase 6; runs before every other tier).
    Locality,
    /// The query names a locality no text matches near its centre: the
    /// buildings nearest to it.
    Area,
    /// Nothing matched.
    None,
}

impl Tier {
    const ORDER: [Tier; 7] = [
        Tier::Exact,
        Tier::Typo,
        Tier::Nearest,
        Tier::Street,
        Tier::TypoStreet,
        Tier::OrWithNumber,
        Tier::OrStreet,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Tier::Exact => "exact",
            Tier::Typo => "typo",
            Tier::Nearest => "nearest",
            Tier::Street => "street",
            Tier::TypoStreet => "typo_street",
            Tier::OrWithNumber => "or_with_number",
            Tier::OrStreet => "or_street",
            Tier::Locality => "locality",
            Tier::Area => "area",
            Tier::None => "none",
        }
    }

    fn match_type(self) -> MatchType {
        match self {
            Tier::Exact | Tier::Nearest | Tier::Street | Tier::Locality | Tier::None => {
                MatchType::Strict
            }
            Tier::Typo | Tier::TypoStreet => MatchType::Typo,
            Tier::OrWithNumber | Tier::OrStreet => MatchType::Fuzzy,
            Tier::Area => MatchType::Spatial,
        }
    }

    /// Strict tiers (typo included) are tried for every reading before
    /// any OR fallback.
    fn is_strict(self) -> bool {
        matches!(
            self,
            Tier::Exact | Tier::Typo | Tier::Nearest | Tier::Street | Tier::TypoStreet
        )
    }

    /// Whether this tier honours the requested house number.
    fn uses_house_number(self) -> bool {
        matches!(
            self,
            Tier::Exact | Tier::Typo | Tier::Nearest | Tier::OrWithNumber
        )
    }

    /// `(confidence, fallback_reason, house_number_delta)` for a hit from
    /// this tier, given whether the query carried a house number.
    fn labels(self, has_hn: bool) -> (Confidence, Option<FallbackReason>, Option<i32>) {
        let missing_or = |r| {
            Some(if has_hn {
                r
            } else {
                FallbackReason::HouseNumberMissing
            })
        };
        match self {
            Tier::Exact => (Confidence::Exact, None, Some(0)),
            Tier::Typo => (
                Confidence::FuzzyStreet,
                Some(FallbackReason::StreetSpellingFuzzy),
                Some(0),
            ),
            Tier::TypoStreet => (
                Confidence::FuzzyStreet,
                missing_or(FallbackReason::StreetSpellingFuzzy),
                None,
            ),
            // Refined per candidate by `label_nearest`.
            Tier::Nearest => (
                Confidence::NearestSameSide,
                Some(FallbackReason::HouseNumberOutOfRange),
                None,
            ),
            Tier::Street => (
                Confidence::MedianStreet,
                missing_or(FallbackReason::HouseNumberOutOfRange),
                None,
            ),
            Tier::OrWithNumber => (
                Confidence::FuzzyStreet,
                Some(FallbackReason::StreetSpellingFuzzy),
                Some(0),
            ),
            Tier::OrStreet => (
                Confidence::FuzzyStreet,
                missing_or(FallbackReason::StreetSpellingFuzzy),
                None,
            ),
            Tier::Locality | Tier::Area => (
                Confidence::Locality,
                Some(FallbackReason::StreetMissing),
                None,
            ),
            Tier::None => (Confidence::NotFound, Some(FallbackReason::NoMatch), None),
        }
    }
}

/// One ranked candidate.
#[derive(Debug, Clone)]
pub struct Candidate {
    /// Building (or row) as stored; coordinates are the doc's own.
    pub hit: HydratedHit,
    pub confidence: Confidence,
    pub fallback_reason: Option<FallbackReason>,
    pub house_number_delta: Option<i32>,
}

/// Output of `search`.
#[derive(Debug, Clone)]
pub struct CandidateSet {
    /// Best first; at most `SearchOptions::limit`.
    pub candidates: Vec<Candidate>,
    /// Raw rows fetched by the winning tier, before dedup.
    pub raw_hits: usize,
    pub tier: Tier,
    /// The over-fetch hit `RAW_HIT_CAP`: some buildings may be missing
    /// from the tail.
    pub capped: bool,
    /// Which reading matched: 0 = the query itself, `n` = its
    /// `alternatives[n - 1]`.
    pub reading: usize,
    /// The locality the query names, centre included, when the locality
    /// step answered (`Tier::Locality` / `Tier::Area`).
    pub locality: Option<Locality>,
}

/// Score ratio from which two candidates on different streets make the
/// answer ambiguous (`CandidateSet::ambiguity`).
pub const AMBIGUITY_RATIO: f32 = 0.9;

impl CandidateSet {
    /// `(ambiguous, score_gap)` between the two best candidates.
    ///
    /// `score_gap = 1 − s₂/s₁`: 0 for a tie, 1 when the runner-up scores
    /// nothing. `ambiguous` when `s₂/s₁ ≥ AMBIGUITY_RATIO` and the two lie
    /// on different streets (different name or municipality: a street
    /// split into several BeSt objects is still one street). Only text
    /// tiers have comparable scores; Nearest, Locality and Area answers,
    /// and sets of fewer than two candidates, yield `(false, None)`.
    pub fn ambiguity(&self) -> (bool, Option<f32>) {
        let text_tier = matches!(
            self.tier,
            Tier::Exact
                | Tier::Typo
                | Tier::Street
                | Tier::TypoStreet
                | Tier::OrWithNumber
                | Tier::OrStreet
        );
        let [a, b, ..] = self.candidates.as_slice() else {
            return (false, None);
        };
        if !text_tier || a.hit.score <= 0.0 {
            return (false, None);
        }
        let ratio = (b.hit.score / a.hit.score).clamp(0.0, 1.0);
        let street = |c: &Candidate| {
            let p = &c.hit.payload;
            let name = |k: &str| {
                serde_json::from_value::<MultilingualName>(p[k].clone())
                    .ok()
                    .and_then(|n| n.primary().map(str::to_lowercase))
            };
            (name("street_name"), name("municipality_name"))
        };
        let same_street = street_key_from_bgid(&a.hit.building_group_id)
            == street_key_from_bgid(&b.hit.building_group_id)
            || street(a) == street(b);
        (ratio >= AMBIGUITY_RATIO && !same_street, Some(1.0 - ratio))
    }

    /// No candidates (`Tier::None`).
    pub fn empty() -> Self {
        Self {
            candidates: Vec::new(),
            raw_hits: 0,
            tier: Tier::None,
            capped: false,
            reading: 0,
            locality: None,
        }
    }
}

/// Ranked retrieval over the tiers in `Tier::ORDER`. Returns the
/// candidates of the first tier with hits, after focus decay (when
/// `query.focus_point` is set) and building dedup (unless `opts.expand`).
///
/// The strict tiers run for the query and then for each of its
/// `alternatives`; only when no reading matches strictly do the OR tiers
/// run, on the query itself.
pub fn search(
    query: &AddressQuery,
    opts: &SearchOptions,
    ctx: &ResolverContext<'_>,
) -> tantivy::Result<CandidateSet> {
    let set = search_readings(query, opts, ctx)?;
    crate::telemetry::record_search(&set);
    Ok(set)
}

/// `search` without the metrics: locality step, every reading's strict
/// tiers, locality-suffix readings, then the OR tiers.
fn search_readings(
    query: &AddressQuery,
    opts: &SearchOptions,
    ctx: &ResolverContext<'_>,
) -> tantivy::Result<CandidateSet> {
    if let Some((loc, words, precise)) = named_locality(query, ctx)
        && let Some(set) = search_locality(query, (&loc, words, precise), opts, ctx)?
    {
        return Ok(set);
    }
    let readings = std::iter::once(query).chain(query.alternatives.iter());
    for (reading, q) in readings.enumerate() {
        if let Some(set) = search_tiers(q, opts, ctx, Tier::is_strict)? {
            // An exact landmark name beats a typo-corrected street
            // ("Gravensteen" is not a misspelt Galerie Ravenstein).
            if matches!(set.tier, Tier::Typo | Tier::TypoStreet)
                && let Some(lm) = search_landmark(query, opts, ctx)?
            {
                return Ok(lm);
            }
            // The street exists in the postal code but not the number: the
            // exact number just across the postal boundary may be the
            // likelier address (`prefer_exact_nearby`).
            if matches!(set.tier, Tier::Street | Tier::Nearest)
                && q.house_number.is_some()
                && let Some(near) =
                    search_postal_relaxed(q, opts, ctx, |t| matches!(t, Tier::Exact | Tier::Typo))?
                && prefer_exact_nearby(&set, &near)
            {
                return Ok(CandidateSet { reading, ..near });
            }
            // Same for the exact number elsewhere in the postal code when
            // the trailing words only named one of its villages ("Chaussée
            // de Marche 797, 5100 Jambes": 797 is in 5100, not in the part
            // called Jambes) (v3 Phase 11.1b).
            if matches!(set.tier, Tier::Street | Tier::Nearest)
                && q.house_number.is_some()
                && let Some(r) = without_city_words(q, ctx)
                && let Some(exact) =
                    search_tiers(&r, opts, ctx, |t| matches!(t, Tier::Exact | Tier::Typo))?
                && prefer_exact_nearby(&set, &exact)
            {
                return Ok(CandidateSet { reading, ..exact });
            }
            // Another reading (the postal code read as a house number)
            // that doesn't match that number exactly is weaker evidence
            // than the street near the postal code.
            if reading > 0
                && !matches!(set.tier, Tier::Exact | Tier::Typo)
                && let Some(near) = search_postal_relaxed(query, opts, ctx, Tier::is_strict)?
            {
                return Ok(near);
            }
            return Ok(CandidateSet { reading, ..set });
        }
    }
    for r in locality_suffix_readings(query, ctx) {
        if let Some(set) = search_tiers(&r, opts, ctx, Tier::is_strict)? {
            return Ok(set);
        }
    }
    // v3 Phase 11.1b: the street without the village words, inside the
    // postal code ("Rue Rogier 44, 5000 Beez").
    if let Some(r) = without_city_words(query, ctx)
        && let Some(set) = search_tiers(&r, opts, ctx, Tier::is_strict)?
    {
        return Ok(set);
    }
    if let Some(set) = search_landmark(query, opts, ctx)? {
        return Ok(set);
    }
    if let Some(set) = search_postal_relaxed(query, opts, ctx, Tier::is_strict)? {
        return Ok(set);
    }
    // After the relaxation (v3 Phase 11.1b): the same street just across
    // the postal boundary beats a shortened name ("Rue de Namur 113, 1301"
    // is not Chaussée de Namur) or a correction inside a wrong postal code.
    // Without a leading street type BeSt doesn't have ("Rue Faubourg de
    // Charleroi 14B"); then with the words unknown in the postal code
    // corrected ("Schildknaapstraat 19, 1000").
    if let Some(r) = without_leading_type(query)
        && let Some(set) = search_tiers(&r, opts, ctx, Tier::is_strict)?
    {
        return Ok(set);
    }
    if let Some(set) = search_local_typo(query, opts, ctx)? {
        return Ok(set);
    }
    Ok(search_tiers(query, opts, ctx, |t| !t.is_strict())?.unwrap_or_else(CandidateSet::empty))
}

/// A nearest-number answer this far from the requested number loses to
/// the exact number on a namesake street across the postal boundary.
const NEAREST_TRUST_DELTA: i32 = 20;

/// Whether `exact` (the exact number found around the postal code) beats
/// `inside` (the street-level or nearest-number match inside it): always
/// for a street-level match; for a nearest number when the exact one is
/// on the same street object (a street crossing the boundary) or the
/// approximation is off by `NEAREST_TRUST_DELTA` or more.
fn prefer_exact_nearby(inside: &CandidateSet, exact: &CandidateSet) -> bool {
    let (Some(i), Some(e)) = (inside.candidates.first(), exact.candidates.first()) else {
        return false;
    };
    if inside.tier == Tier::Street {
        return true;
    }
    street_key_from_bgid(&i.hit.building_group_id) == street_key_from_bgid(&e.hit.building_group_id)
        || i.house_number_delta
            .is_none_or(|d| d.abs() >= NEAREST_TRUST_DELTA)
}

/// Radius around the postal code's centre searched when nothing matches
/// inside it, and the distance decay ranking those hits.
const POSTAL_RELAX_RADIUS_M: f64 = 20_000.0;
const POSTAL_RELAX_DECAY_M: f64 = 5_000.0;

/// The `tiers` of `query` without its postal code, inside
/// `POSTAL_RELAX_RADIUS_M` of the code's centre and nearest first,
/// labelled `AdjacentCommune` / `PostalCodeMismatch`. `None` when the query
/// has no (relaxable, known) postal code or nothing matches.
fn search_postal_relaxed(
    query: &AddressQuery,
    opts: &SearchOptions,
    ctx: &ResolverContext<'_>,
    tiers: impl Fn(Tier) -> bool,
) -> tantivy::Result<Option<CandidateSet>> {
    if query.postal_code_strict || query.area.is_some() {
        return Ok(None);
    }
    let Some((lat, lon)) = query
        .postal_code
        .as_deref()
        .and_then(|pc| ctx.localities.postal_centre(pc))
    else {
        return Ok(None);
    };
    let relaxed = AddressQuery {
        postal_code: None,
        area: Some(Area {
            lat,
            lon,
            radius_m: POSTAL_RELAX_RADIUS_M,
        }),
        focus_point: query.focus_point.or(Some((lat, lon))),
        focus_decay_m: if query.focus_point.is_some() {
            query.focus_decay_m
        } else {
            Some(POSTAL_RELAX_DECAY_M)
        },
        alternatives: Vec::new(),
        ..query.clone()
    };
    let Some(mut set) = search_tiers(&relaxed, opts, ctx, tiers)? else {
        return Ok(None);
    };
    for c in &mut set.candidates {
        c.confidence = Confidence::AdjacentCommune;
        c.fallback_reason = Some(FallbackReason::PostalCodeMismatch);
    }
    Ok(Some(set))
}

/// `query` without its trailing words when they name a locality of its
/// postal code (a village, part of municipality or municipality): they
/// then only say where in the code the street is, and BeSt may file it
/// under another part (v3 Phase 11.1b). `None` without a postal code or
/// such words.
fn without_city_words(query: &AddressQuery, ctx: &ResolverContext<'_>) -> Option<AddressQuery> {
    let pc = query.postal_code.as_ref()?;
    if query.area.is_some() {
        return None;
    }
    let toks = &query.street_tokens;
    (1..toks.len()).find_map(|k| {
        let (street, place) = toks.split_at(k);
        ctx.localities
            .lookup(place)
            .iter()
            .any(|l| l.postal_codes.contains(pc))
            .then(|| AddressQuery {
                street_tokens: street.to_vec(),
                alternatives: Vec::new(),
                ..query.clone()
            })
    })
}

/// French street types a user may put in front of a name BeSt files
/// without one ("Rue Faubourg de Charleroi" is "Faubourg de Charleroi").
const LEADING_TYPES: &[&str] = &[
    "rue",
    "avenue",
    "chaussee",
    "place",
    "boulevard",
    "chemin",
    "route",
    "allee",
    "impasse",
    "quai",
    "square",
    "clos",
    "dreve",
    "sentier",
];

/// `query` without a leading street type, when two words or more remain.
fn without_leading_type(query: &AddressQuery) -> Option<AddressQuery> {
    let toks = &query.street_tokens;
    (toks.len() >= 3 && LEADING_TYPES.contains(&toks[0].as_str())).then(|| AddressQuery {
        street_tokens: toks[1..].to_vec(),
        alternatives: Vec::new(),
        ..query.clone()
    })
}

/// The typo tiers with "unknown" meant inside the postal code: a word
/// found elsewhere in Belgium but nowhere in the code is corrected too
/// ("Schildknaapstraat 19, 1000" is BeSt's Schildknaapsstraat; Koksijde's
/// Schildknaapstraat hides it from the regular typo tier) (v3 Phase
/// 11.1b). `None` without a postal code or such a word.
fn search_local_typo(
    query: &AddressQuery,
    opts: &SearchOptions,
    ctx: &ResolverContext<'_>,
) -> tantivy::Result<Option<CandidateSet>> {
    let Some(pc) = query.postal_code.as_deref() else {
        return Ok(None);
    };
    if query.area.is_some() {
        return Ok(None);
    }
    let field = searchable_field(ctx.schema);
    let searcher = ctx.reader.searcher();
    let term_q = |field, text: &str| -> Box<dyn tantivy::query::Query> {
        Box::new(TermQuery::new(
            Term::from_field_text(field, text),
            IndexRecordOption::Basic,
        ))
    };
    let mut mask = Vec::with_capacity(query.street_tokens.len());
    for t in &query.street_tokens {
        let any_form = ctx
            .abbrev
            .expand(t)
            .iter()
            .map(|f| (Occur::Should, term_q(field, f)))
            .collect();
        let q = BooleanQuery::new(vec![
            (Occur::Must, Box::new(BooleanQuery::new(any_form))),
            (Occur::Must, term_q(ctx.schema.postal_code, pc)),
        ]);
        mask.push(searcher.search(&q, &tantivy::collector::Count)? == 0);
    }
    if !mask.iter().any(|u| *u) {
        return Ok(None);
    }
    search_tiers_masked(
        query,
        opts,
        ctx,
        |t| matches!(t, Tier::Typo | Tier::TypoStreet),
        Some(mask),
    )
}

/// Run the `tiers` (of `Tier::ORDER`) of one reading, in order.
fn search_tiers(
    query: &AddressQuery,
    opts: &SearchOptions,
    ctx: &ResolverContext<'_>,
    tiers: impl Fn(Tier) -> bool,
) -> tantivy::Result<Option<CandidateSet>> {
    search_tiers_masked(query, opts, ctx, tiers, None)
}

/// `search_tiers`, the typo tiers correcting the slots `typo_mask` marks
/// (default: the words unknown to the whole index).
fn search_tiers_masked(
    query: &AddressQuery,
    opts: &SearchOptions,
    ctx: &ResolverContext<'_>,
    tiers: impl Fn(Tier) -> bool,
    typo_mask: Option<Vec<bool>>,
) -> tantivy::Result<Option<CandidateSet>> {
    let slots: Vec<Vec<String>> = query
        .street_tokens
        .iter()
        .map(|t| ctx.abbrev.expand(t))
        .collect();
    let has_hn = query.house_number.is_some();
    let filters = FilterParams {
        region: query.region_filter.clone(),
        postal_code: query.postal_code.clone(),
        area: query.area,
        bbox: query.bbox,
    };
    let parsed = |with_hn: bool| ParsedQuery {
        raw_query: String::new(),
        street_tokens: query.street_tokens.clone(),
        house_number: if with_hn {
            query.house_number.clone()
        } else {
            None
        },
        postal_code: None,
        ..Default::default()
    };
    let field = searchable_field(ctx.schema);
    let searcher = ctx.reader.searcher();
    // An area bounds the hits, and distance re-ranks them: fetch them all
    // (up to the cap) so the ranking doesn't depend on `limit`.
    let raw_limit = if query.area.is_some() {
        RAW_HIT_CAP
    } else {
        raw_limit(opts.limit, opts.expand)
    };
    // Which slots the typo tiers make fuzzy; computed on first use.
    let mut unknown: Option<Vec<bool>> = typo_mask;
    // The Nearest tier's street hits, reused by the Street tier.
    let mut street_top: Option<Vec<(tantivy::Score, tantivy::DocAddress)>> = None;
    let run = |q: &dyn tantivy::query::Query| {
        if ctx.notability_enabled {
            search_with_notability(&searcher, q, raw_limit, ctx.addr_counts.clone())
        } else {
            search_score_only(&searcher, q, raw_limit)
        }
    };

    for tier in Tier::ORDER.into_iter().filter(|t| tiers(*t)) {
        let enabled = match tier {
            // Nearest is not a street approximation: it stays on with
            // mid-street off (design-batch-geocoding §236).
            Tier::Exact | Tier::Typo | Tier::Nearest | Tier::OrWithNumber => has_hn,
            Tier::Street | Tier::TypoStreet | Tier::OrStreet => opts.mid_street_fallback,
            // Not in `Tier::ORDER`: `search_locality` runs them.
            Tier::Locality | Tier::Area | Tier::None => false,
        };
        if !enabled {
            continue;
        }
        if tier == Tier::Nearest {
            let p = parsed(false);
            let Some(q) = build_strict_query(field, &slots, &p, ctx.schema, &filters) else {
                continue;
            };
            let top = run(q.as_ref())?;
            let found = nearest_units(&searcher, q.as_ref(), &top, query, ctx)?;
            street_top = Some(top);
            if let Some(units) = found {
                let mut set =
                    collect_candidates(&searcher, &units, tier, has_hn, query, opts, ctx, None)?;
                label_nearest(&mut set, query);
                return Ok(Some(set));
            }
            continue;
        }
        if tier == Tier::Street
            && let Some(top) = street_top.take()
        {
            if top.is_empty() {
                continue;
            }
            return collect_candidates(
                &searcher,
                &top,
                tier,
                has_hn,
                query,
                opts,
                ctx,
                user_focus(query),
            )
            .map(Some);
        }
        let p = parsed(tier.uses_house_number());
        let q = match tier {
            Tier::Exact | Tier::Street => {
                build_strict_query(field, &slots, &p, ctx.schema, &filters)
            }
            Tier::Typo | Tier::TypoStreet => {
                let unknown = unknown.get_or_insert_with(|| {
                    slots
                        .iter()
                        .map(|forms| slot_is_unknown(&searcher, field, forms))
                        .collect()
                });
                build_typo_query(field, &slots, unknown, &p, ctx.schema, &filters)
            }
            _ => build_fuzzy_query(field, &slots, &p, ctx.schema, &filters),
        };
        let Some(q) = q else { continue };
        let top = run(q.as_ref())?;
        if top.is_empty() {
            continue;
        }
        return collect_candidates(
            &searcher,
            &top,
            tier,
            has_hn,
            query,
            opts,
            ctx,
            user_focus(query),
        )
        .map(Some);
    }
    Ok(None)
}

/// The query's `focus_point` with its decay length.
fn user_focus(query: &AddressQuery) -> Option<((f64, f64), f64)> {
    query
        .focus_point
        .map(|p| (p, query.focus_decay_m.unwrap_or(FOCUS_HALF_LIFE_M)))
}

/// Materialise a tier's raw hits into ranked, labelled candidates.
/// `focus` is `(point, decay length in metres)`.
#[allow(clippy::too_many_arguments)]
fn collect_candidates(
    searcher: &tantivy::Searcher,
    top: &[(tantivy::Score, tantivy::DocAddress)],
    tier: Tier,
    has_hn: bool,
    query: &AddressQuery,
    opts: &SearchOptions,
    ctx: &ResolverContext<'_>,
    focus: Option<((f64, f64), f64)>,
) -> tantivy::Result<CandidateSet> {
    let mut docs: Vec<TantivyDocument> = Vec::with_capacity(top.len());
    let mut raw_hits: Vec<RawHit> = Vec::with_capacity(top.len());
    for (i, (score, addr)) in top.iter().enumerate() {
        let doc: TantivyDocument = searcher.doc(*addr)?;
        raw_hits.push(RawHit::from_doc(
            &doc,
            ctx.schema,
            *score,
            tier.match_type(),
            i,
        ));
        docs.push(doc);
    }
    // Nearest hits are already in distance order; box and focus would
    // only scramble it (the Nearest caller passes no focus).
    if tier != Tier::Nearest
        && let Some(b) = query.box_number.as_deref()
    {
        prefer_box(&mut raw_hits, b);
    }
    if let Some((point, scale)) = focus {
        apply_decay(&mut raw_hits, point, scale, ctx.schema, &docs);
    }

    let (confidence, fallback_reason, house_number_delta) = tier.labels(has_hn);
    let candidate = |idx: usize, score, unit_count, bgid| -> tantivy::Result<Candidate> {
        let hit = hydrate(
            &docs[idx],
            ctx.schema,
            score,
            tier.match_type(),
            unit_count,
            bgid,
            None,
        )
        .map_err(|e| tantivy::TantivyError::InternalError(e.to_string()))?;
        Ok(Candidate {
            hit,
            confidence,
            fallback_reason,
            house_number_delta,
        })
    };
    let candidates = if opts.expand {
        raw_hits
            .iter()
            .take(opts.limit)
            .map(|h| candidate(h.raw_index, h.score, 1, h.building_group_id.clone()))
            .collect::<tantivy::Result<Vec<_>>>()?
    } else {
        group_by_building(&raw_hits)
            .into_iter()
            .take(opts.limit)
            .map(|g| {
                candidate(
                    g.representative_index,
                    g.max_score,
                    g.unit_count,
                    g.building_group_id,
                )
            })
            .collect::<tantivy::Result<Vec<_>>>()?
    };
    Ok(CandidateSet {
        candidates,
        raw_hits: raw_hits.len(),
        tier,
        capped: !opts.expand && raw_hits.len() >= RAW_HIT_CAP,
        reading: 0,
        locality: None,
    })
}

// --- Locality step (v2 Phase 6) ------------------------------------------

/// Radius searched around a locality's centre, by kind.
fn locality_radius_m(kind: LocalityKind) -> f64 {
    match kind {
        LocalityKind::Municipality => 5_000.0,
        LocalityKind::PostalArea => 4_000.0,
        LocalityKind::PartOfMunicipality => 3_000.0,
        LocalityKind::Village => 2_000.0,
        LocalityKind::Hamlet => 1_500.0,
        LocalityKind::Locality => 1_000.0,
        LocalityKind::IsolatedDwelling => 800.0,
        LocalityKind::Landmark => LANDMARK_RADIUS_M,
    }
}

/// Two localities this close, or sharing a postal code, are taken to be
/// the same place or one inside the other.
const SAME_PLACE_M: f64 = 15_000.0;

/// An OSM node this close to a BeSt locality of the same name is its
/// centre (the village square rather than the address centroid).
const OSM_CENTRE_M: f64 = 10_000.0;

fn distance_m(a: &Locality, b: &Locality) -> f64 {
    haversine_m(a.lat, a.lon, b.lat, b.lon)
}

/// The locality a house-number-less query names, if any:
/// - the whole street text is a locality name (`"chassepierre"`,
///   `"han sur lesse 5580"`), or
/// - it is a locality followed by the locality or province containing it
///   (`"spontin yvoir"`, `"celles houyet"`, `"beausaint la roche"` for La
///   Roche-en-Ardenne, `"ave namur"` for the province): the first, more
///   precise one.
///
/// A postal code in the query must be one of the locality's. Among
/// localities sharing the name, `LocalityIndex::lookup` order decides
/// (administrative first); the centre is taken from a same-name OSM node
/// within `OSM_CENTRE_M` when there is one. Returns the locality, how
/// many leading street tokens name it, and whether its centre is precise
/// (`precise_centre`).
fn named_locality(
    query: &AddressQuery,
    ctx: &ResolverContext<'_>,
) -> Option<(Locality, usize, bool)> {
    if query.house_number.is_some() || query.street_tokens.is_empty() {
        return None;
    }
    let pc_ok = |l: &&Locality| {
        query
            .postal_code
            .as_ref()
            .is_none_or(|pc| l.postal_codes.contains(pc))
    };
    let named = |tokens: &[String]| -> Vec<&Locality> {
        ctx.localities.lookup(tokens).iter().filter(pc_ok).collect()
    };
    let toks = &query.street_tokens;
    let whole = named(toks);
    if let Some(first) = whole.first() {
        let l = with_centre(first, &whole);
        let precise = precise_centre(&l, &whole);
        return Some((l, toks.len(), precise));
    }
    for k in (1..toks.len()).rev() {
        let (inner, outer) = toks.split_at(k);
        let inner = named(inner);
        if inner.is_empty() {
            continue;
        }
        let province = Province::named(outer);
        let mut outer_named: Vec<&Locality> = ctx.localities.lookup(outer).iter().collect();
        outer_named.extend(ctx.localities.lookup_prefix(outer));
        let found = inner.iter().find(|i| {
            province.is_some_and(|p| p.contains_any(&i.postal_codes))
                || outer_named.iter().any(|o| {
                    o.postal_codes.iter().any(|pc| i.postal_codes.contains(pc))
                        || distance_m(i, o) < SAME_PLACE_M
                })
        });
        if let Some(i) = found {
            let l = with_centre(i, &inner);
            let precise = precise_centre(&l, &inner);
            return Some((l, k, precise));
        }
    }
    None
}

/// Smallest area searched around a locality named after the street
/// (`locality_suffix_readings`): a hamlet's streets reach beyond its node.
const SUFFIX_MIN_RADIUS_M: f64 = 3_000.0;

/// Most same-name localities tried for one suffix ("Celles" names three).
const SUFFIX_MAX_PLACES: usize = 3;

/// Readings for "`<street words> <locality>`" whose strict tiers failed
/// because the locality isn't in the street's searchable text: an
/// OSM-only hamlet ("Rue du Moulin Frahan"), or a municipality the street
/// doesn't belong to ("Rue de la Loi 17, 1040 Etterbeek"). Each keeps the
/// street words, number and postal code, and searches inside the area of
/// one locality named by the trailing words (longest suffix first, then
/// `lookup` order), focused on its centre.
fn locality_suffix_readings(query: &AddressQuery, ctx: &ResolverContext<'_>) -> Vec<AddressQuery> {
    let toks = &query.street_tokens;
    let mut out = Vec::new();
    for k in 1..toks.len() {
        let (street, place) = toks.split_at(k);
        let places = ctx.localities.lookup(place).iter().filter(|l| {
            query
                .postal_code
                .as_ref()
                .is_none_or(|pc| l.postal_codes.contains(pc))
        });
        for l in places.take(SUFFIX_MAX_PLACES) {
            out.push(AddressQuery {
                street_tokens: street.to_vec(),
                focus_point: query.focus_point.or(Some((l.lat, l.lon))),
                area: Some(Area {
                    lat: l.lat,
                    lon: l.lon,
                    radius_m: locality_radius_m(l.kind).max(SUFFIX_MIN_RADIUS_M),
                }),
                alternatives: Vec::new(),
                ..query.clone()
            });
        }
        if !out.is_empty() {
            break;
        }
    }
    out
}

/// `l`, centred on a same-name OSM node nearby when `l` is administrative.
fn with_centre(l: &Locality, same_name: &[&Locality]) -> Locality {
    let mut out = l.clone();
    if !l.kind.is_osm()
        && let Some(o) = same_name
            .iter()
            .filter(|o| o.kind.is_osm())
            .min_by(|a, b| distance_m(l, a).total_cmp(&distance_m(l, b)))
            .filter(|o| distance_m(l, o) < OSM_CENTRE_M)
    {
        out.lat = o.lat;
        out.lon = o.lon;
    }
    out
}

/// Whether `l`'s centre is a mapped point (an OSM node, possibly lent by
/// `with_centre`) rather than an address centroid.
fn precise_centre(l: &Locality, same_name: &[&Locality]) -> bool {
    l.kind.is_osm()
        || same_name
            .iter()
            .any(|o| o.kind.is_osm() && o.lat == l.lat && o.lon == l.lon)
}

/// Answer a locality query.
///
/// With a precise centre (an OSM node: the village square), the buildings
/// nearest to it (`Tier::Area`, rings of growing radius): the name adds
/// nothing, and matching it as text would pull in namesake streets nearby
/// ("Route de Dolhain" in Baelen for Dolhain).
///
/// With an address centroid (municipalities, parts of municipality OSM
/// doesn't map), the buildings carrying the locality's name — the first
/// `words` street tokens; the container words are enforced by the area —
/// inside the area, nearest first (`Tier::Locality`), else the nearest
/// buildings.
///
/// `None` when nothing lies in the area, so the regular tiers run.
fn search_locality(
    query: &AddressQuery,
    (loc, words, precise): (&Locality, usize, bool),
    opts: &SearchOptions,
    ctx: &ResolverContext<'_>,
) -> tantivy::Result<Option<CandidateSet>> {
    let radius = locality_radius_m(loc.kind);
    let centre = (loc.lat, loc.lon);
    let filters = |radius_m| FilterParams {
        region: query.region_filter.clone(),
        postal_code: query.postal_code.clone(),
        area: Some(Area {
            lat: loc.lat,
            lon: loc.lon,
            radius_m,
        }),
        bbox: query.bbox,
    };
    let searcher = ctx.reader.searcher();
    // Area-bounded and re-ranked by distance: see `search_tiers`.
    let raw_limit = RAW_HIT_CAP;
    let words = &query.street_tokens[..words];
    let slots: Vec<Vec<String>> = words.iter().map(|t| ctx.abbrev.expand(t)).collect();
    let parsed = ParsedQuery {
        street_tokens: words.to_vec(),
        ..Default::default()
    };
    let focus = Some((centre, LOCALITY_DECAY_M));
    let finish = |top: &[(tantivy::Score, tantivy::DocAddress)], tier| {
        collect_candidates(&searcher, top, tier, false, query, opts, ctx, focus).map(|set| {
            Some(CandidateSet {
                locality: Some(loc.clone()),
                ..set
            })
        })
    };

    let field = searchable_field(ctx.schema);
    if !precise
        && let Some(q) = build_strict_query(field, &slots, &parsed, ctx.schema, &filters(radius))
    {
        let top = if ctx.notability_enabled {
            search_with_notability(&searcher, q.as_ref(), raw_limit, ctx.addr_counts.clone())?
        } else {
            search_score_only(&searcher, q.as_ref(), raw_limit)?
        };
        if !top.is_empty() {
            return finish(&top, Tier::Locality);
        }
    }
    for r in [radius / 8.0, radius / 2.0, radius] {
        let Some(q) = build_area_query(ctx.schema, &filters(r)) else {
            continue;
        };
        let top = search_score_only(&searcher, q.as_ref(), raw_limit)?;
        if !top.is_empty() {
            return finish(&top, Tier::Area);
        }
    }
    Ok(None)
}

// --- Landmark step (v2 Phase 10) -----------------------------------------

/// Buildings around a landmark are searched within this radius (in rings,
/// as for a locality with a precise centre).
const LANDMARK_RADIUS_M: f64 = 600.0;

/// A landmark must lie this close to the query's postal code centre, or
/// to the locality named after it ("Gravensteen Gent").
const LANDMARK_NEAR_M: f64 = 15_000.0;

/// The landmark a house-number-less query names, if any: the whole street
/// text is a landmark name (`"atomium"`, `"bruxelles midi"`), or a
/// landmark name followed by a locality it lies near (`"gravensteen
/// gent"`). A postal code in the query must have its centre within
/// `LANDMARK_NEAR_M`; with a `focus_point` the nearest same-name landmark
/// wins, else `LandmarkIndex::lookup` order (sights before churches).
fn named_landmark(query: &AddressQuery, ctx: &ResolverContext<'_>) -> Option<Landmark> {
    if query.house_number.is_some() || query.street_tokens.is_empty() || ctx.landmarks.is_empty() {
        return None;
    }
    let postal_centre = query
        .postal_code
        .as_deref()
        .map(|pc| ctx.localities.postal_centre(pc));
    let ok = |l: &&Landmark| {
        let near = |(lat, lon): (f64, f64)| haversine_m(lat, lon, l.lat, l.lon) < LANDMARK_NEAR_M;
        postal_centre.is_none_or(|c| c.is_some_and(near))
            && query.bbox.is_none_or(|b| {
                (b.min_lat..=b.max_lat).contains(&l.lat) && (b.min_lon..=b.max_lon).contains(&l.lon)
            })
    };
    let pick = |hits: Vec<&Landmark>| -> Option<Landmark> {
        let best = match query.focus_point {
            Some((lat, lon)) => hits.into_iter().min_by(|a, b| {
                haversine_m(lat, lon, a.lat, a.lon).total_cmp(&haversine_m(lat, lon, b.lat, b.lon))
            }),
            None => hits.into_iter().next(),
        };
        best.cloned()
    };
    let toks = &query.street_tokens;
    let whole: Vec<&Landmark> = ctx.landmarks.lookup(toks).iter().filter(ok).collect();
    if !whole.is_empty() {
        return pick(whole);
    }
    for k in (1..toks.len()).rev() {
        let (name, place) = toks.split_at(k);
        let places = ctx.localities.lookup(place);
        let near: Vec<&Landmark> = ctx
            .landmarks
            .lookup(name)
            .iter()
            .filter(ok)
            .filter(|l| {
                places
                    .iter()
                    .any(|p| haversine_m(p.lat, p.lon, l.lat, l.lon) < LANDMARK_NEAR_M)
            })
            .collect();
        if !near.is_empty() {
            return pick(near);
        }
    }
    None
}

/// Answer a landmark query (`named_landmark`) with the buildings nearest
/// to it, labelled `Confidence::Landmark` / `StreetMissing`. Runs after
/// every strict reading, so a street of the same name ("Avenue de
/// l'Atomium") still wins — unless only typo correction found it — and
/// before the postal relaxation and the OR tiers, which would match stray
/// words ("Gare" in every Rue de la Gare).
/// The postal code only vouches for the landmark: buildings across the
/// boundary from it count.
fn search_landmark(
    query: &AddressQuery,
    opts: &SearchOptions,
    ctx: &ResolverContext<'_>,
) -> tantivy::Result<Option<CandidateSet>> {
    let Some(lm) = named_landmark(query, ctx) else {
        return Ok(None);
    };
    let loc = Locality {
        kind: LocalityKind::Landmark,
        name: lm.name,
        lat: lm.lat,
        lon: lm.lon,
        postal_codes: Vec::new(),
        address_count: 0,
        region: None,
    };
    let unfiltered = AddressQuery {
        postal_code: None,
        ..query.clone()
    };
    let Some(mut set) = search_locality(&unfiltered, (&loc, 0, true), opts, ctx)? else {
        return Ok(None);
    };
    for c in &mut set.candidates {
        c.confidence = Confidence::Landmark;
    }
    Ok(Some(set))
}

/// Most units the Nearest tier reads for one street (Belgian streets rarely
/// exceed a few hundred).
const NEAREST_MAX_UNITS: usize = 5000;

/// Farthest neighbour the Nearest tier accepts: beyond it the street-level
/// median is the better guess (PhacochR-style cut-off).
fn nearest_limit(requested: u32) -> u32 {
    50.max(requested / 2)
}

/// Units of the top street hit's street whose number is within
/// `nearest_limit` of the requested one, ranked same side first, then by
/// distance, then lower number, as `(score, doc)` pairs with a dense
/// descending score so units of one building tie. `None` when there are
/// no street hits, the request has no leading number, or nothing is in
/// range.
///
/// "The street" is every street object in the top hit's postal code that
/// matches `street_q` *and* carries the top hit's name: a Belgian street
/// is often split into several objects (Avenue Louise belongs to both
/// Brussels and Ixelles), and its two sides can sit in different ones.
fn nearest_units(
    searcher: &tantivy::Searcher,
    street_q: &dyn tantivy::query::Query,
    street_top: &[(tantivy::Score, tantivy::DocAddress)],
    query: &AddressQuery,
    ctx: &ResolverContext<'_>,
) -> tantivy::Result<Option<Vec<(tantivy::Score, tantivy::DocAddress)>>> {
    let Some(requested) = query.house_number.as_deref().and_then(leading_number) else {
        return Ok(None);
    };
    let Some((_, top_addr)) = street_top.first() else {
        return Ok(None);
    };
    let top_doc: TantivyDocument = searcher.doc(*top_addr)?;
    let text = |doc: &TantivyDocument, f| {
        doc.get_first(f)
            .and_then(|v| v.as_str())
            .map(str::to_string)
    };
    let Some(postal) = text(&top_doc, ctx.schema.postal_code) else {
        return Ok(None);
    };
    let street_name = |doc: &TantivyDocument| -> Option<MultilingualName> {
        let payload = text(doc, ctx.schema.payload)?;
        let v: serde_json::Value = serde_json::from_str(&payload).ok()?;
        serde_json::from_value(v.get("street_name")?.clone()).ok()
    };
    let Some(top_name) = street_name(&top_doc) else {
        return Ok(None);
    };
    let units_q = BooleanQuery::new(vec![
        (Occur::Must, street_q.box_clone()),
        (
            Occur::Must,
            Box::new(TermQuery::new(
                Term::from_field_text(ctx.schema.postal_code, &postal),
                IndexRecordOption::Basic,
            )),
        ),
    ]);
    let units = search_score_only(searcher, &units_q, NEAREST_MAX_UNITS)?;

    let limit = nearest_limit(requested);
    // Street key → shares the top hit's name (decoded once per street).
    let mut same_street: HashMap<String, bool> = HashMap::new();
    // (other_side, |delta|, number, house_number) → rank key.
    let mut ranked: Vec<((bool, u32, u32, String), tantivy::DocAddress)> = Vec::new();
    for (_, addr) in units {
        let doc: TantivyDocument = searcher.doc(addr)?;
        let (Some(hn), Some(bgid)) = (
            text(&doc, ctx.schema.house_number),
            text(&doc, ctx.schema.building_group_id),
        ) else {
            continue;
        };
        let Some(n) = leading_number(&hn) else {
            continue;
        };
        let dist = n.abs_diff(requested);
        if dist > limit {
            continue;
        }
        let key = street_key_from_bgid(&bgid).unwrap_or(&bgid).to_string();
        let same = *same_street
            .entry(key)
            .or_insert_with(|| street_name(&doc).is_some_and(|n| same_name(&n, &top_name)));
        if same {
            ranked.push(((n % 2 != requested % 2, dist, n, hn.to_uppercase()), addr));
        }
    }
    if ranked.is_empty() {
        return Ok(None);
    }
    ranked.sort_by(|a, b| a.0.cmp(&b.0));
    // Dense rank → descending score; equal keys (same building) tie.
    let mut out = Vec::with_capacity(ranked.len());
    let mut rank = 0usize;
    for (i, (key, addr)) in ranked.iter().enumerate() {
        if i > 0 && ranked[i - 1].0 != *key {
            rank += 1;
        }
        out.push(((ranked.len() - rank) as tantivy::Score, *addr));
    }
    Ok(Some(out))
}

/// Two street names are the same street when any language form matches
/// (case-insensitively).
fn same_name(a: &MultilingualName, b: &MultilingualName) -> bool {
    [(&a.fr, &b.fr), (&a.nl, &b.nl), (&a.de, &b.de)]
        .into_iter()
        .any(|(x, y)| matches!((x, y), (Some(x), Some(y)) if x.to_lowercase() == y.to_lowercase()))
}

/// Per-candidate labels for the Nearest tier: same or other side of the
/// street, and `matched − requested`.
fn label_nearest(set: &mut CandidateSet, query: &AddressQuery) {
    let Some(requested) = query.house_number.as_deref().and_then(leading_number) else {
        return;
    };
    for c in &mut set.candidates {
        let Some(n) = c.hit.payload["house_number"]
            .as_str()
            .and_then(leading_number)
        else {
            continue;
        };
        c.confidence = if n % 2 == requested % 2 {
            Confidence::NearestSameSide
        } else {
            Confidence::NearestAnySide
        };
        c.house_number_delta =
            Some(i64::from(n) - i64::from(requested)).and_then(|d| i32::try_from(d).ok());
    }
}

/// Box numbers compare case-insensitively and ignore whitespace and
/// leading zeros (`"03"` = `"3"`, `"b  2"` = `"B2"`).
pub(crate) fn normalize_box(b: &str) -> String {
    let b: String = b
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_lowercase();
    let stripped = b.trim_start_matches('0');
    if stripped.is_empty() && !b.is_empty() {
        "0".into()
    } else {
        stripped.to_string()
    }
}

/// Nudge hits whose box matches `wanted` just above their equally-scored
/// siblings, so the requested unit represents its building. The nudge is
/// relative (×1.0001) so it only breaks ties.
fn prefer_box(hits: &mut [RawHit], wanted: &str) {
    let wanted = normalize_box(wanted);
    let mut bumped = false;
    for h in hits.iter_mut() {
        if h.box_number.as_deref().map(normalize_box).as_deref() == Some(wanted.as_str()) {
            h.score = h.score * 1.0001 + f32::EPSILON;
            bumped = true;
        }
    }
    if bumped {
        hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    }
}

/// Resolve a single address with default options. Always returns a
/// `ResolvedAddress` — Tantivy IO failures are logged and reported as
/// `NotFound` rather than bubbled per row.
pub fn resolve(query: &AddressQuery, ctx: &ResolverContext<'_>) -> ResolvedAddress {
    resolve_with(query, &SearchOptions::default(), ctx)
}

/// `resolve` with explicit options (`limit` / `expand` are ignored: two
/// buildings are fetched, the runner-up only to judge ambiguity).
///
/// Street-level outcomes (`MedianStreet`, and `FuzzyStreet` when the house
/// number wasn't used) get the street's median coordinates from the
/// mid-street side-table when it has them, so the point is representative
/// of the street rather than of one arbitrary building on it.
pub fn resolve_with(
    query: &AddressQuery,
    opts: &SearchOptions,
    ctx: &ResolverContext<'_>,
) -> ResolvedAddress {
    let opts = SearchOptions {
        limit: 2,
        expand: false,
        ..*opts
    };
    let set = match search(query, &opts, ctx) {
        Ok(s) => s,
        Err(e) => {
            warn!(error=%e, "resolver: tantivy search failed; treating as no-hit");
            return ResolvedAddress::not_found();
        }
    };
    let (ambiguous, _) = set.ambiguity();
    let Some(c) = set.candidates.into_iter().next() else {
        return ResolvedAddress::not_found();
    };
    let Ok(address) = serde_json::from_value::<Address>(c.hit.payload.clone()) else {
        warn!(bgid = %c.hit.building_group_id, "resolver: payload failed serde decode");
        return ResolvedAddress::not_found();
    };
    let mut hit = c.hit;
    // A locality or landmark answer is the BeSt address nearest to it, at
    // its own BeSt position: BeSt is the only source of what is returned.
    if set.locality.is_none()
        && !set.tier.uses_house_number()
        && let Some((lat, lon)) = street_key_from_bgid(&hit.building_group_id)
            .and_then(|k| ctx.mid_street.lookup(k, &address.postal_code))
    {
        hit.lat = lat;
        hit.lon = lon;
    }
    ResolvedAddress {
        street_match_score: hit.score,
        address: Some(address),
        hit: Some(hit),
        confidence: c.confidence,
        house_number_delta: c.house_number_delta,
        fallback_reason: c.fallback_reason,
        ambiguous,
        locality: set.locality,
        reading: set.reading,
        validation: None,
    }
}

/// Helper for batch callers: take a raw user-provided street string and
/// produce the analyzed token vec that goes into
/// `AddressQuery::street_tokens`.
pub fn analyze_street_input(index: &Index, raw: &str) -> Vec<String> {
    analyze(index, raw)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::abbreviation::AbbreviationMap;
    use crate::addr_count::AddressCountIndex;
    use crate::mid_street::MidStreetIndex;
    use crabfinder_test_support::{AddressBuilder, TestIndex, fixtures, write_mid_street};

    /// Avenue Louise 10/11/13/20/32/40/45/50 in 1050 + Rue Paire 2/4/6.
    fn louise_index() -> TestIndex {
        TestIndex::build(&fixtures::louise_street())
    }

    fn ctx(ti: &TestIndex, mid_street: Arc<MidStreetIndex>) -> ResolverContext<'_> {
        ResolverContext {
            index: &ti.index,
            reader: &ti.reader,
            schema: &ti.schema,
            abbrev: Arc::new(AbbreviationMap::empty()),
            addr_counts: Arc::new(AddressCountIndex::build(&ti.reader, &ti.schema).unwrap()),
            notability_enabled: true,
            mid_street,
            localities: Arc::new(crate::localities::LocalityIndex::empty()),
            landmarks: Arc::new(crate::landmarks::LandmarkIndex::empty()),
        }
    }

    fn query(ti: &TestIndex, street: &str, hn: Option<&str>, pc: Option<&str>) -> AddressQuery {
        AddressQuery {
            street_tokens: analyze_street_input(&ti.index, street),
            house_number: hn.map(Into::into),
            postal_code: pc.map(Into::into),
            ..Default::default()
        }
    }

    /// A mid-street table whose only entry pins Avenue Louise / 1050 to a
    /// sentinel coordinate, so tests can tell side-table coords apart from
    /// a building's own.
    fn sentinel_mid_street(lat: f64, lon: f64) -> (tempfile::TempDir, Arc<MidStreetIndex>) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mid.bin");
        let sentinel = AddressBuilder::new("s", fixtures::LOUISE_OID, "1")
            .postal("1050")
            .at(lat, lon)
            .build();
        write_mid_street(&path, &[sentinel]);
        let mid = Arc::new(MidStreetIndex::load(&path).unwrap());
        (dir, mid)
    }

    #[test]
    fn exact_match_returns_address_and_confidence_exact() {
        let ti = louise_index();
        let c = ctx(&ti, Arc::new(MidStreetIndex::empty()));
        let r = resolve(&query(&ti, "Avenue Louise", Some("32"), Some("1050")), &c);
        assert_eq!(r.confidence, Confidence::Exact);
        assert_eq!(r.house_number_delta, Some(0));
        let addr = r.address.unwrap();
        assert_eq!(addr.house_number, "32");
        assert_eq!(addr.postal_code, "1050");
        assert!(r.fallback_reason.is_none());
        assert!(r.street_match_score > 0.0);
    }

    #[test]
    fn unknown_street_is_not_found() {
        let ti = louise_index();
        let c = ctx(&ti, Arc::new(MidStreetIndex::empty()));
        let r = resolve(
            &query(&ti, "Quetzalcoatl Boulevard", Some("999"), Some("1050")),
            &c,
        );
        assert_eq!(r.confidence, Confidence::NotFound);
        assert_eq!(r.fallback_reason, Some(FallbackReason::NoMatch));
        assert!(r.address.is_none() && r.hit.is_none());
    }

    #[test]
    fn median_fallback_when_number_out_of_range() {
        let ti = louise_index();
        let (_d, mid) = sentinel_mid_street(51.0, 5.0);
        let c = ctx(&ti, mid);
        let r = resolve(
            &query(&ti, "Avenue Louise", Some("99999"), Some("1050")),
            &c,
        );
        assert_eq!(r.confidence, Confidence::MedianStreet);
        assert_eq!(
            r.fallback_reason,
            Some(FallbackReason::HouseNumberOutOfRange)
        );
        assert_eq!(r.house_number_delta, None);
        let hit = r.hit.unwrap();
        assert!((hit.lat - 51.0).abs() < 1e-9 && (hit.lon - 5.0).abs() < 1e-9);
    }

    #[test]
    fn median_fallback_when_number_missing() {
        let ti = louise_index();
        let (_d, mid) = sentinel_mid_street(51.123, 5.123);
        let c = ctx(&ti, mid);
        let r = resolve(&query(&ti, "Avenue Louise", None, Some("1050")), &c);
        assert_eq!(r.confidence, Confidence::MedianStreet);
        assert_eq!(r.fallback_reason, Some(FallbackReason::HouseNumberMissing));
        assert!((r.hit.unwrap().lat - 51.123).abs() < 1e-9);
    }

    #[test]
    fn median_uses_top_hit_coords_when_mid_street_missing() {
        let ti = louise_index();
        let c = ctx(&ti, Arc::new(MidStreetIndex::empty()));
        let r = resolve(
            &query(&ti, "Avenue Louise", Some("99999"), Some("1050")),
            &c,
        );
        assert_eq!(r.confidence, Confidence::MedianStreet);
        assert!((50.83..=50.86).contains(&r.hit.unwrap().lat));
    }

    #[test]
    fn fuzzy_street_when_a_token_is_unknown() {
        let ti = louise_index();
        let (_d, mid) = sentinel_mid_street(51.0, 5.0);
        let c = ctx(&ti, mid);
        let r = resolve(&query(&ti, "Avenue Louise Quetzal", Some("32"), None), &c);
        assert_eq!(r.confidence, Confidence::FuzzyStreet);
        assert_eq!(r.fallback_reason, Some(FallbackReason::StreetSpellingFuzzy));
        let r = resolve(&query(&ti, "Avenue Louise Quetzal", None, None), &c);
        assert_eq!(r.confidence, Confidence::FuzzyStreet);
        assert_eq!(r.fallback_reason, Some(FallbackReason::HouseNumberMissing));
    }

    #[test]
    fn notability_disabled_path_resolves() {
        let ti = louise_index();
        let mut c = ctx(&ti, Arc::new(MidStreetIndex::empty()));
        c.notability_enabled = false;
        let r = resolve(&query(&ti, "Rue Paire", Some("4"), None), &c);
        assert_eq!(r.confidence, Confidence::Exact);
        assert_eq!(r.address.unwrap().house_number, "4");
    }

    fn opts(limit: usize) -> SearchOptions {
        SearchOptions {
            limit,
            ..SearchOptions::default()
        }
    }

    #[test]
    fn search_tier_exact() {
        let ti = louise_index();
        let c = ctx(&ti, Arc::new(MidStreetIndex::empty()));
        let set = search(&query(&ti, "Avenue Louise", Some("32"), None), &opts(5), &c).unwrap();
        assert_eq!(set.tier, Tier::Exact);
        assert_eq!(set.candidates.len(), 1);
        let top = &set.candidates[0];
        assert_eq!(top.confidence, Confidence::Exact);
        assert_eq!(top.house_number_delta, Some(0));
        assert_eq!(top.hit.match_type, MatchType::Strict);
        assert!(!set.capped);
    }

    #[test]
    fn search_tier_street_lists_buildings() {
        let ti = louise_index();
        let c = ctx(&ti, Arc::new(MidStreetIndex::empty()));
        let set = search(
            &query(&ti, "Avenue Louise", Some("999"), None),
            &opts(20),
            &c,
        )
        .unwrap();
        assert_eq!(set.tier, Tier::Street);
        assert_eq!(set.candidates.len(), 8, "every Louise building");
        assert!(set.candidates.iter().all(|c| {
            c.confidence == Confidence::MedianStreet
                && c.fallback_reason == Some(FallbackReason::HouseNumberOutOfRange)
        }));
        let set = search(&query(&ti, "Avenue Louise", None, None), &opts(3), &c).unwrap();
        assert_eq!(set.tier, Tier::Street);
        assert_eq!(set.candidates.len(), 3);
        assert_eq!(
            set.candidates[0].fallback_reason,
            Some(FallbackReason::HouseNumberMissing)
        );
    }

    #[test]
    fn search_tier_or_with_number_honours_the_number() {
        let ti = louise_index();
        let c = ctx(&ti, Arc::new(MidStreetIndex::empty()));
        let set = search(
            &query(&ti, "Louise Quetzal", Some("45"), None),
            &opts(5),
            &c,
        )
        .unwrap();
        assert_eq!(set.tier, Tier::OrWithNumber);
        let top = &set.candidates[0];
        assert_eq!(top.hit.payload["house_number"], "45");
        assert_eq!(top.confidence, Confidence::FuzzyStreet);
        assert_eq!(top.house_number_delta, Some(0));
        assert_eq!(top.hit.match_type, MatchType::Fuzzy);
    }

    #[test]
    fn search_tier_or_street() {
        let ti = louise_index();
        let c = ctx(&ti, Arc::new(MidStreetIndex::empty()));
        let set = search(
            &query(&ti, "Louise Quetzal", Some("999"), None),
            &opts(5),
            &c,
        )
        .unwrap();
        assert_eq!(set.tier, Tier::OrStreet);
        assert_eq!(
            set.candidates[0].fallback_reason,
            Some(FallbackReason::StreetSpellingFuzzy)
        );
        assert_eq!(set.candidates[0].house_number_delta, None);
    }

    #[test]
    fn search_no_match_is_empty() {
        let ti = louise_index();
        let c = ctx(&ti, Arc::new(MidStreetIndex::empty()));
        let set = search(&query(&ti, "Quetzal", Some("1"), None), &opts(5), &c).unwrap();
        assert_eq!(set.tier, Tier::None);
        assert!(set.candidates.is_empty());
        // Nothing at all to search for.
        let set = search(&query(&ti, "", None, None), &opts(5), &c).unwrap();
        assert_eq!(set.tier, Tier::None);
    }

    #[test]
    fn mid_street_fallback_off_skips_street_tiers() {
        let ti = louise_index();
        let c = ctx(&ti, Arc::new(MidStreetIndex::empty()));
        let o = SearchOptions {
            mid_street_fallback: false,
            ..opts(5)
        };
        for (street, hn) in [
            ("Avenue Louise", Some("999")),
            ("Avenue Louise", None),
            ("Louise Quetzal", Some("999")),
        ] {
            let set = search(&query(&ti, street, hn, None), &o, &c).unwrap();
            assert_eq!(set.tier, Tier::None, "{street} {hn:?}");
        }
        let set = search(&query(&ti, "Avenue Louise", Some("10"), None), &o, &c).unwrap();
        assert_eq!(set.tier, Tier::Exact);
        let r = resolve_with(&query(&ti, "Avenue Louise", Some("999"), None), &o, &c);
        assert_eq!(r.confidence, Confidence::NotFound);
    }

    #[test]
    fn postal_code_is_a_hard_filter() {
        let ti = louise_index();
        let c = ctx(&ti, Arc::new(MidStreetIndex::empty()));
        let set = search(
            &query(&ti, "Avenue Louise", Some("10"), Some("1000")),
            &opts(5),
            &c,
        )
        .unwrap();
        // 1000 has no Louise: every tier is filtered to nothing.
        assert_eq!(set.tier, Tier::None);
        let set = search(&query(&ti, "", None, Some("1050")), &opts(50), &c).unwrap();
        assert_eq!(
            set.candidates.len(),
            11,
            "postal-only query lists the postal code"
        );
    }

    #[test]
    fn region_filter_applies() {
        let ti = louise_index();
        let c = ctx(&ti, Arc::new(MidStreetIndex::empty()));
        let mut q = query(&ti, "Avenue Louise", Some("10"), None);
        q.region_filter = Some("BE-VLG".into());
        assert_eq!(search(&q, &opts(5), &c).unwrap().tier, Tier::None);
        q.region_filter = Some("BE-BRU".into());
        assert_eq!(search(&q, &opts(5), &c).unwrap().tier, Tier::Exact);
    }

    #[test]
    fn focus_point_reorders_street_candidates() {
        let ti = louise_index();
        let c = ctx(&ti, Arc::new(MidStreetIndex::empty()));
        // Louise n°50 sits at lat 50.8395; focus right on it.
        let mut q = query(&ti, "Avenue Louise", None, None);
        q.focus_point = Some((50.8395, 4.3567));
        let set = search(&q, &opts(3), &c).unwrap();
        assert_eq!(set.candidates[0].hit.payload["house_number"], "50");
        // And on n°10 (lat 50.8355).
        q.focus_point = Some((50.8355, 4.3567));
        let set = search(&q, &opts(3), &c).unwrap();
        assert_eq!(set.candidates[0].hit.payload["house_number"], "10");
    }

    #[test]
    fn expand_returns_rows_not_buildings() {
        let addrs: Vec<_> = ["1", "2", "3"]
            .iter()
            .map(|b| {
                AddressBuilder::new(&format!("u{b}"), "9", "7")
                    .street(Some("Rue Unique"), None)
                    .box_no(b)
                    .build()
            })
            .collect();
        let ti = TestIndex::build(&addrs);
        let c = ctx(&ti, Arc::new(MidStreetIndex::empty()));
        let q = query(&ti, "Rue Unique", Some("7"), None);
        let set = search(&q, &opts(10), &c).unwrap();
        assert_eq!(set.candidates.len(), 1);
        assert_eq!(set.candidates[0].hit.unit_count, 3);
        assert_eq!(
            set.candidates[0].hit.payload["box_number"], "1",
            "lowest box represents"
        );
        let o = SearchOptions {
            expand: true,
            ..opts(10)
        };
        let set = search(&q, &o, &c).unwrap();
        assert_eq!(set.candidates.len(), 3);
        assert!(set.candidates.iter().all(|c| c.hit.unit_count == 1));
    }

    #[test]
    fn overfetch_cap_is_reported() {
        let addrs: Vec<_> = (0..2001)
            .map(|i| {
                AddressBuilder::new(&format!("c{i}"), "5", &i.to_string())
                    .street(Some("Rue Longue"), None)
                    .build()
            })
            .collect();
        let ti = TestIndex::build(&addrs);
        let c = ctx(&ti, Arc::new(MidStreetIndex::empty()));
        let set = search(&query(&ti, "Rue Longue", None, None), &opts(100), &c).unwrap();
        assert!(set.capped);
        assert_eq!(set.raw_hits, crate::dedup::RAW_HIT_CAP);
        assert_eq!(set.candidates.len(), 100);
    }

    #[test]
    fn resolve_agrees_with_search_top_candidate() {
        let ti = louise_index();
        let c = ctx(&ti, Arc::new(MidStreetIndex::empty()));
        for (street, hn) in [
            ("Avenue Louise", Some("32")),
            ("Avenue Louise", Some("999")),
            ("Avenue Louise", None),
            ("Louise Quetzal", Some("45")),
            ("Rue Paire", Some("4")),
            ("Quetzal", None),
        ] {
            let q = query(&ti, street, hn, None);
            let set = search(&q, &opts(1), &c).unwrap();
            let r = resolve(&q, &c);
            match set.candidates.first() {
                Some(top) => {
                    assert_eq!(r.confidence, top.confidence, "{street} {hn:?}");
                    assert_eq!(r.hit.unwrap().building_group_id, top.hit.building_group_id);
                }
                None => assert_eq!(r.confidence, Confidence::NotFound),
            }
        }
    }

    #[test]
    fn fuzzy_with_number_keeps_building_coords() {
        let ti = louise_index();
        let (_d, mid) = sentinel_mid_street(51.0, 5.0);
        let c = ctx(&ti, mid);
        let r = resolve(&query(&ti, "Louise Quetzal", Some("45"), None), &c);
        assert_eq!(r.confidence, Confidence::FuzzyStreet);
        assert_eq!(r.house_number_delta, Some(0));
        assert!(
            (r.hit.unwrap().lat - 51.0).abs() > 0.1,
            "number matched: no median swap"
        );
    }

    #[test]
    fn tier_strings() {
        for (t, s) in [
            (Tier::Exact, "exact"),
            (Tier::Street, "street"),
            (Tier::OrWithNumber, "or_with_number"),
            (Tier::OrStreet, "or_street"),
            (Tier::None, "none"),
        ] {
            assert_eq!(t.as_str(), s);
        }
        assert_eq!(Tier::None.labels(false).0, Confidence::NotFound);
        assert_eq!(Tier::None.match_type(), MatchType::Strict);
    }

    #[test]
    fn confidence_and_reason_wire_strings() {
        use Confidence as C;
        let all = [
            (C::Exact, "exact"),
            (C::NearestSameSide, "nearest_same_side"),
            (C::NearestAnySide, "nearest_any_side"),
            (C::MedianStreet, "median_street"),
            (C::FuzzyStreet, "fuzzy_street"),
            (C::AdjacentCommune, "adjacent_commune"),
            (C::NotFound, "not_found"),
        ];
        for (c, s) in all {
            assert_eq!(c.as_str(), s);
        }
        use FallbackReason as F;
        for (f, s) in [
            (F::HouseNumberMissing, "house_number_missing"),
            (F::HouseNumberOutOfRange, "house_number_out_of_range"),
            (F::StreetSpellingFuzzy, "street_spelling_fuzzy"),
            (F::PostalCodeMismatch, "postal_code_mismatch"),
            (F::NoMatch, "no_match"),
        ] {
            assert_eq!(f.as_str(), s);
        }
    }

    // --- Phase 2: parser readings, box preference, house-number case ----

    fn parsed_query(ti: &TestIndex, q: &str) -> AddressQuery {
        AddressQuery::from_parsed(&ti.index, &crabfinder_parser::parse(q))
    }

    fn hn_of(c: &Candidate) -> String {
        c.hit.payload["house_number"].as_str().unwrap().to_string()
    }

    fn best_id_of(c: &Candidate) -> String {
        let a: Address = serde_json::from_value(c.hit.payload.clone()).unwrap();
        a.id.0.to_string()
    }

    #[test]
    fn from_parsed_maps_every_field_and_alternative() {
        let ti = louise_index();
        let q = parsed_query(&ti, "Rue du Grand-Hospice 12 bte 3, B-1000");
        assert_eq!(q.street_tokens, ["rue", "du", "grand", "hospice"]);
        assert_eq!(q.house_number.as_deref(), Some("12"));
        assert_eq!(q.box_number.as_deref(), Some("3"));
        assert_eq!(q.postal_code.as_deref(), Some("1000"));
        assert!(q.alternatives.is_empty());

        let mut q = parsed_query(&ti, "Chaussée de Waterloo 1357 Uccle");
        assert_eq!(q.alternatives.len(), 1);
        assert_eq!(q.alternatives[0].house_number.as_deref(), Some("1357"));
        q.for_each_reading(|r| r.region_filter = Some("BE-BRU".into()));
        assert_eq!(q.region_filter.as_deref(), Some("BE-BRU"));
        assert_eq!(q.alternatives[0].region_filter.as_deref(), Some("BE-BRU"));
    }

    #[test]
    fn four_digit_house_number_found_through_the_alternative() {
        let ti = TestIndex::build(&fixtures::multi_region());
        let c = ctx(&ti, Arc::new(MidStreetIndex::empty()));
        let set = search(
            &parsed_query(&ti, "Chaussée de Waterloo 1357 Uccle"),
            &opts(5),
            &c,
        )
        .unwrap();
        assert_eq!(set.tier, Tier::Exact);
        assert_eq!(set.reading, 1, "the primary reading treats 1357 as postal");
        assert_eq!(hn_of(&set.candidates[0]), "1357");

        // Two 4-digit tokens: no alternative needed.
        let set = search(
            &parsed_query(&ti, "Chaussée de Waterloo 1357 1180 Uccle"),
            &opts(5),
            &c,
        )
        .unwrap();
        assert_eq!((set.tier, set.reading), (Tier::Exact, 0));
    }

    #[test]
    fn number_inside_street_name_goes_back_to_the_street() {
        let ti = TestIndex::build(&fixtures::multi_region());
        let c = ctx(&ti, Arc::new(MidStreetIndex::empty()));
        let set = search(&parsed_query(&ti, "Rue du 11 Novembre 5"), &opts(5), &c).unwrap();
        assert_eq!((set.tier, set.reading), (Tier::Exact, 0));
        assert_eq!(hn_of(&set.candidates[0]), "5");
    }

    #[test]
    fn alternatives_only_add_strict_tiers() {
        // Primary reading: hn 11 (doesn't exist) → nearest number (n°5).
        let ti = TestIndex::build(&fixtures::multi_region());
        let c = ctx(&ti, Arc::new(MidStreetIndex::empty()));
        let mut q = parsed_query(&ti, "Rue du 11 Novembre Ixelles");
        assert_eq!(q.alternatives.len(), 1);
        let set = search(&q, &opts(5), &c).unwrap();
        assert_eq!((set.tier, set.reading), (Tier::Nearest, 0));
        // Remove the street-level option: the alternative can't help (no
        // number) and the primary falls to the OR tier with its number.
        q.for_each_reading(|r| r.postal_code = Some("1050".into()));
        let off = SearchOptions {
            mid_street_fallback: false,
            ..opts(5)
        };
        // Nearest is not a street approximation: it survives mid-street off.
        let set = search(&q, &off, &c).unwrap();
        assert_eq!((set.tier, set.reading), (Tier::Nearest, 0));
        // Out of nearest range (|5 − 999| > 499) and no street tiers: the
        // alternative has no number, so only the primary's OR tier is left.
        let mut far = parsed_query(&ti, "Rue du 999 Novembre Ixelles");
        far.for_each_reading(|r| r.postal_code = Some("1050".into()));
        assert_eq!(far.alternatives.len(), 1);
        let set = search(&far, &off, &c).unwrap();
        assert_eq!((set.tier, set.reading), (Tier::None, 0));
    }

    fn boxed_building() -> TestIndex {
        let mut addrs = fixtures::louise_street();
        for (oid, b) in [("b1", "1"), ("b2", "2"), ("b3", "3")] {
            addrs.push(
                AddressBuilder::new(oid, "900", "12")
                    .street(Some("Rue des Boîtes"), Some("Dozenstraat"))
                    .box_no(b)
                    .postal("1000")
                    .build(),
            );
        }
        addrs.push(
            AddressBuilder::new("lc", "901", "32a")
                .street(Some("Rue Minuscule"), None)
                .postal("1000")
                .build(),
        );
        TestIndex::build(&addrs)
    }

    #[test]
    fn box_number_picks_the_unit_without_filtering() {
        let ti = boxed_building();
        let c = ctx(&ti, Arc::new(MidStreetIndex::empty()));
        let top = |q: &str, o: SearchOptions| {
            let set = search(&parsed_query(&ti, q), &o, &c).unwrap();
            (set.candidates.len(), best_id_of(&set.candidates[0]))
        };
        // No box → lowest box represents the building.
        assert!(top("Rue des Boîtes 12", opts(5)).1.contains("/b1/"));
        // Requested box wins; the building is still one candidate.
        let (n, id) = top("Rue des Boîtes 12 bte 2", opts(5));
        assert_eq!(n, 1);
        assert!(id.contains("/b2/"), "{id}");
        assert!(top("Rue des Boîtes 12/3", opts(5)).1.contains("/b3/"));
        // Leading zeros are ignored.
        assert!(top("Rue des Boîtes 12 bte 002", opts(5)).1.contains("/b2/"));
        // Unknown box: still found, default representative.
        assert!(top("Rue des Boîtes 12 bus 9", opts(5)).1.contains("/b1/"));
        // Expanded rows put the requested unit first.
        let expanded = SearchOptions {
            expand: true,
            ..opts(5)
        };
        let (n, id) = top("Rue des Boîtes 12 bte 2", expanded);
        assert_eq!(n, 3);
        assert!(id.contains("/b2/"));
    }

    #[test]
    fn house_number_suffix_is_case_insensitive() {
        let ti = boxed_building();
        let c = ctx(&ti, Arc::new(MidStreetIndex::empty()));
        for q in [
            "Rue Minuscule 32A",
            "Rue Minuscule 32a",
            "Rue Minuscule 32 a",
        ] {
            let set = search(&parsed_query(&ti, q), &opts(5), &c).unwrap();
            assert_eq!(set.tier, Tier::Exact, "{q}");
            assert_eq!(hn_of(&set.candidates[0]), "32a");
        }
        // Raw (batch) queries with either case match too.
        let mut q = query(&ti, "Rue Minuscule", Some("32A"), None);
        assert_eq!(resolve(&q, &c).confidence, Confidence::Exact);
        q.house_number = Some("32a".into());
        assert_eq!(resolve(&q, &c).confidence, Confidence::Exact);
    }

    #[test]
    fn normalize_box_rules() {
        assert_eq!(normalize_box("003"), "3");
        assert_eq!(normalize_box(" 000 "), "0");
        assert_eq!(normalize_box("A"), "a");
        assert_eq!(normalize_box("0A"), "a");
        assert_eq!(normalize_box("b  2"), normalize_box("B2"));
        assert_eq!(normalize_box(""), "");
    }

    // --- Phase 3: typo tiers ---------------------------------------------

    fn louise_plus_louis() -> TestIndex {
        let mut addrs = fixtures::louise_street();
        addrs.push(
            AddressBuilder::new("lv", "4000", "999")
                .street(Some("Avenue Louis"), None)
                .postal("1050")
                .build(),
        );
        TestIndex::build(&addrs)
    }

    #[test]
    fn typo_with_house_number() {
        let ti = louise_index();
        let c = ctx(&ti, Arc::new(MidStreetIndex::empty()));
        let set = search(&parsed_query(&ti, "Avenue Loiuse 32"), &opts(5), &c).unwrap();
        assert_eq!(set.tier, Tier::Typo);
        let top = &set.candidates[0];
        assert_eq!(hn_of(top), "32");
        assert_eq!(top.hit.match_type, MatchType::Typo);
        assert_eq!(top.confidence, Confidence::FuzzyStreet);
        assert_eq!(
            top.fallback_reason,
            Some(FallbackReason::StreetSpellingFuzzy)
        );
        assert_eq!(top.house_number_delta, Some(0));
    }

    #[test]
    fn typo_without_number_is_street_level_with_median() {
        let ti = louise_index();
        let (_tmp, ms) = sentinel_mid_street(50.5, 4.5);
        let c = ctx(&ti, ms);
        let set = search(&parsed_query(&ti, "Avenue Loiuse"), &opts(5), &c).unwrap();
        assert_eq!(set.tier, Tier::TypoStreet);
        let r = resolve(&parsed_query(&ti, "Avenue Loiuse 999"), &c);
        assert_eq!(r.confidence, Confidence::FuzzyStreet);
        assert_eq!(r.fallback_reason, Some(FallbackReason::StreetSpellingFuzzy));
        assert_eq!(r.house_number_delta, None);
        let hit = r.hit.unwrap();
        assert_eq!((hit.lat, hit.lon), (50.5, 4.5), "median coordinates");

        // Street-level typo tiers obey the mid-street switch.
        let off = SearchOptions {
            mid_street_fallback: false,
            ..opts(5)
        };
        let set = search(&parsed_query(&ti, "Avenue Loiuse"), &off, &c).unwrap();
        assert_eq!(set.tier, Tier::None);
    }

    #[test]
    fn typo_respects_postal_filter_and_distance() {
        let ti = louise_index();
        let c = ctx(&ti, Arc::new(MidStreetIndex::empty()));
        let tier = |q: &str| search(&parsed_query(&ti, q), &opts(5), &c).unwrap().tier;
        assert_eq!(tier("Avenue Loiuse 32 1050"), Tier::Typo);
        assert_ne!(tier("Avenue Loiuse 32 1000"), Tier::Typo);
        // Two edits on a 6-letter word: out of reach.
        assert_ne!(tier("Avenue Lxiuse 32"), Tier::Typo);
        // 5-letter word, one transposition: corrected.
        assert_eq!(tier("Rue Piare 4"), Tier::Typo);
        // 4-letter typed word: never corrected.
        assert_ne!(tier("Rue Pare 4"), Tier::Typo);
    }

    #[test]
    fn correctly_spelled_words_are_never_corrected() {
        // "Avenue Louise 999": the strict street exists, so no typo tier
        // may turn "louise" into the neighbouring "Avenue Louis 999".
        let ti = louise_plus_louis();
        let c = ctx(&ti, Arc::new(MidStreetIndex::empty()));
        let set = search(&parsed_query(&ti, "Avenue Louise 999"), &opts(5), &c).unwrap();
        assert_eq!(set.tier, Tier::Street);
        assert!(set.candidates.iter().all(|c| hn_of(c) != "999"));
        // And an exact query is exact.
        let set = search(&parsed_query(&ti, "Avenue Louis 999"), &opts(5), &c).unwrap();
        assert_eq!(set.tier, Tier::Exact);
    }

    #[test]
    fn typo_tier_strings() {
        assert_eq!(Tier::Typo.as_str(), "typo");
        assert_eq!(Tier::TypoStreet.as_str(), "typo_street");
        assert!(Tier::Typo.uses_house_number());
        assert!(!Tier::TypoStreet.uses_house_number());
        assert_eq!(MatchType::Typo.as_str(), "typo");
    }

    // --- Phase 4: nearest house number -----------------------------------

    fn nearest(ti: &TestIndex, q: &str) -> CandidateSet {
        let c = ctx(ti, Arc::new(MidStreetIndex::empty()));
        search(&parsed_query(ti, q), &opts(5), &c).unwrap()
    }

    fn summary(set: &CandidateSet) -> Vec<(String, Confidence, Option<i32>)> {
        set.candidates
            .iter()
            .map(|c| (hn_of(c), c.confidence, c.house_number_delta))
            .collect()
    }

    #[test]
    fn nearest_prefers_the_same_side() {
        // 44: odd 45 is one away, but even 40 (−4) is on the same side.
        let ti = louise_index();
        let set = nearest(&ti, "Avenue Louise 44 1050");
        assert_eq!(set.tier, Tier::Nearest);
        let top = &set.candidates[0];
        assert_eq!(hn_of(top), "40");
        assert_eq!(top.confidence, Confidence::NearestSameSide);
        assert_eq!(top.house_number_delta, Some(-4));
        assert_eq!(
            top.fallback_reason,
            Some(FallbackReason::HouseNumberOutOfRange)
        );
        assert_eq!(top.hit.match_type, MatchType::Strict);
        // Same side by distance first, other side after.
        let hns: Vec<String> = summary(&set).into_iter().map(|t| t.0).collect();
        assert_eq!(hns, ["40", "50", "32", "20", "10"]);
    }

    #[test]
    fn nearest_odd_and_letter_suffix() {
        let ti = louise_index();
        let s = nearest(&ti, "Avenue Louise 15");
        assert_eq!(
            summary(&s)[0],
            ("13".into(), Confidence::NearestSameSide, Some(-2))
        );
        let s = nearest(&ti, "Avenue Louise 34B");
        assert_eq!(
            summary(&s)[0],
            ("32".into(), Confidence::NearestSameSide, Some(-2))
        );
        // Same number, unknown suffix: delta 0.
        let s = nearest(&ti, "Avenue Louise 32C");
        assert_eq!(
            summary(&s)[0],
            ("32".into(), Confidence::NearestSameSide, Some(0))
        );
    }

    #[test]
    fn nearest_any_side_when_the_side_is_empty() {
        // Rue Paire has 2/4/6 only; 5 → tie between 4 and 6, lower wins.
        let ti = louise_index();
        let s = nearest(&ti, "Rue Paire 5");
        assert_eq!(s.tier, Tier::Nearest);
        assert_eq!(
            summary(&s)[0],
            ("4".into(), Confidence::NearestAnySide, Some(-1))
        );
        assert_eq!(hn_of(&s.candidates[1]), "6");
    }

    #[test]
    fn nearest_gives_up_beyond_the_limit() {
        assert_eq!(nearest_limit(10), 50);
        assert_eq!(nearest_limit(200), 100);
        let ti = louise_index();
        // 9999 − 50 > 4999 → street median instead.
        assert_eq!(nearest(&ti, "Avenue Louise 9999").tier, Tier::Street);
        // 110 − 50 = 60 > 55 → street median.
        assert_eq!(nearest(&ti, "Avenue Louise 110").tier, Tier::Street);
        // 100 − 50 = 50 ≤ 50 → nearest.
        assert_eq!(nearest(&ti, "Avenue Louise 100").tier, Tier::Nearest);
        // A number with no digits can't be compared.
        let c = ctx(&ti, Arc::new(MidStreetIndex::empty()));
        let set = search(
            &query(&ti, "Avenue Louise", Some("bis"), None),
            &opts(5),
            &c,
        )
        .unwrap();
        assert_eq!(set.tier, Tier::Street);
    }

    #[test]
    fn nearest_stays_within_the_postal_code() {
        // Same street object in another postal code with a closer number.
        let mut addrs = fixtures::louise_street();
        addrs.push(
            AddressBuilder::new("x39", fixtures::LOUISE_OID, "39")
                .street(Some("Avenue Louise"), Some("Louizalaan"))
                .postal("1000")
                .build(),
        );
        let ti = TestIndex::build(&addrs);
        let s = nearest(&ti, "Avenue Louise 37, 1050");
        assert_eq!(
            summary(&s)[0],
            ("45".into(), Confidence::NearestSameSide, Some(8))
        );
        let s = nearest(&ti, "Avenue Louise 37, 1000");
        assert_eq!(
            summary(&s)[0],
            ("39".into(), Confidence::NearestSameSide, Some(2))
        );
    }

    #[test]
    fn resolve_keeps_the_neighbours_own_coordinates() {
        let ti = louise_index();
        let (_tmp, ms) = sentinel_mid_street(50.5, 4.5);
        let c = ctx(&ti, ms);
        let r = resolve(&query(&ti, "Avenue Louise", Some("34"), Some("1050")), &c);
        assert_eq!(r.confidence, Confidence::NearestSameSide);
        assert_eq!(r.house_number_delta, Some(-2));
        let hit = r.hit.unwrap();
        assert!(
            (hit.lat - (50.8345 + 32.0 * 0.0001)).abs() < 1e-9,
            "n°32's own point"
        );
        assert_eq!(Tier::Nearest.as_str(), "nearest");
    }

    #[test]
    fn nearest_spans_street_objects_with_the_same_name() {
        // "Chaussée Partagée": even side in object 7001, odd side in 7002
        // (same postal code), plus a different street that also contains
        // both words and has n°31.
        let mut addrs = Vec::new();
        for (oid, street, hn) in [
            ("e2", "7001", "2"),
            ("e4", "7001", "4"),
            ("o5", "7002", "5"),
        ] {
            addrs.push(
                AddressBuilder::new(oid, street, hn)
                    .street(Some("Chaussée Partagée"), None)
                    .postal("1050")
                    .build(),
            );
        }
        addrs.push(
            AddressBuilder::new("x", "7003", "31")
                .street(Some("Rue Partagée de la Chaussée"), None)
                .postal("1050")
                .build(),
        );
        let ti = TestIndex::build(&addrs);
        // 7 is odd: the odd side lives in the other street object.
        let s = nearest(&ti, "Chaussée Partagée 7, 1050");
        assert_eq!(s.tier, Tier::Nearest);
        assert_eq!(
            summary(&s)[0],
            ("5".into(), Confidence::NearestSameSide, Some(-2))
        );
        // 33: n°31 is on a different street — nearest same side is n°5.
        let s = nearest(&ti, "Chaussée Partagée 33, 1050");
        assert!(summary(&s).iter().all(|t| t.0 != "31"), "{:?}", summary(&s));
    }

    #[test]
    fn same_name_matches_any_language() {
        let ml = |fr: Option<&str>, nl: Option<&str>| MultilingualName {
            fr: fr.map(Into::into),
            nl: nl.map(Into::into),
            de: None,
        };
        assert!(same_name(
            &ml(Some("Rue X"), None),
            &ml(Some("rue x"), Some("X-straat"))
        ));
        assert!(same_name(
            &ml(None, Some("Meir")),
            &ml(Some("Meir"), Some("MEIR"))
        ));
        assert!(!same_name(
            &ml(Some("Rue X"), None),
            &ml(None, Some("Rue X"))
        ));
    }

    // --- Phase 6: adjacent-pair phrase bonus -------------------------------

    #[test]
    fn adjacent_pairs_beat_scattered_words() {
        // "Grand-Place 1 1000 Bruxelles": the full phrase can't match
        // across the street/municipality value gap, so without the pair
        // bonus "Place du Grand Sablon" ties with "Grand-Place".
        let addrs = vec![
            AddressBuilder::new("sab", "7001", "1")
                .street(Some("Place du Grand Sablon"), Some("Grote Zavel"))
                .build(),
            AddressBuilder::new("gp", "7002", "1")
                .street(Some("Grand-Place"), Some("Grote Markt"))
                .build(),
        ];
        let ti = TestIndex::build(&addrs);
        let c = ctx(&ti, Arc::new(MidStreetIndex::empty()));
        let s = search(
            &parsed_query(&ti, "Grand-Place 1 1000 Bruxelles"),
            &opts(5),
            &c,
        )
        .unwrap();
        assert_eq!(s.tier, Tier::Exact);
        assert_eq!(s.candidates.len(), 2);
        assert_eq!(
            s.candidates[0].hit.payload["street_name"]["fr"],
            "Grand-Place"
        );
        assert!(s.candidates[0].hit.score > s.candidates[1].hit.score);
    }

    // --- Phase 6: locality step ---------------------------------------------

    fn locality(kind: LocalityKind, fr: &str, lat: f64, lon: f64, pcs: &[&str]) -> Locality {
        Locality {
            kind,
            name: MultilingualName {
                fr: Some(fr.into()),
                nl: None,
                de: None,
            },
            lat,
            lon,
            postal_codes: pcs.iter().map(|p| p.to_string()).collect(),
            address_count: 1,
            region: Some(crabfinder_core::Region::Wallonia),
        }
    }

    /// Spontin (part of Yvoir, 5530): Rue Haute near the village node,
    /// Rue Basse 2 km east. Denée (5537): Rue du Monastère next to the
    /// Maredsous hamlet node, Rue de Maredsous 3 km away.
    fn villages() -> (TestIndex, Vec<Locality>) {
        let wal = |oid: &str, street_oid: &str, street: &str, pc: &str, pom: &str| {
            AddressBuilder::new(oid, street_oid, "1")
                .region(crabfinder_core::Region::Wallonia)
                .municipality("91141", Some("Yvoir"), None, None)
                .postal(pc)
                .part_of_municipality(pom, Some(pom), None)
                .street(Some(street), None)
        };
        let addrs = vec![
            wal("a1", "8001", "Rue Haute", "5530", "Spontin")
                .at(50.3227, 5.0070)
                .build(),
            wal("a2", "8002", "Rue Basse", "5530", "Spontin")
                .at(50.3227, 5.0350)
                .build(),
            wal("a3", "8003", "Rue du Monastère", "5537", "Denée")
                .municipality("91005", Some("Anhée"), None, None)
                .at(50.3048, 4.7697)
                .build(),
            wal("a4", "8004", "Rue de Maredsous", "5537", "Denée")
                .municipality("91005", Some("Anhée"), None, None)
                .at(50.3300, 4.7900)
                .build(),
        ];
        let locs = vec![
            locality(LocalityKind::Municipality, "Yvoir", 50.33, 5.0, &["5530"]),
            // BeSt centroid between the two streets; the OSM node is the
            // village centre.
            locality(
                LocalityKind::PartOfMunicipality,
                "Spontin",
                50.3227,
                5.021,
                &["5530"],
            ),
            locality(LocalityKind::Village, "Spontin", 50.3226, 5.0069, &["5530"]),
            locality(
                LocalityKind::Hamlet,
                "Maredsous",
                50.3046,
                4.7695,
                &["5537"],
            ),
            // Same name, far away and with no address around.
            locality(LocalityKind::Hamlet, "Nulle Part", 51.3, 3.0, &["8000"]),
        ];
        (TestIndex::build(&addrs), locs)
    }

    fn ctx_with<'a>(ti: &'a TestIndex, locs: Vec<Locality>) -> ResolverContext<'a> {
        let mut c = ctx(ti, Arc::new(MidStreetIndex::empty()));
        c.localities = Arc::new(crate::localities::LocalityIndex::from_localities(
            &ti.index, locs,
        ));
        c
    }

    fn street_of_candidate(c: &Candidate) -> String {
        c.hit.payload["street_name"]["fr"]
            .as_str()
            .unwrap()
            .to_string()
    }

    #[test]
    fn locality_query_ranks_the_village_centre_first() {
        let (ti, locs) = villages();
        let c = ctx_with(&ti, locs);
        let set = search(&parsed_query(&ti, "spontin"), &opts(5), &c).unwrap();
        // The OSM node is a precise centre: the nearest buildings.
        assert_eq!(set.tier, Tier::Area);
        assert_eq!(street_of_candidate(&set.candidates[0]), "Rue Haute");
        assert_eq!(set.candidates[0].confidence, Confidence::Locality);
        assert_eq!(
            set.candidates[0].fallback_reason,
            Some(FallbackReason::StreetMissing)
        );
        assert_eq!(set.candidates[0].hit.match_type, MatchType::Spatial);
        let loc = set.locality.unwrap();
        // The OSM node, not the BeSt centroid.
        assert_eq!(
            (loc.kind, loc.lat, loc.lon),
            (LocalityKind::PartOfMunicipality, 50.3226, 5.0069)
        );

        // `resolve` answers with the nearest BeSt address, at its own BeSt
        // position — never the OSM node.
        let r = resolve(&parsed_query(&ti, "Spontin"), &c);
        assert_eq!(r.confidence, Confidence::Locality);
        let hit = r.hit.unwrap();
        let first = &set.candidates[0].hit;
        assert_eq!((hit.lat, hit.lon), (first.lat, first.lon));
        assert_ne!((hit.lat, hit.lon), (50.3226, 5.0069));
        assert_eq!(r.address.unwrap().postal_code, "5530");
    }

    #[test]
    fn village_then_container_picks_the_village() {
        let (ti, locs) = villages();
        let c = ctx_with(&ti, locs);
        for q in [
            "spontin yvoir",
            "spontin namur",
            "spontin province de namur",
        ] {
            let set = search(&parsed_query(&ti, q), &opts(5), &c).unwrap();
            assert_eq!(set.tier, Tier::Area, "{q}");
            assert_eq!(street_of_candidate(&set.candidates[0]), "Rue Haute", "{q}");
        }
        // The wrong province is no container: regular tiers.
        let set = search(&parsed_query(&ti, "spontin luxembourg"), &opts(5), &c).unwrap();
        assert!(set.locality.is_none());
    }

    #[test]
    fn container_may_be_a_name_prefix() {
        let (ti, mut locs) = villages();
        locs.push(locality(
            LocalityKind::Municipality,
            "Yvoir-sur-Meuse",
            50.33,
            5.0,
            &["9999"],
        ));
        let c = ctx_with(&ti, locs);
        // "yvoir" alone matches Yvoir exactly; drop it to test the prefix.
        let mut q = parsed_query(&ti, "spontin yvoir");
        let set = search(&q, &opts(5), &c).unwrap();
        assert_eq!(set.tier, Tier::Area);
        q.street_tokens = analyze_street_input(&ti.index, "spontin yvoir sur");
        let set = search(&q, &opts(5), &c).unwrap();
        // "yvoir sur" is a prefix of Yvoir-sur-Meuse, 1.5 km from Spontin.
        assert_eq!(set.locality.unwrap().lat, 50.3226);
    }

    #[test]
    fn centroid_locality_matches_its_name_as_text() {
        // Yvoir has no OSM node: its centre is an address centroid, so the
        // buildings carrying its name come first, nearest to it.
        let (ti, locs) = villages();
        let c = ctx_with(&ti, locs);
        let set = search(&parsed_query(&ti, "yvoir"), &opts(5), &c).unwrap();
        assert_eq!(set.tier, Tier::Locality);
        assert_eq!(set.candidates.len(), 2);
        assert_eq!(set.candidates[0].hit.match_type, MatchType::Strict);
        assert_eq!(set.candidates[0].confidence, Confidence::Locality);
        assert_eq!(set.locality.unwrap().kind, LocalityKind::Municipality);
    }

    #[test]
    fn hamlet_without_text_match_returns_the_nearest_buildings() {
        let (ti, locs) = villages();
        let c = ctx_with(&ti, locs);
        let set = search(&parsed_query(&ti, "Maredsous"), &opts(5), &c).unwrap();
        // Rue de Maredsous is 3 km away, outside the hamlet's area.
        assert_eq!(set.tier, Tier::Area);
        assert_eq!(street_of_candidate(&set.candidates[0]), "Rue du Monastère");
        assert_eq!(set.candidates[0].hit.match_type, MatchType::Spatial);
        assert_eq!(set.candidates[0].confidence, Confidence::Locality);
        assert_eq!(Tier::Area.as_str(), "area");
        assert_eq!(Tier::Locality.as_str(), "locality");
    }

    #[test]
    fn locality_step_is_skipped_when_it_does_not_apply() {
        let (ti, locs) = villages();
        let c = ctx_with(&ti, locs);
        // A house number: an address query.
        let set = search(&parsed_query(&ti, "Spontin 1"), &opts(5), &c).unwrap();
        assert!(set.locality.is_none());
        // A postal code the locality doesn't have.
        let set = search(&parsed_query(&ti, "Spontin 5537"), &opts(5), &c).unwrap();
        assert!(set.locality.is_none());
        // A locality with nothing around it falls through to the tiers.
        let set = search(&parsed_query(&ti, "Nulle Part"), &opts(5), &c).unwrap();
        assert!(set.locality.is_none());
        assert_eq!(set.tier, Tier::None);
        // Street words before the locality: not a locality query.
        let set = search(&parsed_query(&ti, "Rue Basse Spontin"), &opts(5), &c).unwrap();
        assert!(set.locality.is_none());
        assert_eq!(street_of_candidate(&set.candidates[0]), "Rue Basse");
        assert_eq!(Confidence::Locality.as_str(), "locality");
        assert_eq!(FallbackReason::StreetMissing.as_str(), "street_missing");
    }

    #[test]
    fn far_osm_namesake_is_not_the_centre() {
        let admin = locality(LocalityKind::PartOfMunicipality, "X", 50.0, 4.0, &["5000"]);
        let near = locality(LocalityKind::Village, "X", 50.05, 4.0, &["5000"]);
        let far = locality(LocalityKind::Village, "X", 50.5, 4.0, &["5000"]);
        let c = with_centre(&admin, &[&admin, &far, &near]);
        assert_eq!((c.lat, c.kind), (50.05, LocalityKind::PartOfMunicipality));
        assert!(precise_centre(&c, &[&admin, &far, &near]));
        let c = with_centre(&admin, &[&admin, &far]);
        assert_eq!(c.lat, 50.0);
        assert!(!precise_centre(&c, &[&admin, &far]));
        assert!(precise_centre(&near, &[]));
        // An OSM locality keeps its own point.
        assert_eq!(with_centre(&near, &[&admin, &near]).lat, 50.05);
        assert_eq!(locality_radius_m(LocalityKind::IsolatedDwelling), 800.0);
        assert_eq!(locality_radius_m(LocalityKind::Locality), 1_000.0);
        assert_eq!(locality_radius_m(LocalityKind::PostalArea), 4_000.0);
        assert_eq!(locality_radius_m(LocalityKind::Municipality), 5_000.0);
    }

    #[test]
    fn street_then_osm_hamlet_searches_around_the_hamlet() {
        let (ti, locs) = villages();
        let c = ctx_with(&ti, locs);
        let set = search(
            &parsed_query(&ti, "Rue du Monastère 1 Maredsous"),
            &opts(5),
            &c,
        )
        .unwrap();
        assert_eq!(set.tier, Tier::Exact);
        assert_eq!(street_of_candidate(&set.candidates[0]), "Rue du Monastère");
        assert!(set.locality.is_none());
        // Too far from the hamlet: no suffix match, OR fallback.
        let set = search(&parsed_query(&ti, "Rue Haute 1 Maredsous"), &opts(5), &c).unwrap();
        assert_eq!(set.tier, Tier::OrWithNumber);
    }

    #[test]
    fn street_then_neighbouring_municipality() {
        // Rue de la Loi lies in Bruxelles; people write the neighbouring
        // Etterbeek.
        let loi = AddressBuilder::new("loi", "9001", "17")
            .street(Some("Rue de la Loi"), Some("Wetstraat"))
            .postal("1040")
            .at(50.8434, 4.3800)
            .build();
        let ti = TestIndex::build(&[loi]);
        let c = ctx_with(
            &ti,
            vec![
                locality(
                    LocalityKind::Municipality,
                    "Etterbeek",
                    50.8330,
                    4.3890,
                    &["1040"],
                ),
                locality(
                    LocalityKind::Municipality,
                    "Etterbeek",
                    51.5,
                    3.0,
                    &["1040"],
                ),
            ],
        );
        let set = search(
            &parsed_query(&ti, "Rue de la Loi 17, 1040 Etterbeek"),
            &opts(5),
            &c,
        )
        .unwrap();
        assert_eq!(set.tier, Tier::Exact);
        assert_eq!(hn_of(&set.candidates[0]), "17");
        // Postal code the locality doesn't have: no suffix reading.
        let set = search(
            &parsed_query(&ti, "Rue de la Loi 17, 1000 Etterbeek"),
            &opts(5),
            &c,
        )
        .unwrap();
        assert_eq!(set.tier, Tier::None);
    }

    #[test]
    fn ambiguity_only_for_text_tiers_across_streets() {
        let ti = louise_index();
        let c = ctx(&ti, Arc::new(MidStreetIndex::empty()));
        // Nearest tier: scores encode distance, not relevance.
        let set = search(&query(&ti, "Avenue Louise", Some("34"), None), &opts(5), &c).unwrap();
        assert_eq!(set.tier, Tier::Nearest);
        assert_eq!(set.ambiguity(), (false, None));
        // Street tier, several buildings of one street: a tie, not ambiguous.
        let set = search(&query(&ti, "Rue Paire", None, None), &opts(5), &c).unwrap();
        assert_eq!(set.tier, Tier::Street);
        let (amb, gap) = set.ambiguity();
        assert!(!amb);
        assert!(gap.unwrap() < 0.1, "{gap:?}");
        // One candidate: no runner-up.
        let set = search(&query(&ti, "Rue Paire", Some("4"), None), &opts(5), &c).unwrap();
        assert_eq!(set.ambiguity(), (false, None));
    }

    #[test]
    fn area_ranking_does_not_depend_on_limit() {
        // 60 buildings of "Rue Longue", indexed farthest first: with the
        // `limit`-scaled over-fetch, `limit = 1` would never see the one
        // nearest to Etterbeek.
        let addrs: Vec<Address> = (0..60u32)
            .map(|i| {
                AddressBuilder::new(&format!("l{i}"), "9100", &(i + 1).to_string())
                    .street(Some("Rue Longue"), None)
                    .postal("1040")
                    .at(50.8330 + 0.0005 * f64::from(60 - i), 4.3890)
                    .build()
            })
            .collect();
        let ti = TestIndex::build(&addrs);
        let c = ctx_with(
            &ti,
            vec![locality(
                LocalityKind::Municipality,
                "Etterbeek",
                50.8330,
                4.3890,
                &["1040"],
            )],
        );
        let q = parsed_query(&ti, "Rue Longue Etterbeek");
        let one = search(&q, &opts(1), &c).unwrap();
        let ten = search(&q, &opts(10), &c).unwrap();
        assert_eq!(hn_of(&one.candidates[0]), "60");
        assert_eq!(hn_of(&ten.candidates[0]), "60");
    }

    // --- v3 Phase 11.1b: soft city words, leading types, local typos -------

    fn namur(
        oid: &str,
        street_oid: &str,
        street: &str,
        hn: &str,
        pc: &str,
        pom: &str,
    ) -> AddressBuilder {
        AddressBuilder::new(oid, street_oid, hn)
            .region(crabfinder_core::Region::Wallonia)
            .municipality("92094", Some("Namur"), None, None)
            .postal(pc)
            .part_of_municipality(pom, Some(pom), None)
            .street(Some(street), None)
    }

    /// Chaussée de Marche: 174/176 on the Jambes stretch, 797 on the
    /// stretch BeSt files under Erpent, both 5100. Rue Rogier 44 in 5000,
    /// far from the Beez village of 5000.
    fn namur_index() -> (TestIndex, Vec<Locality>) {
        let addrs = vec![
            namur(
                "j174",
                "9101",
                "Chaussée de Marche",
                "174",
                "5100",
                "Jambes",
            )
            .at(50.455, 4.875)
            .build(),
            namur(
                "j176",
                "9101",
                "Chaussée de Marche",
                "176",
                "5100",
                "Jambes",
            )
            .at(50.4551, 4.8752)
            .build(),
            namur(
                "e797",
                "9102",
                "Chaussée de Marche",
                "797",
                "5100",
                "Erpent",
            )
            .at(50.43, 4.92)
            .build(),
            namur("r44", "9103", "Rue Rogier", "44", "5000", "Namur")
                .at(50.468, 4.862)
                .build(),
        ];
        let locs = vec![
            locality(
                LocalityKind::PartOfMunicipality,
                "Jambes",
                50.455,
                4.875,
                &["5100"],
            ),
            locality(
                LocalityKind::PartOfMunicipality,
                "Beez",
                50.48,
                4.92,
                &["5000"],
            ),
            locality(
                LocalityKind::Municipality,
                "Namur",
                50.46,
                4.87,
                &["5000", "5100"],
            ),
        ];
        (TestIndex::build(&addrs), locs)
    }

    #[test]
    fn village_words_give_way_to_the_exact_number_in_the_postal_code() {
        let (ti, locs) = namur_index();
        let c = ctx_with(&ti, locs);
        let q = |s: &str| search(&parsed_query(&ti, s), &opts(5), &c).unwrap();
        // The Jambes stretch has no 797: the exact number on the other
        // stretch beats the nearest one, 621 numbers off.
        let set = q("Chaussée de Marche 797, 5100 Jambes");
        assert_eq!(
            (set.tier, hn_of(&set.candidates[0])),
            (Tier::Exact, "797".into())
        );
        // No exact number anywhere: the village words still choose.
        let set = q("Chaussée de Marche 180, 5100 Jambes");
        assert_eq!(
            (set.tier, hn_of(&set.candidates[0])),
            (Tier::Nearest, "176".into())
        );
        // Strict fails on "rogier beez", the 3 km area around Beez misses
        // Rue Rogier: the postal code alone finds it.
        let set = q("Rue Rogier 44, 5000 Beez");
        assert_eq!(
            (set.tier, hn_of(&set.candidates[0])),
            (Tier::Exact, "44".into())
        );
        // Words naming a place of another postal code are not dropped.
        let set = q("Rue Rogier 44, 5000 Jambes");
        assert_ne!(set.tier, Tier::Exact);
        // Without a postal code there is nothing to keep the street in.
        let mut nopc = parsed_query(&ti, "Rue Rogier 44 Beez");
        nopc.postal_code = None;
        assert!(without_city_words(&nopc, &c).is_none());
    }

    #[test]
    fn a_leading_street_type_is_dropped_only_as_a_last_reading() {
        let addrs = vec![
            namur(
                "f14",
                "9201",
                "Faubourg de Charleroi",
                "14",
                "6041",
                "Gosselies",
            )
            .build(),
            namur("n113", "9202", "Rue de Namur", "113", "1300", "Wavre")
                .at(50.717, 4.611)
                .build(),
            namur(
                "c115",
                "9203",
                "Chaussée de Namur",
                "115",
                "1301",
                "Bierges",
            )
            .at(50.71, 4.60)
            .build(),
        ];
        let ti = TestIndex::build(&addrs);
        let c = ctx_with(
            &ti,
            vec![
                locality(
                    LocalityKind::Municipality,
                    "Wavre",
                    50.717,
                    4.611,
                    &["1300", "1301"],
                ),
                locality(
                    LocalityKind::Municipality,
                    "Charleroi",
                    50.41,
                    4.44,
                    &["6041"],
                ),
            ],
        );
        let q = |s: &str| search(&parsed_query(&ti, s), &opts(5), &c).unwrap();
        let set = q("Rue Faubourg de Charleroi 14, 6041");
        assert_eq!(
            (set.tier, hn_of(&set.candidates[0])),
            (Tier::Exact, "14".into())
        );
        // The right street across the postal boundary wins over
        // "de namur" (Chaussée de Namur) inside it.
        let set = q("Rue de Namur 113, 1301");
        assert_eq!(set.candidates[0].confidence, Confidence::AdjacentCommune);
        assert_eq!(street_of_candidate(&set.candidates[0]), "Rue de Namur");
        // Two words at least must remain, and only a known type goes.
        let two = parsed_query(&ti, "Rue Rogier 1");
        assert!(without_leading_type(&two).is_none());
        let other = parsed_query(&ti, "Grand Faubourg de Charleroi 1");
        assert!(without_leading_type(&other).is_none());
    }

    #[test]
    fn words_unknown_in_the_postal_code_are_corrected_there() {
        let addrs = vec![
            // BeSt's Brussels spelling, and a Koksijde namesake of the
            // typed form that keeps the regular typo tier away.
            AddressBuilder::new("ecu", "9301", "19")
                .street(Some("Rue de l'Ecuyer"), Some("Schildknaapsstraat"))
                .postal("1000")
                .build(),
            AddressBuilder::new("kok", "9302", "7")
                .region(crabfinder_core::Region::Flanders)
                .street(None, Some("Schildknaapstraat"))
                .postal("8670")
                .at(51.12, 2.65)
                .build(),
        ];
        let ti = TestIndex::build(&addrs);
        let c = ctx_with(
            &ti,
            vec![
                locality(
                    LocalityKind::Municipality,
                    "Bruxelles",
                    50.85,
                    4.35,
                    &["1000"],
                ),
                locality(
                    LocalityKind::Municipality,
                    "Koksijde",
                    51.12,
                    2.65,
                    &["8670"],
                ),
            ],
        );
        let set = search(
            &parsed_query(&ti, "Schildknaapstraat 19, 1000"),
            &opts(5),
            &c,
        )
        .unwrap();
        assert_eq!(
            (set.tier, hn_of(&set.candidates[0])),
            (Tier::Typo, "19".into())
        );
        // Nothing unknown in the postal code: no local typo pass.
        let ok = parsed_query(&ti, "Schildknaapsstraat 19, 1000");
        assert!(search_local_typo(&ok, &opts(5), &c).unwrap().is_none());
        let mut nopc = parsed_query(&ti, "Schildknaapstraat 19");
        nopc.postal_code = None;
        assert!(search_local_typo(&nopc, &opts(5), &c).unwrap().is_none());
    }
}
