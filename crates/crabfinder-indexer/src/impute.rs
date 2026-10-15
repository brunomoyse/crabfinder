//! Position imputation for addresses BeSt publishes without a position
//! (schema v9).
//!
//! BeSt writes the Lambert 72 point `0 0` for addresses whose position
//! hasn't been assigned yet: ~140k Walloon addresses in the 2026-10 dump.
//! Reprojected as-is they all land on one spot in the Oise (France), which
//! poisons `/geocode` results, `/reverse`, street medians and locality
//! centroids. `pipeline::stream_resolved_addresses` leaves such addresses
//! in Lambert 72; the indexer defers them here and, once every positioned
//! address has been streamed, gives each one the best guess available:
//!
//! 1. `SameNumber` — another unit with the same leading house number on the
//!    same street and postal code is positioned: the mean of those points.
//! 2. `Interpolated` — linear interpolation between the nearest lower and
//!    higher numbers on the same side of the street (any side when that
//!    side has none); the single nearest neighbour when only one side
//!    exists.
//! 3. `Street` — the street has positioned addresses but the house number
//!    has no leading number: the street's lower-median point.
//! 4. `Locality` — nothing on the street is positioned: the centroid of the
//!    part of municipality, else the municipality.
//!
//! Addresses with none of these (only possible on `--limit` smoke runs) are
//! dropped and counted.

use std::collections::BTreeMap;

use crabfinder_core::house_number::leading_number;
use crabfinder_core::{Address, Coordinates, PositionSource, WgsCoord};

use crate::localities::LocalitiesBuilder;
use crate::mid_street::{MidStreetBuilder, lower_median};

/// Outcome counts, for the indexer report.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ImputeReport {
    pub same_number: usize,
    pub interpolated: usize,
    pub street: usize,
    pub locality: usize,
    pub dropped: usize,
}

impl ImputeReport {
    pub fn imputed(&self) -> usize {
        self.same_number + self.interpolated + self.street + self.locality
    }

    fn count(&mut self, p: Option<PositionSource>) {
        match p {
            Some(PositionSource::SameNumber) => self.same_number += 1,
            Some(PositionSource::Interpolated) => self.interpolated += 1,
            Some(PositionSource::Street) => self.street += 1,
            Some(PositionSource::Locality) => self.locality += 1,
            Some(PositionSource::Measured) | None => self.dropped += 1,
        }
    }
}

impl std::fmt::Display for ImputeReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "same_number={} interpolated={} street={} locality={} dropped={}",
            self.same_number, self.interpolated, self.street, self.locality, self.dropped
        )
    }
}

/// Whether the pipeline left `addr` without a usable position.
pub fn needs_position(addr: &Address) -> bool {
    matches!(addr.coordinates, Coordinates::Lambert72(_))
}

/// Holds the deferred addresses until every positioned one is recorded.
#[derive(Default)]
pub struct PositionImputer {
    pending: Vec<Address>,
}

impl PositionImputer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn defer(&mut self, addr: Address) {
        self.pending.push(addr);
    }

    pub fn len(&self) -> usize {
        self.pending.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    /// Position every deferred address from the measured ones recorded in
    /// `streets` and `localities`. Returns the positioned addresses (WGS84,
    /// `position_source` set) and the counts; dropped addresses are not
    /// returned.
    pub fn impute(
        self,
        streets: &MidStreetBuilder,
        localities: &LocalitiesBuilder,
    ) -> (Vec<Address>, ImputeReport) {
        let mut report = ImputeReport::default();
        let mut out = Vec::with_capacity(self.pending.len());
        for mut addr in self.pending {
            let guess = impute_one(&addr, streets.points(&addr)).or_else(|| {
                localities
                    .centroid(&addr)
                    .map(|p| (PositionSource::Locality, p))
            });
            report.count(guess.map(|g| g.0));
            if let Some((source, (lat, lon))) = guess {
                addr.coordinates = Coordinates::Wgs84(WgsCoord { lat, lon });
                addr.position_source = source;
                out.push(addr);
            }
        }
        (out, report)
    }
}

