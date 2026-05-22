# Eval diff — `reports/post-3-1-noboost.json` → `reports/post-3-1.json`

_Minimal MVP diff. Per-query regression list deferred to Phase 6.5b._

## Overall (Tier A) — Δ

| Metric | baseline | current | Δ |
|---|---|---|---|
| recall@1 | 0.911 | 0.915 | +0.003 |
| recall@5 | 0.961 | 0.962 | +0.002 |
| recall@10 | 0.973 | 0.977 | +0.004 |
| bgid_recall@1 | 0.913 | 0.921 | +0.008 |
| MRR | 0.933 | 0.936 | +0.003 |
| strict_match_rate | 0.997 | 0.997 | +0.000 |
| n | 2406 | 2406 | 0 |

## §3.2 contract — cross_language_recall@1 — Δ

| Pool | Baseline | Current | Δ |
|---|---|---|---|
| random   (n=121/121) | 0.983 | 0.992 | +0.008 |
| boosted  (n=200/200) | 0.985 | 0.985 | +0.000 |

## Latency — Δ

| | baseline | current | Δ |
|---|---|---|---|
| p50  | 0.7 ms | 2.2 ms | +1.5 ms |
| p95  | 2.6 ms | 10.9 ms | +8.3 ms |
| p99  | 8.0 ms | 37.1 ms | +29.1 ms |

## By region — Δ

| Region | n base/cur | recall@1 base/cur (Δ) | MRR base/cur (Δ) | strict base/cur (Δ) |
|---|---|---|---|---|
| BE-BRU | 400/400 | 0.965/0.965 (+0.000) | 0.976/0.976 (+0.000) | 1.000/1.000 (+0.000) |
| BE-VLG | 1205/1205 | 0.927/0.927 (+0.000) | 0.946/0.946 (-0.000) | 1.000/1.000 (+0.000) |
| BE-WAL | 706/706 | 0.932/0.932 (+0.000) | 0.953/0.953 (+0.000) | 0.997/0.997 (+0.000) |

## By variant — Δ

| Variant | n base/cur | recall@1 base/cur (Δ) | MRR base/cur (Δ) | strict base/cur (Δ) |
|---|---|---|---|---|
| canonical | 500/500 | 0.980/0.980 (+0.000) | 0.987/0.987 (+0.000) | 0.998/0.998 (+0.000) |
| casing | 500/500 | 0.980/0.980 (+0.000) | 0.987/0.987 (+0.000) | 0.998/0.998 (+0.000) |
| cross_language | 311/311 | 0.987/0.987 (+0.000) | 0.992/0.992 (+0.000) | 1.000/1.000 (+0.000) |
| minimal | 500/500 | 0.750/0.750 (+0.000) | 0.818/0.818 (-0.000) | 1.000/1.000 (+0.000) |
| with_postal | 500/500 | 0.998/0.998 (+0.000) | 0.998/0.998 (+0.000) | 1.000/1.000 (+0.000) |

## Adversarial — by category — Δ

| Category | n base/cur | recall@1 base/cur (Δ) | MRR base/cur (Δ) | strict base/cur (Δ) |
|---|---|---|---|---|
| cross_language | 10/10 | 0.900/1.000 (+0.100) | 0.950/1.000 (+0.050) | 1.000/1.000 (+0.000) |
| de_zone | 9/9 | 0.000/0.000 (+0.000) | 0.200/0.200 (+0.000) | 0.889/0.889 (+0.000) |
| facilites | 6/6 | 0.000/0.000 (+0.000) | 0.102/0.102 (+0.000) | 0.833/0.833 (+0.000) |
| hamlet | 30/30 | 0.233/0.233 (+0.000) | 0.263/0.263 (+0.000) | 0.900/0.900 (+0.000) |
| notability | 30/30 | 0.233/0.467 (+0.233) | 0.378/0.607 (+0.229) | 1.000/1.000 (+0.000) |
| special_chars | 10/10 | 0.900/0.900 (+0.000) | 0.950/0.950 (+0.000) | 0.900/0.900 (+0.000) |

## Known-failure state changes

_No state changes — all known_failure entries continue to behave as documented._
