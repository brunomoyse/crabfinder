//! Reverse house-number interpolation (v2 Phase 10): `/reverse?…&
//! interpolate=true` estimates the number a point would carry on the
//! nearest street when it falls in a gap between two known addresses
//! (a vacant plot, a new building BeSt doesn't list yet).
//!
//! Pure geometry over "anchors" — the known numbers of one street side
//! near the point — so it is unit-testable without an index; `http::
//! reverse` gathers the anchors from the R-tree.

/// One known address on the street: its leading number and position.
#[derive(Debug, Clone, PartialEq)]
pub struct Anchor {
    pub number: u32,
    /// The house number as published (`"11"`, `"25B"`).
    pub house_number: String,
    pub lat: f64,
    pub lon: f64,
}

/// The estimate: `house_number` lies between `before` and `after`, at
/// `(lat, lon)` on the segment joining them, `distance_m` from the point.
#[derive(Debug, Clone, PartialEq)]
pub struct Estimate {
    pub house_number: u32,
    pub before: String,
    pub after: String,
    pub lat: f64,
    pub lon: f64,
    pub distance_m: f64,
}

/// Farthest the point may lie from the segment between two anchors
/// (the street width plus front gardens).
pub const MAX_OFFSET_M: f64 = 60.0;

/// Longest segment interpolated over: farther apart, the numbering isn't
/// regular enough to guess.
pub const MAX_SEGMENT_M: f64 = 400.0;

/// Planar metres east / north of `(lat0, lon0)` (equirectangular; exact
/// enough over a few hundred metres).
fn local_xy(lat0: f64, lon0: f64, lat: f64, lon: f64) -> (f64, f64) {
    let x = (lon - lon0).to_radians() * lat0.to_radians().cos() * 6_371_000.0;
    let y = (lat - lat0).to_radians() * 6_371_000.0;
    (x, y)
}

