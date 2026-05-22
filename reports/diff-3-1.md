# Eval diff — `reports/mvp-v1-baseline.json` → `reports/post-3-1.json`

_Minimal MVP diff. Per-query regression list deferred to Phase 6.5b._

## Overall (Tier A) — Δ

| Metric | baseline | current | Δ |
|---|---|---|---|
| recall@1 | 0.919 | 0.918 | -0.001 |
| recall@5 | 0.968 | 0.968 | -0.000 |
| recall@10 | 0.979 | 0.982 | +0.002 |
| bgid_recall@1 | 0.923 | 0.926 | +0.003 |
| MRR | 0.940 | 0.940 | +0.000 |
| strict_match_rate | 0.997 | 0.997 | +0.000 |
| n | 2406 | 2406 | 0 |

## §3.2 contract — cross_language_recall@1 — Δ

| Pool | Baseline | Current | Δ |
|---|---|---|---|
| random   (n=121/121) | 0.975 | 0.992 | +0.017 |
| boosted  (n=200/200) | 0.985 | 0.980 | -0.005 |

## Latency — Δ

| | baseline | current | Δ |
|---|---|---|---|
| p50  | 1.4 ms | 1.7 ms | +0.4 ms |
| p95  | 7.7 ms | 10.4 ms | +2.6 ms |
| p99  | 24.9 ms | 34.2 ms | +9.2 ms |

## By region — Δ

| Region | n base/cur | recall@1 base/cur (Δ) | MRR base/cur (Δ) | strict base/cur (Δ) |
|---|---|---|---|---|
| BE-BRU | 400/400 | 0.970/0.963 (-0.007) | 0.978/0.975 (-0.003) | 1.000/1.000 (+0.000) |
| BE-VLG | 1205/1205 | 0.917/0.924 (+0.007) | 0.940/0.945 (+0.005) | 1.000/1.000 (+0.000) |
| BE-WAL | 706/706 | 0.941/0.933 (-0.007) | 0.956/0.951 (-0.005) | 0.997/0.997 (+0.000) |

## By variant — Δ

| Variant | n base/cur | recall@1 base/cur (Δ) | MRR base/cur (Δ) | strict base/cur (Δ) |
|---|---|---|---|---|
| canonical | 500/500 | 0.976/0.976 (+0.000) | 0.985/0.986 (+0.000) | 0.998/0.998 (+0.000) |
| casing | 500/500 | 0.976/0.976 (+0.000) | 0.985/0.986 (+0.000) | 0.998/0.998 (+0.000) |
| cross_language | 311/311 | 0.987/0.987 (+0.000) | 0.992/0.992 (+0.000) | 1.000/1.000 (+0.000) |
| minimal | 500/500 | 0.750/0.750 (+0.000) | 0.813/0.814 (+0.001) | 1.000/1.000 (+0.000) |
| with_postal | 500/500 | 0.998/0.998 (+0.000) | 0.998/0.998 (+0.000) | 1.000/1.000 (+0.000) |

## Adversarial — by category — Δ

| Category | n base/cur | recall@1 base/cur (Δ) | MRR base/cur (Δ) | strict base/cur (Δ) |
|---|---|---|---|---|
| cross_language | 10/10 | 0.800/0.900 (+0.100) | 0.900/0.933 (+0.033) | 1.000/1.000 (+0.000) |
| de_zone | 9/9 | 0.889/0.778 (-0.111) | 0.889/0.778 (-0.111) | 0.889/0.889 (+0.000) |
| facilites | 6/6 | 0.667/0.667 (+0.000) | 0.750/0.750 (+0.000) | 0.833/0.833 (+0.000) |
| hamlet | 30/30 | 0.467/0.467 (+0.000) | 0.561/0.558 (-0.004) | 0.900/0.900 (+0.000) |
| notability | 30/30 | 0.367/0.333 (-0.033) | 0.490/0.522 (+0.032) | 1.000/1.000 (+0.000) |
| special_chars | 10/10 | 1.000/0.800 (-0.200) | 1.000/0.900 (-0.100) | 0.900/0.900 (+0.000) |

## Known-failure state changes

_No state changes — all known_failure entries continue to behave as documented._
