//! In-memory R-tree spatial index keyed by Tantivy `DocAddress`.
//!
//! Built once at service startup from the existing Tantivy `lat` / `lon`
//! fast-field columns. The R-tree internally ranks by squared planar
//! distance on `(lon, lat)`; at Belgium's latitude (~50°N) that's not a
//! perfectly faithful proxy for the great-circle distance we report to the
//! user, so:
//!
//!   * we oversearch the R-tree (planar) by 2× the user's limit,
//!   * recompute Haversine distance on each candidate,
//!   * sort + filter + take N by Haversine.
//!
//! 2× is more than enough: at lat=50°N, 1° latitude is 111 km and 1°
//! longitude is ~71 km, ratio 1.56. Even at the corner cases the planar
//! ordering disagrees with Haversine, the disagreement is a swap of adjacent
//! candidates — oversearching by 2 catches them.

use std::sync::Arc;

use anyhow::{Context, Result};
use geo::{Distance, Haversine, Point};
use rstar::{AABB, PointDistance, RTree, RTreeObject};
use tantivy::{DocAddress, IndexReader};

use crabfinder_indexer::index::CrabSchema;

/// One indexed point. Cheap (24 bytes); ~840k of these is ~20MB raw.
#[derive(Debug, Clone, Copy)]
pub struct SpatialPoint {
    pub doc_addr: DocAddress,
    pub lat: f64,
    pub lon: f64,
}

impl RTreeObject for SpatialPoint {
    type Envelope = AABB<[f64; 2]>;
    fn envelope(&self) -> Self::Envelope {
        // Note ordering: `[lon, lat]` so that x corresponds to longitude
        // (the conventional GIS "x" axis). This is consistent with the
        // distance functions below.
        AABB::from_point([self.lon, self.lat])
    }
}

impl PointDistance for SpatialPoint {
    fn distance_2(&self, point: &[f64; 2]) -> f64 {
        let dx = self.lon - point[0];
        let dy = self.lat - point[1];
        dx * dx + dy * dy
    }
}

/// A single nearest-neighbour result.
#[derive(Debug, Clone, Copy)]
pub struct SpatialHit {
    pub doc_addr: DocAddress,
    /// Great-circle distance from the query point, in metres.
    pub distance_m: f64,
}

/// The in-memory spatial index.
pub struct SpatialIndex {
    tree: RTree<SpatialPoint>,
    point_count: usize,
}

impl SpatialIndex {
    /// Build the spatial index by scanning every live Tantivy doc once,
    /// reading `lat` / `lon` from the fast-field columns. Skips deleted
    /// (tombstoned) docs.
    pub fn build_from_tantivy(reader: &IndexReader, schema: &CrabSchema) -> Result<Self> {
        let _ = schema; // we use field names not handles for fast-field lookup
        let searcher = reader.searcher();
        let mut points: Vec<SpatialPoint> = Vec::with_capacity(searcher.num_docs() as usize);

        for (seg_ord, seg_reader) in searcher.segment_readers().iter().enumerate() {
            let fast_fields = seg_reader.fast_fields();
            let lat_col = fast_fields
                .f64("lat")
                .context("reading lat fast-field column")?;
            let lon_col = fast_fields
                .f64("lon")
                .context("reading lon fast-field column")?;
            let max_doc = seg_reader.max_doc();
            let alive = seg_reader.alive_bitset();
            for doc_id in 0..max_doc {
                if let Some(b) = alive {
                    if !b.is_alive(doc_id) {
                        continue;
                    }
                }
                let lat = match lat_col.first(doc_id) {
                    Some(v) => v,
                    None => continue,
                };
                let lon = match lon_col.first(doc_id) {
                    Some(v) => v,
                    None => continue,
                };
                points.push(SpatialPoint {
                    doc_addr: DocAddress::new(seg_ord as u32, doc_id),
                    lat,
                    lon,
                });
            }
        }

        let point_count = points.len();
        let tree = RTree::bulk_load(points);
        Ok(Self { tree, point_count })
    }

    /// Construct from raw points — exposed for tests so they don't need a
    /// Tantivy index just to exercise the spatial logic.
    #[cfg(test)]
    pub fn from_points(points: Vec<SpatialPoint>) -> Self {
        let point_count = points.len();
        let tree = RTree::bulk_load(points);
        Self { tree, point_count }
    }

    pub fn point_count(&self) -> usize {
        self.point_count
    }

