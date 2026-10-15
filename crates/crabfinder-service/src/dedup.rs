//! Building-level dedup of raw Tantivy hits.
//!
//! Phase 3: collapse hits sharing a `building_group_id` into one Group whose
//! score = `max(member.score)` and whose match_type = `strict` if any member
//! matched strict, else `fuzzy`. Representative unit = highest-scoring
//! member, ties broken by `box_number` ascending (None first), then
//! `address_id` ascending — deterministic and stable across calls.
//!
//! Pure function, no Tantivy types. The service-layer wrapper does the doc
//! lookups and feeds raw `RawHit`s in, dedup decides which to surface.

use std::collections::HashMap;

use tantivy::{TantivyDocument, schema::Value};

use crabfinder_indexer::index::CrabSchema;

/// Over-fetch multiplier: dedup happens after Tantivy ranking, so callers
/// fetch `limit * RAW_HIT_MULTIPLIER` raw hits, then collapse. With
/// Brussels' per-unit modeling a single building can have 20+ units; 20×
/// gives enough headroom for limit ≤ 100 while staying well inside
/// Tantivy's cheap range.
pub const RAW_HIT_MULTIPLIER: usize = 20;
/// Hard ceiling on the over-fetch.
pub const RAW_HIT_CAP: usize = 2000;

/// Number of raw hits to fetch so that `limit` distinct buildings usually
/// survive dedup. `expand` callers want raw rows, so no over-fetch.
pub fn raw_limit(limit: usize, expand: bool) -> usize {
    if expand {
        limit
    } else {
        (limit * RAW_HIT_MULTIPLIER).min(RAW_HIT_CAP)
    }
}

/// One raw Tantivy hit, already enriched with the fields dedup cares about.
#[derive(Debug, Clone, PartialEq)]
pub struct RawHit {
    pub score: f32,
    pub building_group_id: String,
    /// `Some` for normal records; `None` for legacy records that lack a box.
    /// `None` sorts first so "the base record" represents the building when
    /// it exists.
    pub box_number: Option<String>,
    /// Canonical `BestId` string (`{namespace}/{oid}/{ver}`). Used only as a
    /// tie-breaker so the representative is deterministic.
    pub address_id: String,
    pub match_type: MatchType,
    /// Index into the original Vec<(score, DocAddress)> so the caller can
    /// re-hydrate the right Tantivy doc without re-doing the search.
    pub raw_index: usize,
}

