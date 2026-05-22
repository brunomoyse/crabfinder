# Eval diff — `reports/mvp-v1-baseline.json` → `reports/post-7a-recheck-full.json`

_Minimal MVP diff. Per-query regression list deferred to Phase 6.5b._

## Overall (Tier A) — Δ

| Metric | baseline | current | Δ |
|---|---|---|---|
| recall@1 | 0.924 | 0.925 | +0.001 |
| recall@5 | 0.971 | 0.971 | -0.000 |
| recall@10 | 0.984 | 0.984 | +0.000 |
| bgid_recall@1 | 0.931 | 0.933 | +0.003 |
| MRR | 0.945 | 0.946 | +0.001 |
| strict_match_rate | 0.995 | 0.995 | +0.000 |
| n | 2705 | 2705 | 0 |

## §3.2 contract — cross_language_recall@1 — Δ

| Pool | Baseline | Current | Δ |
|---|---|---|---|
| random   (n=121/121) | 0.992 | 0.992 | +0.000 |
| boosted  (n=200/200) | 0.980 | 0.985 | +0.005 |

## Latency — Δ

| | baseline | current | Δ |
|---|---|---|---|
| p50  | 1.9 ms | 2.6 ms | +0.8 ms |
| p95  | 5.6 ms | 60.7 ms | +55.1 ms |
| p99  | 9.4 ms | 98.6 ms | +89.1 ms |

## By region — Δ

| Region | n base/cur | recall@1 base/cur (Δ) | MRR base/cur (Δ) | strict base/cur (Δ) |
|---|---|---|---|---|
| BE-BRU | 492/492 | 0.963/0.963 (+0.000) | 0.976/0.975 (-0.001) | 0.994/0.994 (+0.000) |
| BE-VLG | 1388/1388 | 0.929/0.929 (+0.000) | 0.950/0.950 (-0.001) | 0.999/0.999 (+0.000) |
| BE-WAL | 730/730 | 0.936/0.940 (+0.004) | 0.952/0.958 (+0.005) | 0.997/0.997 (+0.000) |

## By variant — Δ

| Variant | n base/cur | recall@1 base/cur (Δ) | MRR base/cur (Δ) | strict base/cur (Δ) |
|---|---|---|---|---|
| abbreviated | 299/299 | 0.970/0.977 (+0.007) | 0.983/0.986 (+0.003) | 0.983/0.983 (+0.000) |
| canonical | 500/500 | 0.976/0.974 (-0.002) | 0.986/0.985 (-0.001) | 0.998/0.998 (+0.000) |
| casing | 500/500 | 0.976/0.974 (-0.002) | 0.986/0.985 (-0.001) | 0.998/0.998 (+0.000) |
| cross_language | 311/311 | 0.987/0.990 (+0.003) | 0.992/0.993 (+0.001) | 1.000/1.000 (+0.000) |
| minimal | 500/500 | 0.750/0.754 (+0.004) | 0.814/0.819 (+0.005) | 1.000/1.000 (+0.000) |
| with_postal | 500/500 | 0.998/0.998 (+0.000) | 0.998/0.998 (+0.000) | 1.000/1.000 (+0.000) |

## Adversarial — by category — Δ

| Category | n base/cur | recall@1 base/cur (Δ) | MRR base/cur (Δ) | strict base/cur (Δ) |
|---|---|---|---|---|
| cross_language | 10/10 | 0.900/0.900 (+0.000) | 0.933/0.950 (+0.017) | 1.000/1.000 (+0.000) |
| de_zone | 9/9 | 0.778/0.889 (+0.111) | 0.778/0.901 (+0.123) | 0.889/0.889 (+0.000) |
| facilites | 6/6 | 0.667/0.833 (+0.167) | 0.750/0.854 (+0.104) | 0.833/0.833 (+0.000) |
| hamlet | 30/30 | 0.467/0.567 (+0.100) | 0.558/0.613 (+0.056) | 0.900/0.900 (+0.000) |
| notability | 30/30 | 0.333/0.200 (-0.133) | 0.522/0.383 (-0.139) | 1.000/1.000 (+0.000) |
| special_chars | 10/10 | 0.800/0.800 (+0.000) | 0.900/0.900 (+0.000) | 0.900/0.900 (+0.000) |

## Known-failure state changes

_No state changes — all known_failure entries continue to behave as documented._
