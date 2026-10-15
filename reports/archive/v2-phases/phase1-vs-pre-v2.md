# Eval diff — `reports/pre-v2-baseline.json` → `/private/tmp/claude-501/-Users-brunomoyse-Development-applications-PERSO-crabfinder/ffd86c4d-8c56-4a73-8e94-dd0aa905ccbc/scratchpad/phase1.json`

_Minimal MVP diff. Per-query regression list deferred to Phase 6.5b._

## Overall (Tier A) — Δ

| Metric | baseline | current | Δ |
|---|---|---|---|
| recall@1 | 0.836 | 0.836 | +0.000 |
| recall@5 | 0.888 | 0.888 | +0.000 |
| recall@10 | 0.907 | 0.907 | +0.000 |
| bgid_recall@1 | 0.841 | 0.841 | +0.000 |
| MRR | 0.859 | 0.859 | +0.000 |
| strict_match_rate | 0.726 | 0.726 | +0.000 |
| n | 3837 | 3837 | 0 |

## §3.2 contract — cross_language_recall@1 — Δ

| Pool | Baseline | Current | Δ |
|---|---|---|---|
| random   (n=116/116) | 0.991 | 0.991 | +0.000 |
| boosted  (n=200/200) | 0.995 | 0.995 | +0.000 |

## Latency — Δ

| | baseline | current | Δ |
|---|---|---|---|
| p50  | 1.6 ms | 1.7 ms | +0.1 ms |
| p95  | 14.7 ms | 16.2 ms | +1.4 ms |
| p99  | 51.5 ms | 53.2 ms | +1.7 ms |

## By region — Δ

| Region | n base/cur | recall@1 base/cur (Δ) | MRR base/cur (Δ) | strict base/cur (Δ) |
|---|---|---|---|---|
| BE-BRU | 764/764 | 0.817/0.817 (+0.000) | 0.839/0.839 (+0.000) | 0.694/0.694 (+0.000) |
| BE-VLG | 1925/1925 | 0.833/0.833 (+0.000) | 0.853/0.853 (+0.000) | 0.731/0.731 (+0.000) |
| BE-WAL | 1053/1053 | 0.883/0.883 (+0.000) | 0.907/0.907 (+0.000) | 0.720/0.720 (+0.000) |

## By variant — Δ

| Variant | n base/cur | recall@1 base/cur (Δ) | MRR base/cur (Δ) | strict base/cur (Δ) |
|---|---|---|---|---|
| abbreviated | 289/289 | 0.990/0.990 (+0.000) | 0.993/0.993 (+0.000) | 0.990/0.990 (+0.000) |
| box_number | 59/59 | 0.983/0.983 (+0.000) | 0.992/0.992 (+0.000) | 0.000/0.000 (+0.000) |
| canonical | 500/500 | 0.986/0.986 (+0.000) | 0.992/0.992 (+0.000) | 1.000/1.000 (+0.000) |
| casing | 500/500 | 0.986/0.986 (+0.000) | 0.992/0.992 (+0.000) | 1.000/1.000 (+0.000) |
| cross_language | 306/306 | 0.997/0.997 (+0.000) | 0.998/0.998 (+0.000) | 1.000/1.000 (+0.000) |
| high_house_number | 101/101 | 0.158/0.158 (+0.000) | 0.170/0.170 (+0.000) | 1.000/1.000 (+0.000) |
| messy_punctuation | 500/500 | 0.884/0.884 (+0.000) | 0.915/0.915 (+0.000) | 0.006/0.006 (+0.000) |
| minimal | 500/500 | 0.776/0.776 (+0.000) | 0.832/0.832 (+0.000) | 1.000/1.000 (+0.000) |
| typo | 487/487 | 0.366/0.366 (+0.000) | 0.423/0.423 (+0.000) | 0.000/0.000 (+0.000) |
| with_postal | 500/500 | 0.996/0.996 (+0.000) | 0.997/0.997 (+0.000) | 1.000/1.000 (+0.000) |

## Adversarial — by category — Δ

| Category | n base/cur | recall@1 base/cur (Δ) | MRR base/cur (Δ) | strict base/cur (Δ) |
|---|---|---|---|---|
| cross_language | 10/10 | 0.900/0.900 (+0.000) | 0.933/0.933 (+0.000) | 1.000/1.000 (+0.000) |
| de_zone | 9/9 | 0.778/0.778 (+0.000) | 0.778/0.778 (+0.000) | 0.889/0.889 (+0.000) |
| facilites | 6/6 | 0.833/0.833 (+0.000) | 0.833/0.833 (+0.000) | 0.833/0.833 (+0.000) |
| hamlet | 30/30 | 0.400/0.400 (+0.000) | 0.417/0.417 (+0.000) | 0.900/0.900 (+0.000) |
| notability | 30/30 | 0.233/0.233 (+0.000) | 0.424/0.424 (+0.000) | 1.000/1.000 (+0.000) |
| special_chars | 10/10 | 0.900/0.900 (+0.000) | 0.950/0.950 (+0.000) | 0.900/0.900 (+0.000) |

## Known-failure state changes

_No state changes — all known_failure entries continue to behave as documented._
