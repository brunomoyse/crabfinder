use serde::{Deserialize, Serialize};

/// EPSG:31370 (Belgian Lambert 72). Easting/northing in metres.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LambertCoord {
    pub easting: f64,
    pub northing: f64,
}

/// EPSG:4326 (WGS84). Geographic lat/lon in degrees.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WgsCoord {
    pub lat: f64,
    pub lon: f64,
}

/// A coordinate with its CRS made explicit. Per file 11 §3, every BeSt XML
/// position is Lambert 72; crabfinder reprojects to WGS84 at index time.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "crs", rename_all = "lowercase")]
pub enum Coordinates {
    #[serde(rename = "EPSG:31370")]
    Lambert72(LambertCoord),
    #[serde(rename = "EPSG:4326")]
    Wgs84(WgsCoord),
}

impl WgsCoord {
    /// Sanity-check that a coordinate falls inside Belgium's bounding box.
    /// Belgium WGS84 envelope (approx): lat ∈ [49.4, 51.6], lon ∈ [2.5, 6.5].
    pub fn is_in_belgium(&self) -> bool {
        (49.4..=51.6).contains(&self.lat) && (2.5..=6.5).contains(&self.lon)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grand_place_is_in_belgium() {
        let p = WgsCoord {
            lat: 50.8467,
            lon: 4.3525,
        };
        assert!(p.is_in_belgium());
    }

    #[test]
    fn paris_is_not_in_belgium() {
        let p = WgsCoord {
            lat: 48.8566,
            lon: 2.3522,
        };
        assert!(!p.is_in_belgium());
    }
}