    /// Return up to `limit` points sorted by ascending Haversine distance.
    /// If `radius_m` is set, only points within that radius are returned.
    ///
    /// The R-tree iterates by squared planar distance on (lon, lat). We
    /// oversearch by 2× to catch the ~1.5× lat/lon scale mismatch at
    /// Belgium's latitude, then resort by exact Haversine.
    pub fn nearest(&self, lat: f64, lon: f64, limit: usize, radius_m: Option<f64>) -> Vec<SpatialHit> {
        if limit == 0 || self.point_count == 0 {
            return Vec::new();
        }

        // Conservative planar cap for early termination when a radius is set.
        // 1m ≈ 1/(111320 * cos(lat))° in the worst direction (longitude).
        // Convert radius_m to that worst-case-degree value, square it, then
        // add a generous 4× buffer (planar/Haversine skew + numerical).
        let planar_cap_2 = radius_m.map(|r| {
            let cos_lat = lat.to_radians().cos().abs().max(0.1);
            let deg = r / (111_320.0 * cos_lat);
            let buffered = deg * 2.0;
            buffered * buffered
        });

        let mut candidates: Vec<SpatialHit> = Vec::new();
        let oversearch_target = if radius_m.is_some() {
            // For radius queries we keep pulling until we exceed the planar
            // cap; the cap bounds the work.
            usize::MAX
        } else {
            // Unbounded: 2× oversearch is enough to correct planar ordering.
            (limit * 2).max(8)
        };

        for point in self.tree.nearest_neighbor_iter(&[lon, lat]) {
            // Early-exit by planar cap for radius queries.
            if let Some(cap2) = planar_cap_2 {
                if point.distance_2(&[lon, lat]) > cap2 {
                    break;
                }
            }
            let d_m = haversine_m(lat, lon, point.lat, point.lon);
            if let Some(r) = radius_m {
                if d_m > r {
                    // Inside planar buffer but outside true radius — skip,
                    // don't break (the iterator is planar-ordered, not
                    // Haversine-ordered).
                    continue;
                }
            }
            candidates.push(SpatialHit {
                doc_addr: point.doc_addr,
                distance_m: d_m,
            });
            if candidates.len() >= oversearch_target {
                break;
            }
        }

        // Re-sort by Haversine. R-tree gave us planar order; the two agree
        // up to ~1.5× skew at Belgium's latitude, so a final sort is the
        // honest way to express "nearest by great-circle".
        candidates.sort_by(|a, b| {
            a.distance_m
                .partial_cmp(&b.distance_m)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        candidates.truncate(limit);
        candidates
    }
}

/// Haversine great-circle distance in metres. Delegates to `geo::Haversine`,
/// which uses the IUGG mean Earth radius (6 371 008.7714 m). The previous
/// hand-coded version rounded to 6 371 000 m; the ~1.4 ppm shift is below
/// the resolution of BeSt address coordinates and well inside the
/// oversearch buffer that compensates for planar/Haversine skew in
/// `SpatialIndex::nearest`.
pub fn haversine_m(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    Haversine.distance(Point::new(lon1, lat1), Point::new(lon2, lat2))
}

/// `Arc<SpatialIndex>` alias for the service `AppState`.
pub type SharedSpatialIndex = Arc<SpatialIndex>;

#[cfg(test)]
mod tests {
    use super::*;

    fn point(seg: u32, doc: u32, lat: f64, lon: f64) -> SpatialPoint {
        SpatialPoint {
            doc_addr: DocAddress::new(seg, doc),
            lat,
            lon,
        }
    }

    #[test]
    fn haversine_known_distances() {
        // London (51.5074, -0.1278) to Paris (48.8566, 2.3522) ≈ 343.5 km.
        let d = haversine_m(51.5074, -0.1278, 48.8566, 2.3522);
        assert!(
            (d - 343_500.0).abs() < 1_000.0,
            "London→Paris should be ~343km, got {d}m"
        );
    }

    #[test]
    fn haversine_zero_for_identical_points() {
        let d = haversine_m(50.8467, 4.3525, 50.8467, 4.3525);
        assert!(d < 1e-6);
    }

    #[test]
    fn haversine_short_distance() {
        // ~100m apart at Brussels (~1° lat = 111km, so 0.001° = 111m).
        let d = haversine_m(50.8467, 4.3525, 50.8477, 4.3525);
        assert!((d - 111.0).abs() < 1.0, "should be ~111m, got {d}m");
    }

    #[test]
    fn empty_tree_returns_empty() {
        let idx = SpatialIndex::from_points(vec![]);
        let hits = idx.nearest(50.8467, 4.3525, 5, None);
        assert!(hits.is_empty());
    }

    #[test]
    fn nearest_finds_the_closest_point() {
        let pts = vec![
            point(0, 0, 50.8467, 4.3525), // Grand-Place
            point(0, 1, 50.8345, 4.3567), // Avenue Louise 32 (~1.4km)
            point(0, 2, 50.8950, 4.3416), // Atomium (~5.4km)
        ];
        let idx = SpatialIndex::from_points(pts);
        let hits = idx.nearest(50.8467, 4.3525, 1, None);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].doc_addr.doc_id, 0);
        assert!(hits[0].distance_m < 1.0);
    }

