//! `focus_point` parsing + exponential distance decay applied to raw hits.
//!
//! Extracted from `main.rs` so both `/geocode` and (future) batch consumers
//! share the same decay math. No HTTP / serde / Axum dependency.

use tantivy::{TantivyDocument, schema::Value};

use crabfinder_indexer::index::CrabSchema;

use crate::dedup::RawHit;
use crate::spatial;

/// Half-life of the focus_point distance decay, in metres. A candidate at
/// `FOCUS_HALF_LIFE_M` metres from the focus point gets its score multiplied
/// by `1/e ≈ 0.37`. 25 km is chosen for Belgian scale: tight enough to
/// disambiguate within a city, loose enough that a Liège user can still
/// land Brussels matches when the local pool is empty.
pub const FOCUS_HALF_LIFE_M: f64 = 25_000.0;

/// Parse a `focus_point` string of the form `"lat,lng"` (WGS84). Permissive
/// on whitespace; rejects malformed or out-of-Belgium values.
pub fn parse_focus_point(raw: &str) -> Option<(f64, f64)> {
    let mut parts = raw.split(',').map(str::trim);
    let lat: f64 = parts.next()?.parse().ok()?;
    let lng: f64 = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    // Belgian bbox check (lenient — accept anywhere in NW Europe to keep the
    // focus_point usable on border cases). Range is wider than Belgium but
    // narrow enough to reject obvious typos like `lng,lat` reversal.
    if !(49.0..=52.0).contains(&lat) || !(2.0..=7.0).contains(&lng) {
        return None;
    }
    Some((lat, lng))
}

/// Multiply each hit's score by `exp(-distance / FOCUS_HALF_LIFE_M)` using
/// the hit's underlying doc lat/lon. Docs are indexed by `hit.raw_index`.
/// Re-sorts hits descending by post-decay score so dedup picks the right
/// representative.
///
/// Pre-condition: `docs[hit.raw_index]` is the doc that produced `hit`.
pub fn apply_focus_decay(
    hits: &mut [RawHit],
    focus: (f64, f64),
    schema: &CrabSchema,
    docs: &[TantivyDocument],
) {
    apply_decay(hits, focus, FOCUS_HALF_LIFE_M, schema, docs);
}

/// Decay length around a named locality's centre (v2 Phase 6): a village
/// query should rank its centre's buildings first.
pub const LOCALITY_DECAY_M: f64 = 1_000.0;

/// `apply_focus_decay` with an explicit decay length `scale_m`.
pub fn apply_decay(
    hits: &mut [RawHit],
    focus: (f64, f64),
    scale_m: f64,
    schema: &CrabSchema,
    docs: &[TantivyDocument],
) {
    let (flat, flng) = focus;
    for hit in hits.iter_mut() {
        let doc = &docs[hit.raw_index];
        if let (Some(lat), Some(lon)) = (
            doc.get_first(schema.lat).and_then(|v| v.as_f64()),
            doc.get_first(schema.lon).and_then(|v| v.as_f64()),
        ) {
            let d = spatial::haversine_m(flat, flng, lat, lon);
            let decay = (-d / scale_m).exp() as f32;
            hit.score *= decay;
        }
    }
    hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- parse_focus_point ------------------------------------------------

    #[test]
    fn parse_focus_point_accepts_belgian_centre() {
        // Brussels city centre.
        let p = parse_focus_point("50.8466,4.3525").unwrap();
        assert!((p.0 - 50.8466).abs() < 1e-9);
        assert!((p.1 - 4.3525).abs() < 1e-9);
    }

    #[test]
    fn parse_focus_point_tolerates_whitespace() {
        // Real-world callers often url-encode spaces; the parser is
        // intentionally permissive here.
        let p = parse_focus_point(" 50.8466 , 4.3525 ").unwrap();
        assert!((p.0 - 50.8466).abs() < 1e-9);
        assert!((p.1 - 4.3525).abs() < 1e-9);
    }

    #[test]
    fn parse_focus_point_rejects_reversed_lat_lng() {
        // 4.35 is a valid latitude in NW Europe? No — the bbox forces
        // lat ∈ [49, 52], so a swap of (lat, lng) → (lng, lat) gets
        // caught here.
        assert!(parse_focus_point("4.3525,50.8466").is_none());
    }

    #[test]
    fn parse_focus_point_rejects_extra_field() {
        assert!(parse_focus_point("50.8,4.3,99").is_none());
    }

    #[test]
    fn parse_focus_point_rejects_garbage() {
        assert!(parse_focus_point("not,a,point").is_none());
        assert!(parse_focus_point("50.8").is_none());
        assert!(parse_focus_point("").is_none());
    }

    #[test]
    fn parse_focus_point_rejects_out_of_belgium() {
        // Paris — lat valid, lng (2.35) inside bbox; this one passes
        // because the bbox is deliberately wide enough to handle
        // border cases (Lille users hitting Brussels-area searches).
        assert!(parse_focus_point("48.8566,2.3522").is_none()); // Paris lat too low
        assert!(parse_focus_point("55.0,4.0").is_none()); // Stockholm lat too high
        assert!(parse_focus_point("50.0,-1.0").is_none()); // English Channel west of bbox
        assert!(parse_focus_point("50.0,10.0").is_none()); // Germany east of bbox
    }

    // --- apply_focus_decay ------------------------------------------------
    //
    // apply_focus_decay needs real Tantivy docs to read lat/lon. We test the
    // math indirectly through the haversine module; the integration through
    // a Tantivy index is exercised by the /geocode endpoint tests in main.rs
    // (focus_point hides distant candidates).

    #[test]
    fn focus_half_life_constant_is_25km() {
        // Pinning the half-life constant: changing it without explicit
        // intent would silently shift ranking across Belgium-sized queries.
        // If you intentionally retune, update this assertion alongside the
        // design note in 04-online-search.md.
        assert_eq!(FOCUS_HALF_LIFE_M, 25_000.0);
    }
}