impl RawHit {
    /// Read the dedup-relevant fields (building group, box, address id) off
    /// a stored Tantivy doc. Missing fields degrade to empty strings / `None`
    /// rather than erroring — dedup only needs them for grouping and
    /// tie-breaking.
    pub fn from_doc(
        doc: &TantivyDocument,
        schema: &CrabSchema,
        score: f32,
        match_type: MatchType,
        raw_index: usize,
    ) -> Self {
        let text = |f| doc.get_first(f).and_then(|v| v.as_str()).map(String::from);
        Self {
            score,
            building_group_id: text(schema.building_group_id).unwrap_or_default(),
            box_number: text(schema.box_number),
            address_id: text(schema.best_id).unwrap_or_default(),
            match_type,
            raw_index,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchType {
    Strict,
    Fuzzy,
    /// Result came from /reverse (spatial nearest-neighbour), not text search.
    /// Carried so clients can rely on `match_type` always being present
    /// regardless of which endpoint produced the feature.
    Spatial,
}

impl MatchType {
    pub fn as_str(self) -> &'static str {
        match self {
            MatchType::Strict => "strict",
            MatchType::Fuzzy => "fuzzy",
            MatchType::Spatial => "spatial",
        }
    }
}

/// One deduplicated building.
#[derive(Debug, Clone)]
pub struct Group {
    pub building_group_id: String,
    /// `max(member.score)` — see design note for justification.
    pub max_score: f32,
    pub unit_count: u32,
    pub match_type: MatchType,
    /// Index into the input Vec of the chosen representative — caller uses
    /// this to hydrate the right Tantivy doc.
    pub representative_index: usize,
}

/// Collapse raw hits into groups, sorted by `max_score` desc.
///
/// Preserves the relative order of equal-scored groups by the order their
/// first member appeared in the input — this keeps results stable across
/// calls with identical input.
pub fn group_by_building(hits: &[RawHit]) -> Vec<Group> {
    // First pass: bucket by building_group_id, remember insertion order.
    let mut buckets: HashMap<&str, Vec<&RawHit>> = HashMap::new();
    let mut order: Vec<&str> = Vec::new();
    for h in hits {
        let key: &str = h.building_group_id.as_str();
        if !buckets.contains_key(key) {
            order.push(key);
        }
        buckets.entry(key).or_default().push(h);
    }

    // Second pass: build Group per bucket.
    let mut groups: Vec<Group> = Vec::with_capacity(order.len());
    for key in order {
        let members: &Vec<&RawHit> = buckets.get(key).expect("bucket present");
        let max_score = members
            .iter()
            .map(|m| m.score)
            .fold(f32::NEG_INFINITY, f32::max);
        // Strict beats Fuzzy beats Spatial. Spatial is only the answer when
        // every member is Spatial — text-search hits never produce Spatial,
        // and /reverse hits never produce Strict/Fuzzy.
        let match_type = if members.iter().any(|m| m.match_type == MatchType::Strict) {
            MatchType::Strict
        } else if members.iter().any(|m| m.match_type == MatchType::Fuzzy) {
            MatchType::Fuzzy
        } else {
            MatchType::Spatial
        };
        let rep = pick_representative(members);
        groups.push(Group {
            building_group_id: key.to_string(),
            max_score,
            unit_count: members.len() as u32,
            match_type,
            representative_index: rep.raw_index,
        });
    }

    // Third pass: sort by max_score descending. Sort is stable, so ties
    // preserve the insertion order set in pass 1.
    groups.sort_by(|a, b| {
        b.max_score
            .partial_cmp(&a.max_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    groups
}

/// Pick the representative unit for a group:
///   1. highest score
///   2. tie-break: box_number ascending, None first
///   3. tie-break: address_id ascending (canonical Display form)
fn pick_representative<'a>(members: &[&'a RawHit]) -> &'a RawHit {
    let mut best: &'a RawHit = members[0];
    for m in &members[1..] {
        if rep_cmp(m, best) == std::cmp::Ordering::Less {
            best = *m;
        }
    }
    best
}

/// Returns Ordering::Less if `a` is the better representative.
fn rep_cmp(a: &RawHit, b: &RawHit) -> std::cmp::Ordering {
    // Higher score wins → invert ordering on score.
    match b
        .score
        .partial_cmp(&a.score)
        .unwrap_or(std::cmp::Ordering::Equal)
    {
        std::cmp::Ordering::Equal => {}
        ord => return ord,
    }
    // Lower box_number wins; None < Some(_).
    match (&a.box_number, &b.box_number) {
        (None, None) => {}
        (None, Some(_)) => return std::cmp::Ordering::Less,
        (Some(_), None) => return std::cmp::Ordering::Greater,
        (Some(x), Some(y)) => match x.cmp(y) {
            std::cmp::Ordering::Equal => {}
            ord => return ord,
        },
    }
    // Lower address_id wins (canonical string compare).
    a.address_id.cmp(&b.address_id)
}

/// Stable ordering for unit listings (`/building/:id/units`):
/// (box_number ascending, None first) then address_id ascending.
///
/// TODO: replace lex ordering with a natural sort. Today's contract gives
/// 13 < 1e < 28 < 2e < 3, which is wrong to a human reader. Existing tests
/// pin the lex order — they need to flip with the implementation. Defer
/// until somebody complains; UIs can re-sort client-side in the meantime.
pub fn sort_units_stable(hits: &mut [RawHit]) {
    hits.sort_by(|a, b| {
        match (&a.box_number, &b.box_number) {
            (None, None) => {}
            (None, Some(_)) => return std::cmp::Ordering::Less,
            (Some(_), None) => return std::cmp::Ordering::Greater,
            (Some(x), Some(y)) => match x.cmp(y) {
                std::cmp::Ordering::Equal => {}
                ord => return ord,
            },
        }
        a.address_id.cmp(&b.address_id)
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(
        bgid: &str,
        score: f32,
        box_no: Option<&str>,
        addr: &str,
        mt: MatchType,
        idx: usize,
    ) -> RawHit {
        RawHit {
            score,
            building_group_id: bgid.into(),
            box_number: box_no.map(|s| s.into()),
            address_id: addr.into(),
            match_type: mt,
            raw_index: idx,
        }
    }

    #[test]
    fn raw_hit_from_doc_reads_fields_and_tolerates_gaps() {
        let schema = CrabSchema::build();
        let mut doc = TantivyDocument::default();
        doc.add_text(schema.building_group_id, "ns|1|2");
        doc.add_text(schema.box_number, "b");
        doc.add_text(schema.best_id, "ns/9/1");
        let h = RawHit::from_doc(&doc, &schema, 1.5, MatchType::Fuzzy, 7);
        assert_eq!(h.building_group_id, "ns|1|2");
        assert_eq!(h.box_number.as_deref(), Some("b"));
        assert_eq!(h.address_id, "ns/9/1");
        assert_eq!(
            (h.score, h.match_type, h.raw_index),
            (1.5, MatchType::Fuzzy, 7)
        );
        let h = RawHit::from_doc(
            &TantivyDocument::default(),
            &schema,
            0.0,
            MatchType::Strict,
            0,
        );
        assert_eq!(h.building_group_id, "");
        assert_eq!(h.box_number, None);
    }

    #[test]
    fn raw_limit_overfetches_unless_expanded() {
        assert_eq!(raw_limit(10, true), 10);
        assert_eq!(raw_limit(10, false), 200);
        assert_eq!(raw_limit(500, false), RAW_HIT_CAP);
    }

    #[test]
    fn empty_input_returns_empty() {
        assert!(group_by_building(&[]).is_empty());
    }

    #[test]
    fn single_member_group() {
        let h = vec![hit("B|1", 5.0, Some("1"), "addr/1", MatchType::Strict, 0)];
        let g = group_by_building(&h);
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].unit_count, 1);
        assert_eq!(g[0].max_score, 5.0);
        assert_eq!(g[0].match_type, MatchType::Strict);
        assert_eq!(g[0].representative_index, 0);
    }

    #[test]
    fn collapses_three_units_into_one_building() {
        let h = vec![
            hit("B|1", 3.0, Some("1"), "addr/1", MatchType::Strict, 0),
            hit("B|1", 3.0, Some("2"), "addr/2", MatchType::Strict, 1),
            hit("B|1", 3.0, Some("3"), "addr/3", MatchType::Strict, 2),
        ];
        let g = group_by_building(&h);
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].unit_count, 3);
        // Lowest box wins → "1" → raw_index 0.
        assert_eq!(g[0].representative_index, 0);
    }

