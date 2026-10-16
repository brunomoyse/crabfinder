# Phase 11.1b ranking fixes — eval diff vs Phase 11.1

Same index (BeSt 2026-10-15, schema v11), same eval sets. The baseline is the service at the Phase 11.1 commit; the current one adds the 11.1b readings (`without_city_words`, `without_leading_type`, `search_local_typo`) and the `1er`/`ier` abbreviation entries.

Per query at rank@1: BeSt-only 4 fixed / 0 broken; +OSM 5 fixed / 0 broken; real-world 9 fixed / 0 broken.

The latency rows were measured while the competitor benchmark ran on the same machine, so they are noisy. The benchmark run (`benchmark-2026-10-16.md`) has crabfinder at p50 2.2 ms / p95 12.6 ms, against 2.4 / 14.1 ms before the fixes.

## BeSt-only (synthetic v3 + adversarial)



### Overall (Tier A) — Δ

| Metric | baseline | current | Δ |
|---|---|---|---|
| recall@1 | 0.964 | 0.965 | +0.001 |
| recall@5 | 0.980 | 0.981 | +0.001 |
| recall@10 | 0.984 | 0.984 | +0.001 |
| bgid_recall@1 | 0.964 | 0.965 | +0.001 |
| MRR | 0.971 | 0.972 | +0.001 |
| strict_match_rate | 0.882 | 0.882 | +0.000 |
| n | 4365 | 4365 | 0 |

### §3.2 contract — cross_language_recall@1 — Δ

| Pool | Baseline | Current | Δ |
|---|---|---|---|
| random   (n=116/116) | 1.000 | 1.000 | +0.000 |
| boosted  (n=200/200) | 1.000 | 1.000 | +0.000 |

### Latency — Δ

| | baseline | current | Δ |
|---|---|---|---|
| p50  | 1.9 ms | 4.8 ms | +3.0 ms |
| p95  | 9.5 ms | 25.6 ms | +16.0 ms |
| p99  | 18.9 ms | 56.9 ms | +38.0 ms |

### By region — Δ

| Region | n base/cur | recall@1 base/cur (Δ) | MRR base/cur (Δ) | strict base/cur (Δ) |
|---|---|---|---|---|
| BE-BRU | 863/863 | 0.988/0.990 (+0.001) | 0.991/0.992 (+0.001) | 0.879/0.879 (+0.000) |
| BE-VLG | 2177/2177 | 0.961/0.962 (+0.001) | 0.969/0.970 (+0.001) | 0.885/0.885 (+0.000) |
| BE-WAL | 1204/1204 | 0.969/0.970 (+0.001) | 0.977/0.978 (+0.001) | 0.885/0.885 (+0.000) |

### By variant — Δ

| Variant | n base/cur | recall@1 base/cur (Δ) | MRR base/cur (Δ) | strict base/cur (Δ) |
|---|---|---|---|---|
| abbreviated | 289/289 | 1.000/1.000 (+0.000) | 1.000/1.000 (+0.000) | 0.990/0.990 (+0.000) |
| box_number | 61/61 | 1.000/1.000 (+0.000) | 1.000/1.000 (+0.000) | 0.967/0.967 (+0.000) |
| canonical | 500/500 | 1.000/1.000 (+0.000) | 1.000/1.000 (+0.000) | 1.000/1.000 (+0.000) |
| casing | 500/500 | 1.000/1.000 (+0.000) | 1.000/1.000 (+0.000) | 1.000/1.000 (+0.000) |
| cross_language | 306/306 | 1.000/1.000 (+0.000) | 1.000/1.000 (+0.000) | 1.000/1.000 (+0.000) |
| high_house_number | 101/101 | 1.000/1.000 (+0.000) | 1.000/1.000 (+0.000) | 1.000/1.000 (+0.000) |
| messy_punctuation | 500/500 | 1.000/1.000 (+0.000) | 1.000/1.000 (+0.000) | 1.000/1.000 (+0.000) |
| minimal | 500/500 | 0.792/0.792 (+0.000) | 0.844/0.844 (+0.000) | 1.000/1.000 (+0.000) |
| typo | 487/487 | 0.988/0.996 (+0.008) | 0.990/0.998 (+0.008) | 0.000/0.000 (+0.000) |
| with_postal | 500/500 | 1.000/1.000 (+0.000) | 1.000/1.000 (+0.000) | 1.000/1.000 (+0.000) |
| wrong_postal | 500/500 | 0.958/0.958 (+0.000) | 0.962/0.962 (+0.000) | 1.000/1.000 (+0.000) |

### Adversarial — by category — Δ

