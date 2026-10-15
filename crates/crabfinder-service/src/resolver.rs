//! Address-resolution core.
//!
//! Pure logic that takes a structured `AddressQuery` and returns a single
//! `ResolvedAddress` decision. No HTTP, no CSV, no CLI plumbing — those
//! belong to the façades (the HTTP /geocode handler and the future batch
//! HTTP/CLI endpoints).
//!
//! See `docs/design-batch-geocoding.md` §1 for the architectural rationale.
//!
//! Lot A scope: only `Confidence::Exact`, `Confidence::FuzzyStreet`, and
//! `Confidence::NotFound` are produced. `MedianStreet`, `NearestSameSide`,
//! `NearestAnySide`, and `AdjacentCommune` are introduced by later lots —
//! the enum variants exist now so the output schema is stable across lots.
//!
//! `dead_code` is silenced module-wide: the surface lands now but isn't
//! consumed until Lot D (CSV batch path) wires the resolver into a façade.
#![allow(dead_code)]

use std::sync::Arc;

use tantivy::{Index, IndexReader, TantivyDocument, schema::Value};
use tracing::warn;

use crabfinder_core::Address;
use crabfinder_indexer::index::CrabSchema;
use crabfinder_parser::ParsedQuery;

use crate::abbreviation::AbbreviationMap;
use crate::addr_count::SharedAddressCountIndex;
use crate::dedup::{MatchType, RawHit, group_by_building};
use crate::hydrate::{HydratedHit, hydrate};
use crate::mid_street::{MidStreetIndex, street_key_from_bgid};
use crate::query_builder::{
    FilterParams, analyze, build_fuzzy_query, build_strict_query, search_score_only,
    search_with_notability, searchable_field,
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
    /// Suffixes are kept as-is — the indexer stores them on the same field.
    pub house_number: Option<String>,
    /// 4-digit Belgian postal code as a string.
    pub postal_code: Option<String>,
    /// Indexed ISO region form (`BE-BRU` / `BE-VLG` / `BE-WAL`). Optional
    /// hard filter — passed straight through to the Tantivy MUST clause.
    pub region_filter: Option<String>,
    /// Optional geographic hint as `(lat, lng)`. When set, results are
    /// re-scored after retrieval by exponential distance decay. The batch
    /// case typically leaves this `None` (no per-row hint), but the field
    /// is here so the same `resolve()` serves recherche later.
    pub focus_point: Option<(f64, f64)>,
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
    /// _Reserved for Lot C+ — not produced in Lot A._
    NearestSameSide,
    /// Street matched but house number was approximated to the nearest
    /// number on the other side of the street.
    /// _Reserved for Lot C+ — not produced in Lot A._
    NearestAnySide,
    /// Street matched but house number was unknown or out of range; result
    /// is the centroid of the street within the requested postal code.
    /// _Reserved for Lot C — not produced in Lot A._
    MedianStreet,
    /// Strict query failed; result came from the OR-of-tokens fuzzy fallback.
    /// House number, if provided, did match — only the street name was fuzzy.
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

/// Resolve a single address. Always returns a `ResolvedAddress` — never
/// errors out (Tantivy IO failures are reported as `NotFound` with a log
/// line; the alternative is to bubble errors per-row and have the batch
/// caller decide whether to abort or skip, which doubles the API surface
/// for marginal benefit on a well-built index).
///
/// Resolution order (Lot C scope):
/// 1. **Strict with house_number** (when provided) → `Confidence::Exact`.
/// 2. **Strict without house_number** → `Confidence::MedianStreet` using
///    the mid_street side-table for coordinates. `fallback_reason` is
///    `HouseNumberOutOfRange` if a number was supplied, `HouseNumberMissing`
///    if not.
/// 3. **Fuzzy without house_number** → `Confidence::FuzzyStreet`.
///    Coordinates come from mid_street when available (so a fuzzy-matched
///    street with no number resolves to the street centroid), else from
///    the top fuzzy hit's own lat/lon.
/// 4. Otherwise → `Confidence::NotFound`.
pub fn resolve(query: &AddressQuery, ctx: &ResolverContext<'_>) -> ResolvedAddress {
    let slots: Vec<Vec<String>> = query
        .street_tokens
        .iter()
        .map(|t| ctx.abbrev.expand(t))
        .collect();

    let parsed_with_hn = ParsedQuery {
        raw_query: String::new(),
        street_tokens: query.street_tokens.clone(),
        house_number: query.house_number.clone(),
        postal_code: query.postal_code.clone(),
    };
    let parsed_without_hn = ParsedQuery {
        raw_query: String::new(),
        street_tokens: query.street_tokens.clone(),
        house_number: None,
        postal_code: query.postal_code.clone(),
    };

    let filters = FilterParams {
        region: query.region_filter.clone(),
        postal_code: None,
    };

    let field = searchable_field(ctx.schema);
    let searcher = ctx.reader.searcher();

    // Run a query and return the top hit.
    let search_top1 =
        |q: &dyn tantivy::query::Query| -> Option<(tantivy::Score, tantivy::DocAddress)> {
            let res = if ctx.notability_enabled {
                search_with_notability(&searcher, q, 1, ctx.addr_counts.clone())
            } else {
                search_score_only(&searcher, q, 1)
            };
            match res {
                Ok(mut hits) => hits.pop(),
                Err(e) => {
                    warn!(error=%e, "resolver: tantivy search failed; treating as no-hit");
                    None
                }
            }
        };

    // --- Step 1: strict with house_number (Exact).
    let exact_hit = if query.house_number.is_some() {
        build_strict_query(field, &slots, &parsed_with_hn, ctx.schema, &filters)
            .and_then(|q| search_top1(q.as_ref()))
    } else {
        None
    };
    if let Some((score, doc_addr)) = exact_hit {
        if let Some(resolved) = finalize(
            &searcher,
            doc_addr,
            score,
            ctx.schema,
            MatchType::Strict,
            Confidence::Exact,
            None,
            Some(0),
            None,
        ) {
            return resolved;
        }
        // Failed to hydrate / deserialize — fall through to attempt fallbacks.
        warn!("resolver: exact hit materialization failed; falling through");
    }

    // --- Step 2: strict WITHOUT house_number (MedianStreet).
    let median_hit = build_strict_query(field, &slots, &parsed_without_hn, ctx.schema, &filters)
        .and_then(|q| search_top1(q.as_ref()));
    if let Some((score, doc_addr)) = median_hit {
        let reason = if query.house_number.is_some() {
            FallbackReason::HouseNumberOutOfRange
        } else {
            FallbackReason::HouseNumberMissing
        };
        let coords_override = lookup_median_for(&searcher, doc_addr, ctx);
        if let Some(resolved) = finalize(
            &searcher,
            doc_addr,
            score,
            ctx.schema,
            MatchType::Strict,
            Confidence::MedianStreet,
            Some(reason),
            None,
            coords_override,
        ) {
            return resolved;
        }
        warn!("resolver: median hit materialization failed; falling through");
    }

    // --- Step 3: fuzzy without house_number (FuzzyStreet).
    let fuzzy_hit = build_fuzzy_query(field, &slots, &parsed_without_hn, ctx.schema, &filters)
        .and_then(|q| search_top1(q.as_ref()));
    if let Some((score, doc_addr)) = fuzzy_hit {
        // Even on fuzzy, if mid_street has the centroid for the resolved
        // street, prefer that over the top fuzzy hit's individual building
        // coords. The user gets a more representative point.
        let coords_override = lookup_median_for(&searcher, doc_addr, ctx);
        let reason = if query.house_number.is_some() {
            FallbackReason::StreetSpellingFuzzy
        } else {
            FallbackReason::HouseNumberMissing
        };
        if let Some(resolved) = finalize(
            &searcher,
            doc_addr,
            score,
            ctx.schema,
            MatchType::Fuzzy,
            Confidence::FuzzyStreet,
            Some(reason),
            None,
            coords_override,
        ) {
            return resolved;
        }
    }

    ResolvedAddress::not_found()
}

#[allow(clippy::too_many_arguments)] // replaced by `search` in Phase 1
/// Materialize a doc into a `ResolvedAddress`. Returns `None` if hydration
/// or payload deserialization fails — the caller can either fall through
/// to a less-confident pass or return `NotFound`.
fn finalize(
    searcher: &tantivy::Searcher,
    doc_addr: tantivy::DocAddress,
    score: tantivy::Score,
    schema: &CrabSchema,
    match_type: MatchType,
    confidence: Confidence,
    fallback_reason: Option<FallbackReason>,
    house_number_delta: Option<i32>,
    coords_override: Option<(f64, f64)>,
) -> Option<ResolvedAddress> {
    let doc: TantivyDocument = match searcher.doc(doc_addr) {
        Ok(d) => d,
        Err(e) => {
            warn!(error=%e, "resolver: doc materialization failed");
            return None;
        }
    };
    let bgid = doc
        .get_first(schema.building_group_id)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let box_no = doc
        .get_first(schema.box_number)
        .and_then(|v| v.as_str())
        .map(|x| x.to_string());
    let addr_id = doc
        .get_first(schema.best_id)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let raw_hit = RawHit {
        score,
        building_group_id: bgid.clone(),
        box_number: box_no,
        address_id: addr_id,
        match_type,
        raw_index: 0,
    };
    let groups = group_by_building(std::slice::from_ref(&raw_hit));
    let group = groups
        .into_iter()
        .next()
        .expect("group_by_building on non-empty must return ≥1 group");

    let mut hit = match hydrate(
        &doc,
        schema,
        group.max_score,
        group.match_type,
        group.unit_count,
        group.building_group_id,
        None,
    ) {
        Ok(h) => h,
        Err(e) => {
            warn!(error=%e, "resolver: hydrate failed");
            return None;
        }
    };

    // Median / fuzzy paths swap in centroid coords if available.
    if let Some((lat, lon)) = coords_override {
        hit.lat = lat;
        hit.lon = lon;
    }

    let address: Option<Address> = serde_json::from_value(hit.payload.clone()).ok();
    if address.is_none() {
        warn!(bgid = %bgid, "resolver: address payload failed serde decode; returning None");
        return None;
    }

    Some(ResolvedAddress {
        address,
        hit: Some(hit),
        confidence,
        house_number_delta,
        street_match_score: group.max_score,
        fallback_reason,
    })
}

/// Pull the (street_key, postal_code) pair from a doc and look it up in the
/// mid_street side-table. Returns `None` when the lookup misses (pre-v7
/// index, street that has no median entry, etc.) — caller falls back to
/// the doc's own coords.
fn lookup_median_for(
    searcher: &tantivy::Searcher,
    doc_addr: tantivy::DocAddress,
    ctx: &ResolverContext<'_>,
) -> Option<(f64, f64)> {
    let doc: TantivyDocument = searcher.doc(doc_addr).ok()?;
    let bgid = doc
        .get_first(ctx.schema.building_group_id)
        .and_then(|v| v.as_str())?;
    let postal = doc
        .get_first(ctx.schema.postal_code)
        .and_then(|v| v.as_str())?;
    let street_key = street_key_from_bgid(bgid)?;
    ctx.mid_street.lookup(street_key, postal)
}

/// Helper for batch callers: take a raw user-provided street string and produce
/// the analyzed token vec that goes into `AddressQuery::street_tokens`. Mirrors
/// what the HTTP `/geocode` handler does after parsing.
#[allow(dead_code)] // consumed by Lot D once batch I/O lands
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
            region_filter: None,
            focus_point: None,
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
}
