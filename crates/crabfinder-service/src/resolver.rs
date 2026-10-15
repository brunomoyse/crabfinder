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
use crabfinder_parser::ParsedQuery;

use crate::abbreviation::AbbreviationMap;
use crate::addr_count::SharedAddressCountIndex;
use crate::dedup::{MatchType, RAW_HIT_CAP, RawHit, group_by_building, raw_limit};
use crate::focus::apply_focus_decay;
use crate::hydrate::{HydratedHit, hydrate};
use crate::mid_street::{MidStreetIndex, street_key_from_bgid};
use crate::query_builder::{
    FilterParams, analyze, build_fuzzy_query, build_strict_query, build_typo_query,
    search_score_only, search_with_notability, searchable_field, slot_is_unknown,
};

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
    /// 4-digit Belgian postal code as a string.
    pub postal_code: Option<String>,
    /// Indexed ISO region form (`BE-BRU` / `BE-VLG` / `BE-WAL`). Optional
    /// hard filter — passed straight through to the Tantivy MUST clause.
    pub region_filter: Option<String>,
    /// Optional geographic hint as `(lat, lng)`. When set, results are
    /// re-scored after retrieval by exponential distance decay
    /// (`focus::apply_focus_decay`).
    pub focus_point: Option<(f64, f64)>,
    /// Other readings of the same free text (parser alternatives, Phase 2),
    /// tried in order when this one has no strict-tier hit. Alternatives
    /// never carry alternatives of their own.
    pub alternatives: Vec<AddressQuery>,
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
    /// Street found only by widening to a neighbouring commune.
    /// _Reserved for post-v1._
    AdjacentCommune,
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
    /// Street matched only in an adjacent commune (post-v1).
    PostalCodeMismatch,
    /// No match at all.
    NoMatch,
}

impl FallbackReason {
    pub fn as_str(self) -> &'static str {
        match self {
            FallbackReason::HouseNumberMissing => "house_number_missing",
            FallbackReason::HouseNumberOutOfRange => "house_number_out_of_range",
            FallbackReason::StreetSpellingFuzzy => "street_spelling_fuzzy",
            FallbackReason::PostalCodeMismatch => "postal_code_mismatch",
            FallbackReason::NoMatch => "no_match",
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
            Tier::None => "none",
        }
    }

    fn match_type(self) -> MatchType {
        match self {
            Tier::Exact | Tier::Nearest | Tier::Street | Tier::None => MatchType::Strict,
            Tier::Typo | Tier::TypoStreet => MatchType::Typo,
            Tier::OrWithNumber | Tier::OrStreet => MatchType::Fuzzy,
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
}

impl CandidateSet {
    fn empty() -> Self {
        Self {
            candidates: Vec::new(),
            raw_hits: 0,
            tier: Tier::None,
            capped: false,
            reading: 0,
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
    let readings = std::iter::once(query).chain(query.alternatives.iter());
    for (reading, q) in readings.enumerate() {
        if let Some(set) = search_tiers(q, opts, ctx, true)? {
            return Ok(CandidateSet { reading, ..set });
        }
    }
    Ok(search_tiers(query, opts, ctx, false)?.unwrap_or_else(CandidateSet::empty))
}

/// Run the strict (`strict == true`) or OR tiers of one reading.
fn search_tiers(
    query: &AddressQuery,
    opts: &SearchOptions,
    ctx: &ResolverContext<'_>,
    strict: bool,
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
    let raw_limit = raw_limit(opts.limit, opts.expand);
    // Which slots the typo tiers make fuzzy; computed on first use.
    let mut unknown: Option<Vec<bool>> = None;
    // The Nearest tier's street hits, reused by the Street tier.
    let mut street_top: Option<Vec<(tantivy::Score, tantivy::DocAddress)>> = None;
    let run = |q: &dyn tantivy::query::Query| {
        if ctx.notability_enabled {
            search_with_notability(&searcher, q, raw_limit, ctx.addr_counts.clone())
        } else {
            search_score_only(&searcher, q, raw_limit)
        }
    };

    for tier in Tier::ORDER.into_iter().filter(|t| t.is_strict() == strict) {
        let enabled = match tier {
            // Nearest is not a street approximation: it stays on with
            // mid-street off (design-batch-geocoding §236).
            Tier::Exact | Tier::Typo | Tier::Nearest | Tier::OrWithNumber => has_hn,
            Tier::Street | Tier::TypoStreet | Tier::OrStreet => opts.mid_street_fallback,
            Tier::None => false,
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
                    collect_candidates(&searcher, &units, tier, has_hn, query, opts, ctx)?;
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
            return collect_candidates(&searcher, &top, tier, has_hn, query, opts, ctx).map(Some);
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
        return collect_candidates(&searcher, &top, tier, has_hn, query, opts, ctx).map(Some);
    }
    Ok(None)
}

/// Materialise a tier's raw hits into ranked, labelled candidates.
fn collect_candidates(
    searcher: &tantivy::Searcher,
    top: &[(tantivy::Score, tantivy::DocAddress)],
    tier: Tier,
    has_hn: bool,
    query: &AddressQuery,
    opts: &SearchOptions,
    ctx: &ResolverContext<'_>,
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
    // only scramble it.
    if tier != Tier::Nearest {
        if let Some(b) = query.box_number.as_deref() {
            prefer_box(&mut raw_hits, b);
        }
        if let Some(focus) = query.focus_point {
            apply_focus_decay(&mut raw_hits, focus, ctx.schema, &docs);
        }
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
    })
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
fn normalize_box(b: &str) -> String {
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

/// `resolve` with explicit options (`limit` / `expand` are ignored).
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
        limit: 1,
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
    let Some(c) = set.candidates.into_iter().next() else {
        return ResolvedAddress::not_found();
    };
    let Ok(address) = serde_json::from_value::<Address>(c.hit.payload.clone()) else {
        warn!(bgid = %c.hit.building_group_id, "resolver: payload failed serde decode");
        return ResolvedAddress::not_found();
    };
    let mut hit = c.hit;
    if !set.tier.uses_house_number()
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
}
