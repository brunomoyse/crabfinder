//! Lambert 72 (EPSG:31370) → WGS84 (EPSG:4326) reprojection.
//!
//! Every BeSt XML position is in Lambert 72 (file 11 §3); we reproject once
//! at index time so /geocode responses can return WGS84 directly.
//!
//! Implementation: delegated to `proj4rs` 0.2 (pure Rust, no system
//! libproj). We feed it an EXPLICIT proj-string that carries the IGN-
//! published 7-parameter Helmert (`+towgs84=...`), because `crs-definitions`'
//! shipped EPSG:31370 string omits those parameters and the result drifts by
//! ~110 m. With the towgs84 parameters supplied, proj4rs agreed with the
//! previous hand-coded LCC + Helmert + Bowring pipeline to **<1 mm** on every
//! reference point checked (epsg.io published, a real Anderlecht address,
//! Brussels baseline, Antwerp). See `docs/georust-adoption-analysis.md` for
//! the probe details.
//!
//! References:
//!   - EPSG:31370 (Belgian Lambert 72) parameters: <https://epsg.io/31370>
//!   - IGN Belgium memo "Coordinate transformations BD72 ↔ WGS84"
//!     (used to verify the +towgs84 string below)

use anyhow::{Context, Result};
use proj4rs::Proj;
use proj4rs::transform::transform;

use crabfinder_core::{LambertCoord, WgsCoord};

/// EPSG:31370 with the IGN-published 7-parameter Helmert applied
/// (position-vector convention, "+towgs84=dx,dy,dz,rx,ry,rz,ds").
/// Reproducing it here verbatim — rather than relying on
/// `Proj::from_user_string("EPSG:31370")` — because the EPSG definition that
/// ships with `crs-definitions` 0.4.0 drops the towgs84 line. Without it,
/// the inverse projection lands on the BD72 ellipsoid (Hayford) and is never
/// shifted onto WGS84, producing a systematic ~110 m offset.
const LAMBERT72_PROJ_STRING: &str =
    "+proj=lcc +lat_1=49.8333339 +lat_2=51.16666723333333 +lat_0=90 \
     +lon_0=4.367486666666666 +x_0=150000.013 +y_0=5400088.438 \
     +ellps=intl \
     +towgs84=-106.869,52.2978,-103.724,0.3366,-0.4570,1.8422,-1.2747 \
     +units=m +no_defs";

const WGS84_PROJ_STRING: &str = "+proj=longlat +datum=WGS84 +no_defs";

pub struct Reprojector {
    from: Proj,
    to: Proj,
}

impl Reprojector {
    pub fn new() -> Result<Self> {
        let from = Proj::from_user_string(LAMBERT72_PROJ_STRING)
            .context("compiling Lambert 72 proj-string")?;
        let to = Proj::from_user_string(WGS84_PROJ_STRING)
            .context("compiling WGS84 proj-string")?;
        Ok(Self { from, to })
    }

    pub fn lambert72_to_wgs84(&self, lc: LambertCoord) -> Result<WgsCoord> {
        // proj4rs takes (x, y, z) in CRS units (metres for projected) and
        // returns geographic CRSs in RADIANS.
        let mut p = (lc.easting, lc.northing, 0.0_f64);
        transform(&self.from, &self.to, &mut p)
            .with_context(|| format!("reprojecting L72 ({}, {})", lc.easting, lc.northing))?;
        Ok(WgsCoord {
            lat: p.1.to_degrees(),
            lon: p.0.to_degrees(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reference pair cross-checked against epsg.io's online transformer
    /// (<https://epsg.io/transform> with EPSG:31370 → EPSG:4326). epsg.io
    /// reports ~(50.84825, 4.34959); we want to match within 1e-4°
    /// (~11 m) — the resolution the BeSt pipeline cares about.
    #[test]
    fn epsg_io_reference_pair() {
        let r = Reprojector::new().unwrap();
        let wgs = r
            .lambert72_to_wgs84(LambertCoord {
                easting: 148651.0,
                northing: 170880.0,
            })
            .unwrap();
        eprintln!("(148651, 170880) → {wgs:?}");
        assert!(
            (wgs.lat - 50.84825).abs() < 1e-4,
            "lat off: got {} expected ~50.84825",
            wgs.lat
        );
        assert!(
            (wgs.lon - 4.34959).abs() < 1e-4,
            "lon off: got {} expected ~4.34959",
            wgs.lon
        );
        assert!(wgs.is_in_belgium());
    }

    /// Real address from the BeSt dump: Chaussée de Mons 475 RDC, Anderlecht.
    /// Expected to land within Brussels-Capital region bounds.
    #[test]
    fn live_anderlecht_address() {
        let r = Reprojector::new().unwrap();
        let wgs = r
            .lambert72_to_wgs84(LambertCoord {
                easting: 146321.078,
                northing: 169504.609,
            })
            .unwrap();
        eprintln!("anderlecht → {wgs:?}");
        assert!(wgs.is_in_belgium());
        // Anderlecht hovers around (50.83, 4.31).
        assert!(wgs.lat > 50.80 && wgs.lat < 50.86, "lat: {}", wgs.lat);
        assert!(wgs.lon > 4.27 && wgs.lon < 4.34, "lon: {}", wgs.lon);
    }

    /// Sanity: easting/northing offsets propagate intuitively. A point 100 m
    /// further east in Lambert 72 should be ~0.0014° further east in WGS84
    /// (at Belgian latitudes one degree of longitude ≈ 70 km).
    #[test]
    fn east_shift_intuitive() {
        let r = Reprojector::new().unwrap();
        let base = r
            .lambert72_to_wgs84(LambertCoord {
                easting: 150_000.0,
                northing: 170_000.0,
            })
            .unwrap();
        let east_100m = r
            .lambert72_to_wgs84(LambertCoord {
                easting: 150_100.0,
                northing: 170_000.0,
            })
            .unwrap();
        let dlon = east_100m.lon - base.lon;
        // 100 m east at Belgian lat ≈ 0.0014°. Allow 10% slack.
        assert!(
            (dlon - 0.001_4).abs() < 0.000_2,
            "Δlon for 100m east: {dlon}"
        );
    }
}