| Category | n base/cur | recall@1 base/cur (Δ) | MRR base/cur (Δ) | strict base/cur (Δ) |
|---|---|---|---|---|
| cross_language | 10/10 | 1.000/1.000 (+0.000) | 1.000/1.000 (+0.000) | 1.000/1.000 (+0.000) |
| de_zone | 9/9 | 1.000/1.000 (+0.000) | 1.000/1.000 (+0.000) | 1.000/1.000 (+0.000) |
| facilites | 6/6 | 1.000/1.000 (+0.000) | 1.000/1.000 (+0.000) | 1.000/1.000 (+0.000) |
| hamlet | 30/30 | 0.833/0.833 (+0.000) | 0.833/0.833 (+0.000) | 0.933/0.933 (+0.000) |
| landmark | 26/26 | 0.269/0.269 (+0.000) | 0.269/0.269 (+0.000) | 0.308/0.308 (+0.000) |
| notability | 30/30 | 1.000/1.000 (+0.000) | 1.000/1.000 (+0.000) | 1.000/1.000 (+0.000) |
| special_chars | 10/10 | 1.000/1.000 (+0.000) | 1.000/1.000 (+0.000) | 0.900/0.900 (+0.000) |

### Distance to the reference point — Δ

| Report | n | found | ≤25 m | ≤100 m | ≤250 m | median | p90 |
|---|---|---|---|---|---|---|---|
| baseline | 0 | 0.000 | 0.000 | 0.000 | 0.000 | – | – |
| current | 56 | 0.875 | 0.000 | 0.161 | 0.321 | 399 m | 44122 m |

### Known-failure state changes

_No state changes — all known_failure entries continue to behave as documented._

## +OSM aids (`--places` index, `--pois` service)



### Overall (Tier A) — Δ

| Metric | baseline | current | Δ |
|---|---|---|---|
| recall@1 | 0.970 | 0.971 | +0.001 |
| recall@5 | 0.985 | 0.986 | +0.001 |
| recall@10 | 0.989 | 0.990 | +0.001 |
| bgid_recall@1 | 0.970 | 0.971 | +0.001 |
| MRR | 0.976 | 0.977 | +0.001 |
| strict_match_rate | 0.874 | 0.874 | +0.000 |
| n | 4365 | 4365 | 0 |

### §3.2 contract — cross_language_recall@1 — Δ

| Pool | Baseline | Current | Δ |
|---|---|---|---|
| random   (n=116/116) | 1.000 | 1.000 | +0.000 |
| boosted  (n=200/200) | 1.000 | 1.000 | +0.000 |

### Latency — Δ

| | baseline | current | Δ |
|---|---|---|---|
| p50  | 2.3 ms | 3.7 ms | +1.4 ms |
| p95  | 11.4 ms | 23.3 ms | +11.9 ms |
| p99  | 24.1 ms | 81.0 ms | +56.9 ms |

### By region — Δ

| Region | n base/cur | recall@1 base/cur (Δ) | MRR base/cur (Δ) | strict base/cur (Δ) |
|---|---|---|---|---|
| BE-BRU | 863/863 | 0.988/0.990 (+0.001) | 0.991/0.992 (+0.001) | 0.879/0.879 (+0.000) |
| BE-VLG | 2177/2177 | 0.961/0.962 (+0.001) | 0.969/0.970 (+0.001) | 0.885/0.885 (+0.000) |
| BE-WAL | 1204/1204 | 0.969/0.970 (+0.001) | 0.977/0.978 (+0.001) | 0.885/0.885 (+0.000) |

### By variant — Δ

| Variant | n base/cur | recall@1 base/cur (Δ) | MRR base/cur (Δ) | strict base/cur (Δ) |
|---|---|---|---|---|
| abbreviated | 289/289 | 1.000/1.000 (+0.000) | 1.000/1.000 (+0.000) | 0.990/0.990 (+0.000) |
| box_number | 61/61 | 1.000/1.000 (+0.000) | 1.000/1.000 (+0.000) | 0.967/0.967 (+0.000) |
| canonical | 500/500 | 1.000/1.000 (+0.000) | 1.000/1.000 (+0.000) | 1.000/1.000 (+0.000) |
| casing | 500/500 | 1.000/1.000 (+0.000) | 1.000/1.000 (+0.000) | 1.000/1.000 (+0.000) |
| cross_language | 306/306 | 1.000/1.000 (+0.000) | 1.000/1.000 (+0.000) | 1.000/1.000 (+0.000) |
| high_house_number | 101/101 | 1.000/1.000 (+0.000) | 1.000/1.000 (+0.000) | 1.000/1.000 (+0.000) |
| messy_punctuation | 500/500 | 1.000/1.000 (+0.000) | 1.000/1.000 (+0.000) | 1.000/1.000 (+0.000) |
| minimal | 500/500 | 0.792/0.792 (+0.000) | 0.844/0.844 (+0.000) | 1.000/1.000 (+0.000) |
| typo | 487/487 | 0.988/0.996 (+0.008) | 0.990/0.998 (+0.008) | 0.000/0.000 (+0.000) |
| with_postal | 500/500 | 1.000/1.000 (+0.000) | 1.000/1.000 (+0.000) | 1.000/1.000 (+0.000) |
| wrong_postal | 500/500 | 0.958/0.958 (+0.000) | 0.962/0.962 (+0.000) | 1.000/1.000 (+0.000) |

### Adversarial — by category — Δ