/// Street-level guess from the positioned `(number, lat, lon)` points of the
/// address's street and postal code.
fn impute_one(addr: &Address, points: &[(u32, f64, f64)]) -> Option<(PositionSource, (f64, f64))> {
    if points.is_empty() {
        return None;
    }
    let Some(n) = leading_number(&addr.house_number) else {
        let (_, lat, lon) = lower_median(&mut points.to_vec());
        return Some((PositionSource::Street, (lat, lon)));
    };
    // Mean point per house number, ordered by number.
    let mut by_number: BTreeMap<u32, (f64, f64, u32)> = BTreeMap::new();
    for &(num, lat, lon) in points {
        let e = by_number.entry(num).or_insert((0.0, 0.0, 0));
        e.0 += lat;
        e.1 += lon;
        e.2 += 1;
    }
    let mean = |(lat, lon, k): (f64, f64, u32)| (lat / f64::from(k), lon / f64::from(k));
    if let Some(&p) = by_number.get(&n) {
        return Some((PositionSource::SameNumber, mean(p)));
    }
    let same_side: Vec<(u32, (f64, f64))> = by_number
        .iter()
        .filter(|(num, _)| *num % 2 == n % 2)
        .map(|(num, p)| (*num, mean(*p)))
        .collect();
    let side = if same_side.is_empty() {
        by_number.iter().map(|(num, p)| (*num, mean(*p))).collect()
    } else {
        same_side
    };
    let lower = side.iter().rev().find(|(num, _)| *num < n);
    let upper = side.iter().find(|(num, _)| *num > n);
    let point = match (lower, upper) {
        (Some(&(a, pa)), Some(&(b, pb))) => {
            let t = f64::from(n - a) / f64::from(b - a);
            (pa.0 + t * (pb.0 - pa.0), pa.1 + t * (pb.1 - pa.1))
        }
        (Some(&(_, p)), None) | (None, Some(&(_, p))) => p,
        // `side` is non-empty and holds no `n`, so one neighbour exists.
        (None, None) => unreachable!("non-empty side without neighbours"),
    };
    Some((PositionSource::Interpolated, point))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crabfinder_core::{
        AddressId, BestId, BuildingGroupId, LambertCoord, MultilingualName, MunicipalityId,
        PartOfMunicipalityId, PostalInfoId, Region, Status, StreetId, StreetType,
    };

    fn addr(street: &str, house: &str, pom: Option<&str>, pos: Option<(f64, f64)>) -> Address {
        let street_id = StreetId::new(BestId::new("ns-street", street, "1"));
        Address {
            id: AddressId::new(BestId::new("ns-addr", format!("{street}-{house}"), "1")),
            building_group_id: BuildingGroupId::from_parts(&street_id, house),
            street_id,
            municipality_id: MunicipalityId::new(BestId::new("ns-muni", "M1", "1")),
            postal_info_id: PostalInfoId::new(BestId::new("ns-postal", "5000", "1")),
            part_of_municipality_id: pom
                .map(|p| PartOfMunicipalityId::new(BestId::new("ns-pom", p, "1"))),
            region: Region::Wallonia,
            status: Status::Current,
            house_number: house.into(),
            box_number: None,
            officially_assigned: true,
            street_name: MultilingualName {
                fr: Some("Rue Test".into()),
                ..Default::default()
            },
            street_type: StreetType::Streetname,
            municipality_name: MultilingualName {
                fr: Some("Namur".into()),
                ..Default::default()
            },
            postal_code: "5000".into(),
            postal_name: MultilingualName::default(),
            part_of_municipality_name: pom.map(|p| MultilingualName {
                fr: Some(p.into()),
                ..Default::default()
            }),
            coordinates: match pos {
                Some((lat, lon)) => Coordinates::Wgs84(WgsCoord { lat, lon }),
                None => Coordinates::Lambert72(LambertCoord {
                    easting: 0.0,
                    northing: 0.0,
                }),
            },
            place_name: None,
            position_source: PositionSource::Measured,
        }
    }

    /// Rue Test: 2 @ (50.0, 4.0), 10 @ (50.8, 4.8), 2 again @ (50.2, 4.2),
    /// 5 @ (51.0, 5.0) — all in part of municipality P1.
    fn recorded() -> (MidStreetBuilder, LocalitiesBuilder) {
        let mut streets = MidStreetBuilder::new();
        let mut loc = LocalitiesBuilder::new();
        for (h, lat, lon) in [
            ("2", 50.0, 4.0),
            ("10", 50.8, 4.8),
            ("2", 50.2, 4.2),
            ("5", 51.0, 5.0),
        ] {
            let a = addr("S1", h, Some("P1"), Some((lat, lon)));
            streets.record(&a);
            loc.record(&a, None);
        }
        (streets, loc)
    }

    fn run(pending: Vec<Address>) -> (Vec<Address>, ImputeReport) {
        let (streets, loc) = recorded();
        let mut imp = PositionImputer::new();
        assert!(imp.is_empty());
        for a in pending {
            assert!(needs_position(&a));
            imp.defer(a);
        }
        imp.impute(&streets, &loc)
    }

    fn latlon(a: &Address) -> (f64, f64) {
        match a.coordinates {
            Coordinates::Wgs84(w) => (w.lat, w.lon),
            Coordinates::Lambert72(_) => panic!("still Lambert"),
        }
    }

    fn close(a: (f64, f64), b: (f64, f64)) -> bool {
        (a.0 - b.0).abs() < 1e-9 && (a.1 - b.1).abs() < 1e-9
    }

    #[test]
    fn same_number_takes_the_mean() {
        let (out, r) = run(vec![addr("S1", "2", Some("P1"), None)]);
        assert_eq!(r.same_number, 1);
        assert_eq!(out[0].position_source, PositionSource::SameNumber);
        assert!(close(latlon(&out[0]), (50.1, 4.1)));
    }

    #[test]
    fn interpolates_on_the_same_side() {
        // 6 lies between 2 (mean 50.1, 4.1) and 10 (50.8, 4.8): t = 0.5.
        // The odd 5 is ignored.
        let (out, r) = run(vec![addr("S1", "6B", Some("P1"), None)]);
        assert_eq!(r.interpolated, 1);
        assert_eq!(out[0].position_source, PositionSource::Interpolated);
        assert!(close(latlon(&out[0]), (50.45, 4.45)));
    }

    #[test]
    fn single_neighbour_and_other_side() {
        // 20: only the lower even neighbour 10 → its point.
        // 7: the odd side is just {5} → its point.
        let (out, r) = run(vec![
            addr("S1", "20", Some("P1"), None),
            addr("S1", "7", Some("P1"), None),
        ]);
        assert_eq!(r.interpolated, 2);
        assert!(close(latlon(&out[0]), (50.8, 4.8)));
        assert!(close(latlon(&out[1]), (51.0, 5.0)));
    }

    #[test]
    fn any_side_when_the_side_is_empty() {
        let mut streets = MidStreetBuilder::new();
        let loc = LocalitiesBuilder::new();
        streets.record(&addr("S2", "2", None, Some((50.0, 4.0))));
        streets.record(&addr("S2", "6", None, Some((50.4, 4.4))));
        let mut imp = PositionImputer::new();
        imp.defer(addr("S2", "3", None, None));
        assert_eq!(imp.len(), 1);
        let (out, _) = imp.impute(&streets, &loc);
        // Odd side empty → between 2 and 6 at t = 0.25.
        assert!(close(latlon(&out[0]), (50.1, 4.1)));
    }

    #[test]
    fn no_number_gets_the_street_median() {
        let (out, r) = run(vec![addr("S1", "ZN", Some("P1"), None)]);
        assert_eq!(r.street, 1);
        assert_eq!(out[0].position_source, PositionSource::Street);
        // Sorted numbers 2, 2, 5, 10 → index 1 → the second 2. Sort is
        // stable on equal numbers, so it is (50.2, 4.2).
        assert!(close(latlon(&out[0]), (50.2, 4.2)));
    }

    #[test]
    fn unknown_street_falls_back_to_the_locality_then_drops() {
        let (out, r) = run(vec![
            addr("S9", "1", Some("P1"), None),
            addr("S9", "1", Some("P-unknown"), None),
        ]);
        // P1 centroid = mean of the four recorded points. The second one
        // falls back to the municipality (same four points).
        let c = (
            (50.0 + 50.8 + 50.2 + 51.0) / 4.0,
            (4.0 + 4.8 + 4.2 + 5.0) / 4.0,
        );
        assert_eq!(r.locality, 2);
        assert!(close(latlon(&out[0]), c) && close(latlon(&out[1]), c));
        assert!(
            out.iter()
                .all(|a| a.position_source == PositionSource::Locality)
        );

        let mut imp = PositionImputer::new();
        imp.defer(addr("S9", "1", None, None));
        let (out, r) = imp.impute(&MidStreetBuilder::new(), &LocalitiesBuilder::new());
        assert!(out.is_empty());
        assert_eq!((r.dropped, r.imputed()), (1, 0));
    }

    #[test]
    fn report_display_and_totals() {
        let r = ImputeReport {
            same_number: 1,
            interpolated: 2,
            street: 3,
            locality: 4,
            dropped: 5,
        };
        assert_eq!(r.imputed(), 10);
        assert_eq!(
            r.to_string(),
            "same_number=1 interpolated=2 street=3 locality=4 dropped=5"
        );
        assert!(!needs_position(&addr("S1", "1", None, Some((50.0, 4.0)))));
    }
}
