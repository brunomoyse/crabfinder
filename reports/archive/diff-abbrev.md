# Eval diff: `reports/pre-abbrev.json` → `reports/post-abbrev.json`

_Minimal MVP diff. Per-query regression list deferred to Phase 6.5b._

## Overall (Tier A): Δ

| Metric | baseline | current | Δ |
|---|---|---|---|
| recall@1 | 0.853 | 0.924 | +0.071 |
| recall@5 | 0.910 | 0.971 | +0.061 |
| recall@10 | 0.928 | 0.984 | +0.056 |
| bgid_recall@1 | 0.860 | 0.931 | +0.071 |
| MRR | 0.878 | 0.945 | +0.067 |
| strict_match_rate | 0.887 | 0.995 | +0.109 |
| n | 2705 | 2705 | 0 |

## §3.2 contract: cross_language_recall@1: Δ

| Pool | Baseline | Current | Δ |
|---|---|---|---|
| random   (n=121/121) | 0.992 | 0.992 | +0.000 |
| boosted  (n=200/200) | 0.980 | 0.980 | +0.000 |

## Latency: Δ

| | baseline | current | Δ |
|---|---|---|---|
| p50  | 2.6 ms | 1.9 ms | -0.8 ms |
| p95  | 16.0 ms | 5.6 ms | -10.5 ms |
| p99  | 32.4 ms | 9.4 ms | -22.9 ms |

## By region: Δ

| Region | n base/cur | recall@1 base/cur (Δ) | MRR base/cur (Δ) | strict base/cur (Δ) |
|---|---|---|---|---|
| BE-BRU | 492/492 | 0.841/0.963 (+0.122) | 0.859/0.976 (+0.116) | 0.813/0.994 (+0.181) |
| BE-VLG | 1388/1388 | 0.838/0.929 (+0.091) | 0.864/0.950 (+0.086) | 0.868/0.999 (+0.130) |
| BE-WAL | 730/730 | 0.929/0.936 (+0.007) | 0.945/0.952 (+0.007) | 0.964/0.997 (+0.033) |

## By variant: Δ

| Variant | n base/cur | recall@1 base/cur (Δ) | MRR base/cur (Δ) | strict base/cur (Δ) |
|---|---|---|---|---|
| abbreviated | 299/299 | 0.328/0.970 (+0.642) | 0.375/0.983 (+0.607) | 0.000/0.983 (+0.983) |
| canonical | 500/500 | 0.976/0.976 (+0.000) | 0.986/0.986 (+0.000) | 0.998/0.998 (+0.000) |
| casing | 500/500 | 0.976/0.976 (+0.000) | 0.986/0.986 (+0.000) | 0.998/0.998 (+0.000) |
| cross_language | 311/311 | 0.987/0.987 (+0.000) | 0.992/0.992 (+0.000) | 1.000/1.000 (+0.000) |
| minimal | 500/500 | 0.750/0.750 (+0.000) | 0.814/0.814 (+0.000) | 1.000/1.000 (+0.000) |
| with_postal | 500/500 | 0.998/0.998 (+0.000) | 0.998/0.998 (+0.000) | 1.000/1.000 (+0.000) |

## Adversarial: by category: Δ

| Category | n base/cur | recall@1 base/cur (Δ) | MRR base/cur (Δ) | strict base/cur (Δ) |
|---|---|---|---|---|
| cross_language | 10/10 | 0.900/0.900 (+0.000) | 0.933/0.933 (+0.000) | 1.000/1.000 (+0.000) |
| de_zone | 9/9 | 0.778/0.778 (+0.000) | 0.778/0.778 (+0.000) | 0.889/0.889 (+0.000) |
| facilites | 6/6 | 0.667/0.667 (+0.000) | 0.750/0.750 (+0.000) | 0.833/0.833 (+0.000) |
| hamlet | 30/30 | 0.467/0.467 (+0.000) | 0.558/0.558 (+0.000) | 0.900/0.900 (+0.000) |
| notability | 30/30 | 0.333/0.333 (+0.000) | 0.522/0.522 (+0.000) | 1.000/1.000 (+0.000) |
| special_chars | 10/10 | 0.800/0.800 (+0.000) | 0.900/0.900 (+0.000) | 0.900/0.900 (+0.000) |

## Known-failure state changes

_No state changes; all known_failure entries continue to behave as documented._