    #[test]
    fn max_not_sum_score_aggregation() {
        // Building A: one strong strict hit, score 10.
        // Building B: eleven weak hits, each score 1 (sum=11 but max=1).
        // If we used sum, B would beat A. With max, A wins.
        let mut h = vec![hit("A|1", 10.0, Some("1"), "a/1", MatchType::Strict, 0)];
        for i in 0..11 {
            h.push(hit(
                "B|1",
                1.0,
                Some(&format!("{i:02}")),
                &format!("b/{i}"),
                MatchType::Fuzzy,
                1 + i,
            ));
        }
        let g = group_by_building(&h);
        assert_eq!(g.len(), 2);
        assert_eq!(
            g[0].building_group_id, "A|1",
            "A must rank first with max scoring"
        );
        assert_eq!(g[0].max_score, 10.0);
        assert_eq!(g[0].match_type, MatchType::Strict);
        assert_eq!(g[1].building_group_id, "B|1");
        assert_eq!(g[1].max_score, 1.0);
        assert_eq!(g[1].unit_count, 11);
        assert_eq!(g[1].match_type, MatchType::Fuzzy);
    }

    #[test]
    fn match_type_strict_wins_within_group() {
        // Mixed members in one building — pipeline rarely produces this
        // but the aggregator must handle it.
        let h = vec![
            hit("B|1", 3.0, Some("1"), "addr/1", MatchType::Fuzzy, 0),
            hit("B|1", 5.0, Some("2"), "addr/2", MatchType::Strict, 1),
            hit("B|1", 2.0, Some("3"), "addr/3", MatchType::Fuzzy, 2),
        ];
        let g = group_by_building(&h);
        assert_eq!(
            g[0].match_type,
            MatchType::Strict,
            "any strict member → group strict"
        );
        // Representative is the strict one because it has the highest score.
        assert_eq!(g[0].representative_index, 1);
    }

    #[test]
    fn match_type_all_spatial_stays_spatial() {
        // /reverse path: every member is Spatial. The group must not get
        // downgraded to Fuzzy by the aggregator (regression: Phase 4).
        let h = vec![
            hit("B|1", 0.0, Some("1"), "a/1", MatchType::Spatial, 0),
            hit("B|1", 0.0, Some("2"), "a/2", MatchType::Spatial, 1),
        ];
        let g = group_by_building(&h);
        assert_eq!(g[0].match_type, MatchType::Spatial);
    }

