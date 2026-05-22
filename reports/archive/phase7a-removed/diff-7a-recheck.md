# Eval diff — `reports/mvp-v1-baseline.json` → `reports/post-7a-recheck.json`

_Minimal MVP diff. Per-query regression list deferred to Phase 6.5b._

## Overall (Tier A) — Δ

| Metric | baseline | current | Δ |
|---|---|---|---|
| recall@1 | 0.924 | 0.939 | +0.015 |
| recall@5 | 0.971 | 0.979 | +0.008 |
| recall@10 | 0.984 | 0.987 | +0.003 |
| bgid_recall@1 | 0.931 | 0.939 | +0.008 |
| MRR | 0.945 | 0.957 | +0.012 |
| strict_match_rate | 0.995 | 0.997 | +0.002 |
| n | 2705 | 2610 | -95 |

## §3.2 contract — cross_language_recall@1 — Δ

| Pool | Baseline | Current | Δ |
|---|---|---|---|
| random   (n=121/111) | 0.992 | 1.000 | +0.008 |
| boosted  (n=200/200) | 0.980 | 0.985 | +0.005 |

## Latency — Δ

| | baseline | current | Δ |
|---|---|---|---|
| p50  | 1.9 ms | 3.8 ms | +1.9 ms |
| p95  | 5.6 ms | 57.8 ms | +52.2 ms |
| p99  | 9.4 ms | 84.5 ms | +75.1 ms |

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
| cross_language | 10/0 | 0.900/0.000 (-0.900) | 0.933/0.000 (-0.933) | 1.000/0.000 (-1.000) |
| de_zone | 9/0 | 0.778/0.000 (-0.778) | 0.778/0.000 (-0.778) | 0.889/0.000 (-0.889) |
| facilites | 6/0 | 0.667/0.000 (-0.667) | 0.750/0.000 (-0.750) | 0.833/0.000 (-0.833) |
| hamlet | 30/0 | 0.467/0.000 (-0.467) | 0.558/0.000 (-0.558) | 0.900/0.000 (-0.900) |
| notability | 30/0 | 0.333/0.000 (-0.333) | 0.522/0.000 (-0.522) | 1.000/0.000 (-1.000) |
| special_chars | 10/0 | 0.800/0.000 (-0.800) | 0.900/0.000 (-0.900) | 0.900/0.000 (-0.900) |

## Known-failure state changes

_No state changes — all known_failure entries continue to behave as documented._