/// The number at `(lat, lon)` between two consecutive anchors of the
/// same parity as `parity_of` (the nearest address: its side of the
/// street) whose numbers leave a gap (`after − before > 2`), on the
/// segment nearest to the point.
///
/// `None` when the point projects outside every such segment, lies more
/// than `MAX_OFFSET_M` off it, or the segment exceeds `MAX_SEGMENT_M`;
/// also when the estimate rounds onto one of the two anchors — then the
/// nearest address already is the answer.
pub fn interpolate(lat: f64, lon: f64, parity_of: u32, anchors: &[Anchor]) -> Option<Estimate> {
    let mut side: Vec<&Anchor> = anchors
        .iter()
        .filter(|a| a.number % 2 == parity_of % 2)
        .collect();
    side.sort_by(|a, b| {
        a.number
            .cmp(&b.number)
            .then(a.house_number.cmp(&b.house_number))
    });
    side.dedup_by_key(|a| a.number);

    let mut best: Option<Estimate> = None;
    for pair in side.windows(2) {
        let (p, q) = (pair[0], pair[1]);
        if q.number - p.number <= 2 {
            continue;
        }
        let (qx, qy) = local_xy(p.lat, p.lon, q.lat, q.lon);
        let (x, y) = local_xy(p.lat, p.lon, lat, lon);
        let len2 = qx * qx + qy * qy;
        if len2 == 0.0 || len2.sqrt() > MAX_SEGMENT_M {
            continue;
        }
        let t = (x * qx + y * qy) / len2;
        if !(0.0..=1.0).contains(&t) {
            continue;
        }
        let offset = ((x - t * qx).powi(2) + (y - t * qy).powi(2)).sqrt();
        if offset > MAX_OFFSET_M || best.as_ref().is_some_and(|b| b.distance_m <= offset) {
            continue;
        }
        let raw = p.number as f64 + t * (q.number - p.number) as f64;
        let steps = ((raw - p.number as f64) / 2.0).round() as u32;
        let n = p.number + 2 * steps;
        if n <= p.number || n >= q.number {
            continue;
        }
        // Place the estimate where its number falls on the segment.
        let s = (n - p.number) as f64 / (q.number - p.number) as f64;
        best = Some(Estimate {
            house_number: n,
            before: p.house_number.clone(),
            after: q.house_number.clone(),
            lat: p.lat + s * (q.lat - p.lat),
            lon: p.lon + s * (q.lon - p.lon),
            distance_m: offset,
        });
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Odd numbers along a west–east street at lat 50.85: 1 at lon 4.35,
    /// then one metre east per unit of number (1e-5° ≈ 0.70 m).
    fn a(n: u32, lon_offset_m: f64) -> Anchor {
        Anchor {
            number: n,
            house_number: n.to_string(),
            lat: 50.85,
            lon: 4.35 + lon_offset_m / 70_200.0,
        }
    }

    fn at(m: f64) -> f64 {
        4.35 + m / 70_200.0
    }

    #[test]
    fn a_point_in_a_gap_gets_the_proportional_number() {
        // 11 at 0 m, 31 at 100 m: 40 m in → 11 + 0.4 × 20 = 19.
        let anchors = [a(11, 0.0), a(31, 100.0), a(33, 110.0)];
        let e = interpolate(50.85005, at(40.0), 11, &anchors).unwrap();
        assert_eq!(e.house_number, 19);
        assert_eq!((e.before.as_str(), e.after.as_str()), ("11", "31"));
        assert!((e.distance_m - 5.6).abs() < 0.5, "{}", e.distance_m);
        assert!((e.lon - at(40.0)).abs() < 1e-6);
        // Rounds to the anchors' parity.
        let e = interpolate(50.85, at(44.0), 11, &anchors).unwrap();
        assert_eq!(e.house_number, 19);
        let e = interpolate(50.85, at(46.0), 11, &anchors).unwrap();
        assert_eq!(e.house_number, 21);
    }

    #[test]
    fn only_the_nearest_addresss_side_counts() {
        // Even numbers 2…42 across the street must not be mixed in.
        let anchors = [a(11, 0.0), a(31, 100.0), a(2, 0.0), a(42, 100.0)];
        assert_eq!(
            interpolate(50.85, at(50.0), 11, &anchors)
                .unwrap()
                .house_number,
            21
        );
        assert_eq!(
            interpolate(50.85, at(50.0), 2, &anchors)
                .unwrap()
                .house_number,
            22
        );
    }

    #[test]
    fn no_estimate_without_a_gap_or_off_the_segment() {
        let close = [a(11, 0.0), a(13, 10.0)];
        assert_eq!(interpolate(50.85, at(5.0), 11, &close), None);
        let gap = [a(11, 0.0), a(31, 100.0)];
        // Beyond either end.
        assert_eq!(interpolate(50.85, at(-10.0), 11, &gap), None);
        assert_eq!(interpolate(50.85, at(120.0), 11, &gap), None);
        // Too far from the street (~110 m north).
        assert_eq!(interpolate(50.851, at(50.0), 11, &gap), None);
        // Next to an anchor: it is the answer.
        assert_eq!(interpolate(50.85, at(1.0), 11, &gap), None);
        // A segment too long to trust.
        let long = [a(11, 0.0), a(31, 1_000.0)];
        assert_eq!(interpolate(50.85, at(500.0), 11, &long), None);
        // Duplicated positions / numbers.
        let same = [a(11, 0.0), a(11, 0.0), a(31, 0.0)];
        assert_eq!(interpolate(50.85, at(0.0), 11, &same), None);
        assert_eq!(interpolate(50.85, at(0.0), 11, &[]), None);
    }

    #[test]
    fn the_nearest_segment_wins() {
        // Two gaps: 11–31 along lat 50.85, 51–71 along a parallel street
        // ~55 m north (a bend) — the point is nearer the first.
        let mut far = [a(51, 0.0), a(71, 100.0)];
        for x in &mut far {
            x.lat = 50.8505;
        }
        let anchors = [a(11, 0.0), a(31, 100.0), far[0].clone(), far[1].clone()];
        let e = interpolate(50.85010, at(50.0), 11, &anchors).unwrap();
        assert_eq!(e.house_number, 21);
        let e = interpolate(50.85045, at(50.0), 11, &anchors).unwrap();
        assert_eq!(e.house_number, 61);
    }
}
