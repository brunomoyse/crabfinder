# Eval diff — `reports/mvp-v1-baseline.json` → `reports/post-3-1-noboost.json`

_Minimal MVP diff. Per-query regression list deferred to Phase 6.5b._

## Overall (Tier A) — Δ

| Metric | baseline | current | Δ |
|---|---|---|---|
| recall@1 | 0.932 | 0.911 | -0.020 |
| recall@5 | 0.975 | 0.961 | -0.014 |
| recall@10 | 0.983 | 0.973 | -0.010 |
| bgid_recall@1 | 0.932 | 0.913 | -0.019 |
| MRR | 0.950 | 0.933 | -0.017 |
| strict_match_rate | 0.997 | 0.997 | +0.000 |
| n | 2406 | 2406 | 0 |

## §3.2 contract — cross_language_recall@1 — Δ

| Pool | Baseline | Current | Δ |
|---|---|---|---|
| random   (n=121/121) | 0.992 | 0.983 | -0.008 |
| boosted  (n=200/200) | 0.980 | 0.985 | +0.005 |

## Latency — Δ

| | baseline | current | Δ |
|---|---|---|---|
| p50  | 1.2 ms | 0.7 ms | -0.5 ms |
| p95  | 5.3 ms | 2.6 ms | -2.6 ms |
| p99  | 16.2 ms | 8.0 ms | -8.2 ms |

## By region — Δ

| Region | n base/cur | recall@1 base/cur (Δ) | MRR base/cur (Δ) | strict base/cur (Δ) |
|---|---|---|---|---|
| BE-BRU | 400/400 | 0.968/0.965 (-0.003) | 0.979/0.976 (-0.003) | 1.000/1.000 (+0.000) |
| BE-VLG | 1205/1205 | 0.924/0.927 (+0.003) | 0.944/0.946 (+0.002) | 1.000/1.000 (+0.000) |
| BE-WAL | 706/706 | 0.933/0.932 (-0.001) | 0.953/0.953 (+0.001) | 0.997/0.997 (+0.000) |

## By variant — Δ

| Variant | n base/cur | recall@1 base/cur (Δ) | MRR base/cur (Δ) | strict base/cur (Δ) |
|---|---|---|---|---|
| canonical | 500/500 | 0.974/0.980 (+0.006) | 0.985/0.987 (+0.002) | 0.998/0.998 (+0.000) |
| casing | 500/500 | 0.974/0.980 (+0.006) | 0.985/0.987 (+0.002) | 0.998/0.998 (+0.000) |
| cross_language | 311/311 | 0.984/0.987 (+0.003) | 0.990/0.992 (+0.002) | 1.000/1.000 (+0.000) |
| minimal | 500/500 | 0.760/0.750 (-0.010) | 0.820/0.818 (-0.001) | 1.000/1.000 (+0.000) |
| with_postal | 500/500 | 0.998/0.998 (+0.000) | 0.998/0.998 (+0.000) | 1.000/1.000 (+0.000) |

## Adversarial — by category — Δ

| Category | n base/cur | recall@1 base/cur (Δ) | MRR base/cur (Δ) | strict base/cur (Δ) |
|---|---|---|---|---|
| cross_language | 10/10 | 1.000/0.900 (-0.100) | 1.000/0.950 (-0.050) | 1.000/1.000 (+0.000) |
| de_zone | 9/9 | 1.000/0.000 (-1.000) | 1.000/0.200 (-0.800) | 0.889/0.889 (+0.000) |
| facilites | 6/6 | 1.000/0.000 (-1.000) | 1.000/0.102 (-0.898) | 0.833/0.833 (+0.000) |
| hamlet | 30/30 | 1.000/0.233 (-0.767) | 1.000/0.263 (-0.737) | 0.900/0.900 (+0.000) |
| notability | 30/30 | 0.600/0.233 (-0.367) | 0.627/0.378 (-0.249) | 1.000/1.000 (+0.000) |
| special_chars | 10/10 | 1.000/0.900 (-0.100) | 1.000/0.950 (-0.050) | 0.900/0.900 (+0.000) |

## Known-failure state changes

_No state changes — all known_failure entries continue to behave as documented._
