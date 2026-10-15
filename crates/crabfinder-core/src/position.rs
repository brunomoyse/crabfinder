//! Where an address's coordinates come from (schema v9).
//!
//! BeSt ships ~140k Walloon addresses with the Lambert 72 position `0 0`
//! (no position assigned yet). Reprojected as-is they land in the Oise, in
//! France. The indexer imputes a position for them instead (see
//! `crabfinder_indexer::impute`) and records how in `PositionSource`.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PositionSource {
    /// The position published in BeSt.
    #[default]
    Measured,
    /// Another unit with the same house number on the same street and
    /// postal code has a position; this address shares it.
    SameNumber,
    /// Interpolated between the nearest numbered neighbours on the same
    /// street and postal code (same side of the street when possible).
    Interpolated,
    /// The street has positioned addresses but none usable for the
    /// interpolation (no leading number): its median-number position.
    Street,
    /// Nothing on the street is positioned: the centroid of the address's
    /// part-of-municipality, else its municipality.
    Locality,
}

impl PositionSource {
    pub fn is_measured(&self) -> bool {
        *self == Self::Measured
    }

    /// Precise enough to place the address itself (reverse geocoding,
    /// locality centroids). `Street` and `Locality` only place the area.
    pub fn is_point_accurate(self) -> bool {
        matches!(self, Self::Measured | Self::SameNumber | Self::Interpolated)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Measured => "measured",
            Self::SameNumber => "same_number",
            Self::Interpolated => "interpolated",
            Self::Street => "street",
            Self::Locality => "locality",
        }
    }

    /// Dense code for the FAST index field.
    pub fn code(self) -> u64 {
        match self {
            Self::Measured => 0,
            Self::SameNumber => 1,
            Self::Interpolated => 2,
            Self::Street => 3,
            Self::Locality => 4,
        }
    }

    /// Inverse of `code`; unknown codes read as `Measured`.
    pub fn from_code(c: u64) -> Self {
        match c {
            1 => Self::SameNumber,
            2 => Self::Interpolated,
            3 => Self::Street,
            4 => Self::Locality,
            _ => Self::Measured,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [PositionSource; 5] = [
        PositionSource::Measured,
        PositionSource::SameNumber,
        PositionSource::Interpolated,
        PositionSource::Street,
        PositionSource::Locality,
    ];

    #[test]
    fn codes_round_trip_and_strings_match_serde() {
        for p in ALL {
            assert_eq!(PositionSource::from_code(p.code()), p);
            assert_eq!(
                serde_json::to_value(p).unwrap(),
                serde_json::Value::String(p.as_str().into())
            );
        }
        assert_eq!(PositionSource::from_code(99), PositionSource::Measured);
        assert_eq!(PositionSource::default(), PositionSource::Measured);
    }

    #[test]
    fn accuracy_classes() {
        assert!(PositionSource::Measured.is_measured());
        assert!(!PositionSource::Street.is_measured());
        let accurate: Vec<_> = ALL.into_iter().filter(|p| p.is_point_accurate()).collect();
        assert_eq!(accurate.len(), 3);
        assert!(!PositionSource::Locality.is_point_accurate());
    }
}