| Category | n base/cur | recall@1 base/cur (Δ) | MRR base/cur (Δ) | strict base/cur (Δ) |
|---|---|---|---|---|
| cross_language | 10/10 | 1.000/1.000 (+0.000) | 1.000/1.000 (+0.000) | 1.000/1.000 (+0.000) |
| de_zone | 9/9 | 0.889/1.000 (+0.111) | 0.889/1.000 (+0.111) | 0.889/0.889 (+0.000) |
| facilites | 6/6 | 1.000/1.000 (+0.000) | 1.000/1.000 (+0.000) | 1.000/1.000 (+0.000) |
| hamlet | 30/30 | 1.000/1.000 (+0.000) | 1.000/1.000 (+0.000) | 0.000/0.000 (+0.000) |
| landmark | 26/26 | 1.000/1.000 (+0.000) | 1.000/1.000 (+0.000) | 0.038/0.038 (+0.000) |
| notability | 30/30 | 1.000/1.000 (+0.000) | 1.000/1.000 (+0.000) | 1.000/1.000 (+0.000) |
| special_chars | 10/10 | 1.000/1.000 (+0.000) | 1.000/1.000 (+0.000) | 0.900/0.900 (+0.000) |

### Distance to the reference point — Δ

| Report | n | found | ≤25 m | ≤100 m | ≤250 m | median | p90 |
|---|---|---|---|---|---|---|---|
| baseline | 0 | 0.000 | 0.000 | 0.000 | 0.000 | – | – |
| current | 56 | 1.000 | 0.482 | 0.839 | 0.946 | 27 m | 190 m |

### Known-failure state changes

_No state changes — all known_failure entries continue to behave as documented._

## Real-world set (`eval/realworld-be-v1.yaml`)



### Overall (Tier A) — Δ

| Metric | baseline | current | Δ |
|---|---|---|---|
| recall@1 | 0.923 | 0.929 | +0.006 |
| recall@5 | 0.932 | 0.938 | +0.006 |
| recall@10 | 0.933 | 0.938 | +0.005 |
| bgid_recall@1 | 0.923 | 0.929 | +0.006 |
| MRR | 0.927 | 0.933 | +0.006 |
| strict_match_rate | 0.970 | 0.979 | +0.010 |
| n | 1510 | 1510 | 0 |

### §3.2 contract — cross_language_recall@1 — Δ

| Pool | Baseline | Current | Δ |
|---|---|---|---|
| random   (n=0/0) | 0.000 | 0.000 | +0.000 |
| boosted  (n=0/0) | 0.000 | 0.000 | +0.000 |

### Latency — Δ

| | baseline | current | Δ |
|---|---|---|---|
| p50  | 2.8 ms | 4.1 ms | +1.3 ms |
| p95  | 26.9 ms | 36.9 ms | +10.0 ms |
| p99  | 65.6 ms | 102.5 ms | +36.9 ms |

### By region — Δ

| Region | n base/cur | recall@1 base/cur (Δ) | MRR base/cur (Δ) | strict base/cur (Δ) |
|---|---|---|---|---|

### By variant — Δ

| Variant | n base/cur | recall@1 base/cur (Δ) | MRR base/cur (Δ) | strict base/cur (Δ) |
|---|---|---|---|---|

### Adversarial — by category — Δ

| Category | n base/cur | recall@1 base/cur (Δ) | MRR base/cur (Δ) | strict base/cur (Δ) |
|---|---|---|---|---|

### Real-world — by dataset — Δ

| Dataset | n base/cur | recall@1 base/cur (Δ) | MRR base/cur (Δ) | strict base/cur (Δ) |
|---|---|---|---|---|
| brussels-bpost | 100/100 | 0.890/0.900 (+0.010) | 0.897/0.907 (+0.010) | 0.920/0.940 (+0.020) |
| brussels-pharmacies | 60/60 | 1.000/1.000 (+0.000) | 1.000/1.000 (+0.000) | 1.000/1.000 (+0.000) |
| flanders-care | 250/250 | 0.928/0.928 (+0.000) | 0.928/0.928 (+0.000) | 0.992/0.992 (+0.000) |
| flanders-schools | 250/250 | 0.992/0.992 (+0.000) | 0.992/0.992 (+0.000) | 0.992/0.992 (+0.000) |
| fwb-schools-brussels | 300/300 | 0.953/0.953 (+0.000) | 0.956/0.956 (+0.000) | 0.967/0.970 (+0.003) |
| fwb-schools-wallonia | 400/400 | 0.840/0.845 (+0.005) | 0.850/0.855 (+0.005) | 0.950/0.970 (+0.020) |
| namur-commerces | 150/150 | 0.953/0.993 (+0.040) | 0.954/0.993 (+0.039) | 0.973/1.000 (+0.027) |

### Distance to the reference point — Δ

| Report | n | found | ≤25 m | ≤100 m | ≤250 m | median | p90 |
|---|---|---|---|---|---|---|---|
| baseline | 1510 | 1.000 | 0.730 | 0.923 | 0.962 | 5 m | 78 m |
| current | 1510 | 1.000 | 0.736 | 0.929 | 0.967 | 5 m | 72 m |

### Known-failure state changes

_No state changes — all known_failure entries continue to behave as documented._