    #[test]
    fn match_type_strict_beats_fuzzy_beats_spatial() {
        // Strict wins over both.
        let h = vec![
            hit("B|1", 1.0, Some("1"), "a/1", MatchType::Spatial, 0),
            hit("B|1", 2.0, Some("2"), "a/2", MatchType::Fuzzy, 1),
            hit("B|1", 3.0, Some("3"), "a/3", MatchType::Strict, 2),
        ];
        assert_eq!(group_by_building(&h)[0].match_type, MatchType::Strict);

        // Fuzzy wins over Spatial.
        let h = vec![
            hit("B|1", 1.0, Some("1"), "a/1", MatchType::Spatial, 0),
            hit("B|1", 2.0, Some("2"), "a/2", MatchType::Fuzzy, 1),
        ];
        assert_eq!(group_by_building(&h)[0].match_type, MatchType::Fuzzy);
    }

    #[test]
    fn match_type_all_fuzzy_stays_fuzzy() {
        let h = vec![
            hit("B|1", 3.0, Some("1"), "addr/1", MatchType::Fuzzy, 0),
            hit("B|1", 5.0, Some("2"), "addr/2", MatchType::Fuzzy, 1),
        ];
        let g = group_by_building(&h);
        assert_eq!(g[0].match_type, MatchType::Fuzzy);
    }

    #[test]
    fn representative_prefers_lower_box_on_score_tie() {
        let h = vec![
            hit("B|1", 5.0, Some("3"), "addr/c", MatchType::Strict, 0),
            hit("B|1", 5.0, Some("1"), "addr/a", MatchType::Strict, 1),
            hit("B|1", 5.0, Some("2"), "addr/b", MatchType::Strict, 2),
        ];
        let g = group_by_building(&h);
        // All scores tied → pick box "1" (raw_index 1).
        assert_eq!(g[0].representative_index, 1);
    }

    #[test]
    fn representative_none_box_beats_some_on_tie() {
        let h = vec![
            hit("B|1", 5.0, Some("1"), "addr/a", MatchType::Strict, 0),
            hit("B|1", 5.0, None, "addr/b", MatchType::Strict, 1),
        ];
        let g = group_by_building(&h);
        // None sorts first → raw_index 1 is the representative.
        assert_eq!(g[0].representative_index, 1);
    }

    #[test]
    fn groups_sorted_by_max_score_desc() {
        let h = vec![
            hit("A|1", 5.0, Some("1"), "a/1", MatchType::Strict, 0),
            hit("B|1", 9.0, Some("1"), "b/1", MatchType::Strict, 1),
            hit("C|1", 2.0, Some("1"), "c/1", MatchType::Strict, 2),
        ];
        let g = group_by_building(&h);
        let ids: Vec<&str> = g.iter().map(|x| x.building_group_id.as_str()).collect();
        assert_eq!(ids, vec!["B|1", "A|1", "C|1"]);
    }

    #[test]
    fn equal_max_score_groups_preserve_input_order() {
        let h = vec![
            hit("A|1", 5.0, Some("1"), "a/1", MatchType::Strict, 0),
            hit("B|1", 5.0, Some("1"), "b/1", MatchType::Strict, 1),
        ];
        let g = group_by_building(&h);
        // Stable sort + tied scores → input order preserved.
        assert_eq!(g[0].building_group_id, "A|1");
        assert_eq!(g[1].building_group_id, "B|1");
    }

    #[test]
    fn sort_units_stable_orders_by_box_then_addr() {
        let mut h = vec![
            hit("B|1", 5.0, Some("3"), "addr/c", MatchType::Strict, 0),
            hit("B|1", 5.0, None, "addr/none", MatchType::Strict, 1),
            hit("B|1", 5.0, Some("1"), "addr/a1", MatchType::Strict, 2),
            hit("B|1", 5.0, Some("1"), "addr/a0", MatchType::Strict, 3),
        ];
        sort_units_stable(&mut h);
        // None first; then box "1" with addr/a0 before addr/a1; then box "3".
        assert_eq!(h[0].raw_index, 1);
        assert_eq!(h[1].raw_index, 3);
        assert_eq!(h[2].raw_index, 2);
        assert_eq!(h[3].raw_index, 0);
    }

    #[test]
    fn sort_units_stable_is_idempotent() {
        let mut a = vec![
            hit("B|1", 5.0, Some("3"), "x/3", MatchType::Strict, 0),
            hit("B|1", 5.0, Some("1"), "x/1", MatchType::Strict, 1),
            hit("B|1", 5.0, Some("2"), "x/2", MatchType::Strict, 2),
        ];
        sort_units_stable(&mut a);
        let after_first: Vec<usize> = a.iter().map(|h| h.raw_index).collect();
        sort_units_stable(&mut a);
        let after_second: Vec<usize> = a.iter().map(|h| h.raw_index).collect();
        assert_eq!(after_first, after_second, "sort must be a fixed point");
    }
}