    #[test]
    fn nearest_returns_n_in_distance_order() {
        let pts = vec![
            point(0, 0, 50.8950, 4.3416), // Atomium ~5.4km from query
            point(0, 1, 50.8345, 4.3567), // Avenue Louise 32 ~1.4km
            point(0, 2, 50.8467, 4.3525), // Grand-Place (the query point)
        ];
        let idx = SpatialIndex::from_points(pts);
        let hits = idx.nearest(50.8467, 4.3525, 3, None);
        assert_eq!(hits.len(), 3);
        // Monotonic non-decreasing distance.
        for w in hits.windows(2) {
            assert!(w[0].distance_m <= w[1].distance_m);
        }
        // First is the query point itself.
        assert_eq!(hits[0].doc_addr.doc_id, 2);
    }

    #[test]
    fn radius_filters_by_haversine_not_planar() {
        let pts = vec![
            point(0, 0, 50.8467, 4.3525),
            point(0, 1, 50.8567, 4.3525), // ~1.1km north
            point(0, 2, 50.8467, 4.3625), // ~700m east (cos(50)*111km*0.01)
        ];
        let idx = SpatialIndex::from_points(pts);

        let hits_in_500m = idx.nearest(50.8467, 4.3525, 10, Some(500.0));
        assert_eq!(hits_in_500m.len(), 1, "only the query point itself is <500m");

        let hits_in_2000m = idx.nearest(50.8467, 4.3525, 10, Some(2000.0));
        assert_eq!(hits_in_2000m.len(), 3, "all three within 2km");
    }

    #[test]
    fn limit_truncates_after_sort() {
        let pts: Vec<SpatialPoint> = (0..20)
            .map(|i| point(0, i, 50.8467 + (i as f64) * 0.0001, 4.3525))
            .collect();
        let idx = SpatialIndex::from_points(pts);
        let hits = idx.nearest(50.8467, 4.3525, 5, None);
        assert_eq!(hits.len(), 5);
        // Closest first.
        assert_eq!(hits[0].doc_addr.doc_id, 0);
        assert_eq!(hits[4].doc_addr.doc_id, 4);
    }

    #[test]
    fn out_of_region_query_returns_nearest_when_no_radius() {
        // Brussels points only; query from Antwerp.
        let pts = vec![
            point(0, 0, 50.8467, 4.3525), // Grand-Place (≈40km from Antwerp)
            point(0, 1, 50.8345, 4.3567), // Avenue Louise (≈42km)
        ];
        let idx = SpatialIndex::from_points(pts);
        // Antwerp ≈ 51.2194, 4.4025
        let hits = idx.nearest(51.2194, 4.4025, 1, None);
        assert_eq!(hits.len(), 1);
        assert!(hits[0].distance_m > 30_000.0, "Antwerp→Brussels should be >30km");
        assert!(hits[0].distance_m < 50_000.0, "Antwerp→Brussels should be <50km");
    }

    #[test]
    fn out_of_region_query_with_small_radius_returns_empty() {
        let pts = vec![point(0, 0, 50.8467, 4.3525)];
        let idx = SpatialIndex::from_points(pts);
        // Antwerp with 5km radius — Brussels is way out.
        let hits = idx.nearest(51.2194, 4.4025, 5, Some(5_000.0));
        assert!(hits.is_empty());
    }

    #[test]
    fn radius_zero_returns_only_exact_matches() {
        let pts = vec![point(0, 0, 50.8467, 4.3525)];
        let idx = SpatialIndex::from_points(pts);
        let hits = idx.nearest(50.8467, 4.3525, 5, Some(0.0));
        // Exact same point → Haversine == 0 → included.
        assert_eq!(hits.len(), 1);
    }
}
