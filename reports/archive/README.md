# Archive: deprecated baselines and §3.1 forensics

This directory holds the v3-era baseline plus the intermediate eval
artifacts that justify the rebaseline. They're kept (not deleted)
because file 12 §3.1.a references them as the empirical justification
for the methodology shift.

| File(s) | What it captures |
|---|---|
| `v3-mvp-v1-baseline-deprecated.*` | The original Phase 5 v3 baseline (frozen at the time of §3.1). |
| `noboost-rewrite-A.*` / `noboost-rewrite-B.*` | Two fresh v4 reindexes with boost OFF against the rewritten adversarial, which proves Tier A metrics are rebuild-stable (Δ ≤ 0.008) while Class 2 adversarial categories still drift 0.07–0.27. Run B was promoted to be the new `reports/mvp-v1-baseline.*` (later superseded by `v4-mvp-pre-abbrev.*` after the abbreviated variant was added). |
| `v4-mvp-pre-abbrev.*` | The previous `reports/mvp-v1-baseline.*` (the run-B noboost-rewrite). Frozen here when Phase 6 §3.3.a's abbreviation expansion shipped; at that point the synthetic eval set grew an `abbreviated` variant (299 new queries), making the old baseline no longer comparable. |
| `pre-abbrev.*` | v4-with-boost service against the new eval set (incl. abbreviated variant) with the `--abbreviation-dict` flag DISABLED, which measures abbreviated recall@1 with no expansion code path active. Used to compute `diff-abbrev.md`. |
| `diff-abbrev.md` | Pre- vs post-abbreviation eval. Headline: abbreviated recall@1 0.328 → 0.970 (+0.642), strict_match_rate 0.000 → 0.983, zero collateral on every other variant or adversarial category. |
| `post-3-1-noboost.*` | v4 with boost OFF, run against the ORIGINAL (pre-rewrite) adversarial. Used in the same-index A/B against `reports/post-3-1.*` to isolate the boost's effect. |
| `diff-noboost-vs-baseline.md` | v3 → v4-noboost (different physical indexes, same eval set): the rebuild-noise scorecard. |
| `diff-boost-vs-noboost.md` | Same physical v4 index, boost on vs off (only the multiplier code path differs): the isolated boost effect. Headline: +0.634 on notability bgid_recall@1, zero collateral. |

Together these prove three things that file 12 §3.1.a cites:

1. The apparent v3 → v4 adversarial collapse is rebuild noise, not the boost.
2. The v3 `minimal recall@1 = 0.760` was a lucky tiebreak; stable value is ~0.746.
3. The notability boost is a strict win on its target with no measurable collateral.

---

## `v3-mvp-v1-baseline-deprecated.*`

## `v3-mvp-v1-baseline-deprecated.*`

Original Phase 5 MVP baseline, captured against a **schema v3** Tantivy index
built from the BeSt 2026-05-19 dump. Frozen here when Phase 6 §3.1 promoted
a new baseline.

**Why deprecated, not just superseded:** the v3 baseline turned out to
overfit Tantivy's per-rebuild segment-tiebreak lottery in two ways that
made it actively misleading as a comparison target:

1. **Same-street tiebreak luck.** Adversarial entries (hamlet, de_zone,
   facilites) pinned a single `expected_building_group_id` per query for
   queries that didn't specify a house number. When the v3 index was
   built, BM25 ties between buildings on the same street happened to
   resolve in favour of the pinned bgid. The v4 reindexes (with otherwise
   identical inputs) re-resolved those ties differently, making every v4
   diff look like a 100% category collapse, even with notability disabled.
   The eval set was rewritten under §3.1 follow-up to express set
   semantics where the user query is HN-agnostic, killing this artifact.

2. **Synthetic `minimal` red-line was a lucky tiebreak.** The v3 baseline
   recorded `minimal recall@1 = 0.760`; two fresh v4 reindexes (rewritten
   adversarial, no boost) measured 0.742 and 0.750. The 0.760 reading was
   not a stable signal: it was the upper end of the natural rebuild
   distribution.

Both issues are visible in:
- `reports/diff-noboost-vs-baseline.md` (v3 → v4 noboost, same eval set: the
  rebuild-noise scorecard)
- `reports/diff-boost-vs-noboost.md` (same physical v4 index, boost on vs
  off: the isolated boost effect)

The new baseline (`reports/mvp-v1-baseline.*`) is the **run B** of a two-
reindex A/B against the rewritten adversarial under noboost, chosen because
its higher notability bgid_recall@1 (0.633 vs A's 0.367) is a more honest
lower-bound for §3.1 to clear.

**Do not** compare current runs against the v3 baseline; the comparator is
not stable and the methodology drifted. Compare against
`reports/mvp-v1-baseline.*` instead.
