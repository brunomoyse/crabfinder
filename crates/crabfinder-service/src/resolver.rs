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

    // Helper closure to run a query and return the top hit. Takes a
    // `Box<dyn Query>` (Sized) — `Searcher::search` requires `Q: Sized`,
    // so we hold the Box and pass it through as `&Box<dyn Query>`.
    let search_top1 = |q: &Box<dyn tantivy::query::Query>| -> Option<(tantivy::Score, tantivy::DocAddress)> {
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
            .and_then(|q| search_top1(&q))
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
    let median_hit =
        build_strict_query(field, &slots, &parsed_without_hn, ctx.schema, &filters)
            .and_then(|q| search_top1(&q));
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
    let fuzzy_hit =
        build_fuzzy_query(field, &slots, &parsed_without_hn, ctx.schema, &filters)
            .and_then(|q| search_top1(&q));
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
    use crabfinder_core::{
        Address, AddressId, BestId, BuildingGroupId, Coordinates, MultilingualName,
        MunicipalityId, PostalInfoId, Region, Status, StreetId, StreetType, WgsCoord,
    };
    use crabfinder_indexer::index::{CrabIndexWriter, CrabSchema, register_tokenizers};
    use crabfinder_indexer::meta::write_sidecar;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use tantivy::{Index, ReloadPolicy};

    static SEQ: AtomicU64 = AtomicU64::new(0);

    fn unique_tempdir() -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let p = std::env::temp_dir().join(format!(
            "crab-resolver-test-{}-{}-{}",
            std::process::id(),
            nanos,
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn make_louise_addr(addr_oid: &str, house: &str, box_no: Option<&str>) -> Address {
        let ns_addr = "https://databrussels.be/id/address".to_string();
        let ns_street = "https://databrussels.be/id/streetname".to_string();
        let ns_muni = "https://databrussels.be/id/municipality".to_string();
        let ns_postal = "https://databrussels.be/id/postalinfo".to_string();
        let street_id = StreetId::new(BestId::new(ns_street, "3919", "1"));
        let building_group_id = BuildingGroupId::from_parts(&street_id, house);
        Address {
            id: AddressId::new(BestId::new(ns_addr, addr_oid, "1")),
            street_id,
            municipality_id: MunicipalityId::new(BestId::new(ns_muni, "21001", "1")),
            postal_info_id: PostalInfoId::new(BestId::new(ns_postal, "1050", "1")),
            part_of_municipality_id: None,
            region: Region::Brussels,
            status: Status::Current,
            house_number: house.into(),
            box_number: box_no.map(|s| s.into()),
            officially_assigned: true,
            street_name: MultilingualName {
                fr: Some("Avenue Louise".into()),
                nl: Some("Louizalaan".into()),
                de: None,
            },
            street_type: StreetType::Streetname,
            municipality_name: MultilingualName {
                fr: Some("Bruxelles".into()),
                nl: Some("Brussel".into()),
                de: None,
            },
            postal_code: "1050".into(),
            postal_name: MultilingualName::default(),
            part_of_municipality_name: None,
            coordinates: Coordinates::Wgs84(WgsCoord {
                lat: 50.8345 + (house.parse::<u32>().unwrap_or(0) as f64) * 0.0001,
                lon: 4.3567,
            }),
            building_group_id,
        }
    }

    fn build_test_index_louise() -> (PathBuf, Index) {
        let dir = unique_tempdir();
        let mut writer = CrabIndexWriter::create(&dir).unwrap();
        // 5 buildings on Avenue Louise: #10, #20, #32, #40, #50.
        // Each has distinct lat so the test can tell which one was returned.
        for hn in ["10", "20", "32", "40", "50"] {
            writer
                .add_address(&make_louise_addr(&format!("a{hn}"), hn, None))
                .unwrap();
        }
        writer.commit().unwrap();
        write_sidecar(&dir).unwrap();
        let index = Index::open_in_dir(&dir).unwrap();
        register_tokenizers(&index);
        (dir, index)
    }

    fn build_ctx<'a>(
        index: &'a Index,
        reader: &'a tantivy::IndexReader,
        schema: &'a CrabSchema,
        mid_street: Arc<MidStreetIndex>,
    ) -> ResolverContext<'a> {
        let addr_counts = Arc::new(AddressCountIndex::build(reader, schema).unwrap());
        let abbrev = Arc::new(AbbreviationMap::empty());
        ResolverContext {
            index,
            reader,
            schema,
            abbrev,
            addr_counts,
            notability_enabled: true,
            mid_street,
        }
    }

    #[test]
    fn resolve_exact_match_returns_address_and_confidence_exact() {
        let (_dir, index) = build_test_index_louise();
        let schema = CrabSchema::build();
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()
            .unwrap();
        let ctx = build_ctx(&index, &reader, &schema, Arc::new(MidStreetIndex::empty()));
        let q = AddressQuery {
            street_tokens: analyze_street_input(&index, "Avenue Louise"),
            house_number: Some("32".into()),
            postal_code: Some("1050".into()),
            region_filter: None,
            focus_point: None,
        };
        let r = resolve(&q, &ctx);
        assert_eq!(r.confidence, Confidence::Exact);
        assert_eq!(r.house_number_delta, Some(0));
        let addr = r.address.unwrap();
        assert_eq!(addr.house_number.as_str(), "32");
        assert_eq!(addr.postal_code.as_str(), "1050");
        assert!(r.fallback_reason.is_none());
    }

    #[test]
    fn resolve_unknown_street_returns_not_found_or_fuzzy() {
        let (_dir, index) = build_test_index_louise();
        let schema = CrabSchema::build();
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()
            .unwrap();
        let ctx = build_ctx(&index, &reader, &schema, Arc::new(MidStreetIndex::empty()));
        let q = AddressQuery {
            street_tokens: analyze_street_input(&index, "Quetzalcoatl Boulevard"),
            house_number: Some("999".into()),
            postal_code: Some("1050".into()),
            region_filter: None,
            focus_point: None,
        };
        let r = resolve(&q, &ctx);
        assert!(matches!(
            r.confidence,
            Confidence::NotFound | Confidence::FuzzyStreet
        ));
    }

    // --- Lot C: MedianStreet behaviour ----------------------------------

    /// Helper: build a `MidStreetIndex` in-memory with a single entry for
    /// Avenue Louise in 1050. The tests don't go through the side-file
    /// loader — they construct the map directly via the public test API.
    fn mid_street_for_louise(lat: f64, lon: f64) -> Arc<MidStreetIndex> {
        // Build the side-file in a temp dir and load it through the normal
        // public API. Exercises the load path, not just an in-memory map.
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(b"CRBMID7\0").unwrap();
        f.write_all(&1u64.to_le_bytes()).unwrap();
        let sk = "https://databrussels.be/id/streetname|3919";
        f.write_all(&(sk.len() as u32).to_le_bytes()).unwrap();
        f.write_all(sk.as_bytes()).unwrap();
        f.write_all(&4u8.to_le_bytes()).unwrap();
        f.write_all(b"1050").unwrap();
        f.write_all(&lat.to_le_bytes()).unwrap();
        f.write_all(&lon.to_le_bytes()).unwrap();
        f.flush().unwrap();
        Arc::new(MidStreetIndex::load(f.path()).unwrap())
    }

    /// Avenue Louise has #10/#20/#32/#40/#50 in the fixture; querying #99999
    /// (out-of-range) should drop to MedianStreet using the mid_street
    /// coords, not the top hit's individual building coord.
    #[test]
    fn median_fallback_when_number_out_of_range() {
        let (_dir, index) = build_test_index_louise();
        let schema = CrabSchema::build();
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()
            .unwrap();
        // Mid_street pinned to a sentinel coordinate (51.0, 5.0).
        let mid = mid_street_for_louise(51.0, 5.0);
        let ctx = build_ctx(&index, &reader, &schema, mid);
        let q = AddressQuery {
            street_tokens: analyze_street_input(&index, "Avenue Louise"),
            house_number: Some("99999".into()),
            postal_code: Some("1050".into()),
            region_filter: None,
            focus_point: None,
        };
        let r = resolve(&q, &ctx);
        assert_eq!(r.confidence, Confidence::MedianStreet, "got {:?}", r.confidence);
        assert_eq!(r.fallback_reason, Some(FallbackReason::HouseNumberOutOfRange));
        assert_eq!(r.house_number_delta, None);
        let hit = r.hit.expect("median hit must populate HydratedHit");
        assert!(
            (hit.lat - 51.0).abs() < 1e-9 && (hit.lon - 5.0).abs() < 1e-9,
            "expected sentinel mid_street coords, got ({}, {})",
            hit.lat,
            hit.lon
        );
    }

    /// Without a house_number in the query at all, we should land directly
    /// in MedianStreet with reason=HouseNumberMissing.
    #[test]
    fn median_fallback_when_number_missing() {
        let (_dir, index) = build_test_index_louise();
        let schema = CrabSchema::build();
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()
            .unwrap();
        let mid = mid_street_for_louise(51.123, 5.123);
        let ctx = build_ctx(&index, &reader, &schema, mid);
        let q = AddressQuery {
            street_tokens: analyze_street_input(&index, "Avenue Louise"),
            house_number: None,
            postal_code: Some("1050".into()),
            region_filter: None,
            focus_point: None,
        };
        let r = resolve(&q, &ctx);
        assert_eq!(r.confidence, Confidence::MedianStreet);
        assert_eq!(r.fallback_reason, Some(FallbackReason::HouseNumberMissing));
        let hit = r.hit.unwrap();
        assert!((hit.lat - 51.123).abs() < 1e-9, "expected mid_street coords");
    }

    /// When the side-table has no entry for this (street, postal), the
    /// resolver should still produce MedianStreet — but with the top hit's
    /// own coordinates (graceful degradation).
    #[test]
    fn median_falls_through_to_top_hit_coords_when_mid_street_missing() {
        let (_dir, index) = build_test_index_louise();
        let schema = CrabSchema::build();
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()
            .unwrap();
        let ctx = build_ctx(&index, &reader, &schema, Arc::new(MidStreetIndex::empty()));
        let q = AddressQuery {
            street_tokens: analyze_street_input(&index, "Avenue Louise"),
            house_number: Some("99999".into()),
            postal_code: Some("1050".into()),
            region_filter: None,
            focus_point: None,
        };
        let r = resolve(&q, &ctx);
        assert_eq!(r.confidence, Confidence::MedianStreet);
        // Without mid_street, the hit.lat should equal whichever top hit
        // Tantivy returned. We just check it lies in the fixture range.
        let hit = r.hit.unwrap();
        assert!(
            (50.83..=50.86).contains(&hit.lat),
            "lat should be a fixture coord, got {}",
            hit.lat
        );
    }
}
