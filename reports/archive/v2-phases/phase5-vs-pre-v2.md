# Eval diff: `reports/pre-v2-baseline.json` → `/private/tmp/claude-501/-Users-brunomoyse-Development-applications-PERSO-crabfinder/ffd86c4d-8c56-4a73-8e94-dd0aa905ccbc/scratchpad/phase5.json`

_Minimal MVP diff. Per-query regression list deferred to Phase 6.5b._

## Overall (Tier A): Δ

| Metric | baseline | current | Δ |
|---|---|---|---|
| recall@1 | 0.841 | 0.962 | +0.121 |
| recall@5 | 0.891 | 0.982 | +0.091 |
| recall@10 | 0.908 | 0.986 | +0.079 |
| bgid_recall@1 | 0.841 | 0.962 | +0.121 |
| MRR | 0.863 | 0.971 | +0.108 |
| strict_match_rate | 0.726 | 0.870 | +0.144 |
| n | 3837 | 3837 | 0 |

## §3.2 contract: cross_language_recall@1: Δ

| Pool | Baseline | Current | Δ |
|---|---|---|---|
| random   (n=116/116) | 0.983 | 1.000 | +0.017 |
| boosted  (n=200/200) | 0.995 | 0.995 | +0.000 |

## Latency: Δ

| | baseline | current | Δ |
|---|---|---|---|
| p50  | 1.8 ms | 1.1 ms | -0.6 ms |
| p95  | 17.0 ms | 6.1 ms | -10.9 ms |
| p99  | 51.4 ms | 8.3 ms | -43.1 ms |

## By region: Δ

| Region | n base/cur | recall@1 base/cur (Δ) | MRR base/cur (Δ) | strict base/cur (Δ) |
|---|---|---|---|---|
| BE-BRU | 764/764 | 0.817/0.992 (+0.175) | 0.839/0.995 (+0.155) | 0.694/0.864 (+0.170) |
| BE-VLG | 1925/1925 | 0.833/0.957 (+0.125) | 0.853/0.968 (+0.115) | 0.731/0.870 (+0.138) |
| BE-WAL | 1053/1053 | 0.883/0.968 (+0.085) | 0.907/0.977 (+0.070) | 0.720/0.868 (+0.148) |

## By variant: Δ

| Variant | n base/cur | recall@1 base/cur (Δ) | MRR base/cur (Δ) | strict base/cur (Δ) |
|---|---|---|---|---|
| abbreviated | 289/289 | 0.990/1.000 (+0.010) | 0.993/1.000 (+0.007) | 0.990/0.990 (+0.000) |
| box_number | 59/59 | 0.983/1.000 (+0.017) | 0.992/1.000 (+0.008) | 0.000/0.932 (+0.932) |
| canonical | 500/500 | 0.986/0.998 (+0.012) | 0.992/0.999 (+0.007) | 1.000/1.000 (+0.000) |
| casing | 500/500 | 0.986/0.998 (+0.012) | 0.992/0.999 (+0.007) | 1.000/1.000 (+0.000) |
| cross_language | 306/306 | 0.997/0.997 (+0.000) | 0.998/0.998 (+0.000) | 1.000/1.000 (+0.000) |
| high_house_number | 101/101 | 0.158/1.000 (+0.842) | 0.170/1.000 (+0.830) | 1.000/1.000 (+0.000) |
| messy_punctuation | 500/500 | 0.884/0.998 (+0.114) | 0.915/0.999 (+0.084) | 0.006/1.000 (+0.994) |
| minimal | 500/500 | 0.776/0.778 (+0.002) | 0.832/0.834 (+0.002) | 1.000/1.000 (+0.000) |
| typo | 487/487 | 0.366/0.988 (+0.622) | 0.423/0.990 (+0.566) | 0.000/0.000 (+0.000) |
| with_postal | 500/500 | 0.996/0.998 (+0.002) | 0.997/0.999 (+0.002) | 1.000/1.000 (+0.000) |

## Adversarial: by category: Δ

| Category | n base/cur | recall@1 base/cur (Δ) | MRR base/cur (Δ) | strict base/cur (Δ) |
|---|---|---|---|---|
| cross_language | 10/10 | 0.800/1.000 (+0.200) | 0.900/1.000 (+0.100) | 1.000/1.000 (+0.000) |
| de_zone | 9/9 | 0.778/0.778 (+0.000) | 0.778/0.778 (+0.000) | 0.889/0.889 (+0.000) |
| facilites | 6/6 | 0.833/0.833 (+0.000) | 0.833/0.833 (+0.000) | 0.833/0.833 (+0.000) |
| hamlet | 30/30 | 0.467/0.467 (+0.000) | 0.467/0.467 (+0.000) | 0.900/0.900 (+0.000) |
| notability | 30/30 | 0.933/0.933 (+0.000) | 0.933/0.933 (+0.000) | 1.000/1.000 (+0.000) |
| special_chars | 10/10 | 0.900/0.900 (+0.000) | 0.950/0.950 (+0.000) | 0.900/0.900 (+0.000) |

## Known-failure state changes

_No state changes; all known_failure entries continue to behave as documented._
