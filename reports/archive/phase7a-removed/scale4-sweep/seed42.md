# crabfinder eval: single report

- Service: `http://127.0.0.1:8080`
- Total queries: **23161**
- Aggregated queries (excludes known_failure): 23158
- Transport/HTTP errors: 0

## Overall (Tier A)

| Metric | Value |
|---|---|
| n | 10507 |
| recall@1 | 0.933 |
| recall@5 | 0.977 |
| recall@10 | 0.987 |
| building_group_recall@1 | 0.935 |
| MRR | 0.952 |
| strict_match_rate | 0.997 |

## Latency (HTTP mode; under contention)

| | n | p50 | p95 | p99 | mean | max |
|---|---|---|---|---|---|---|
| ms | 23161 | 2.5 | 52.4 | 84.3 | 7.9 | 170.2 |

_Methodology: HTTP loopback. Loopback adds ~0.5–1 ms/query; significant on sub-3 ms strict queries, negligible on fuzzy. Measures user-perceived end-to-end latency, not search-engine latency._

## §3.2 contract: cross_language_recall@1

- Random samples (n=435): **0.986**
- Boosted samples (n=800): **0.991**

## By region

| Region | n | recall@1 | recall@5 | bgid_recall@1 | MRR | strict |
|---|---|---|---|---|---|---|
| BE-BRU | 1977 | 0.954 | 0.989 | 0.954 | 0.970 | 0.993 |
| BE-VLG | 5523 | 0.934 | 0.976 | 0.934 | 0.952 | 0.999 |
| BE-WAL | 2912 | 0.928 | 0.978 | 0.928 | 0.950 | 0.999 |

## By query variant

| Variant | n | recall@1 | recall@5 | bgid_recall@1 | MRR | strict |
|---|---|---|---|---|---|---|
| abbreviated | 1187 | 0.973 | 0.999 | 0.973 | 0.985 | 0.982 |
| canonical | 2000 | 0.977 | 0.999 | 0.977 | 0.986 | 1.000 |
| casing | 2000 | 0.977 | 0.999 | 0.977 | 0.986 | 1.000 |
| cross_language | 1225 | 0.990 | 0.998 | 0.990 | 0.994 | 1.000 |
| minimal | 2000 | 0.740 | 0.897 | 0.740 | 0.808 | 1.000 |
| with_postal | 2000 | 0.998 | 0.998 | 0.998 | 0.998 | 1.000 |

## By street primary language

| Language | n | recall@1 | recall@5 | bgid_recall@1 | MRR | strict |
|---|---|---|---|---|---|---|
| de | 66 | 0.939 | 1.000 | 0.939 | 0.967 | 1.000 |
| fr | 2763 | 0.928 | 0.979 | 0.928 | 0.950 | 0.999 |
| nl | 7583 | 0.939 | 0.979 | 0.939 | 0.957 | 0.997 |

## Adversarial: by category

| Category | n | recall@1 | recall@5 | bgid_recall@1 | MRR | strict |
|---|---|---|---|---|---|---|
| cross_language | 10 | 0.900 | 1.000 | 0.900 | 0.950 | 1.000 |
| de_zone | 9 | 0.889 | 0.889 | 0.889 | 0.901 | 0.889 |
| facilites | 6 | 0.833 | 0.833 | 0.833 | 0.854 | 0.833 |
| hamlet | 30 | 0.567 | 0.667 | 0.567 | 0.613 | 0.900 |
| notability | 30 | 0.200 | 0.567 | 0.933 | 0.383 | 1.000 |
| special_chars | 10 | 0.800 | 1.000 | 0.800 | 0.900 | 0.900 |

## Autocomplete: prefix metrics (Phase 7a)

### Overall

| Metric | Value |
|---|---|
| n | 12651 |
| recall@1 | 0.203 |
| recall@5 | 0.221 |
| recall@10 | 0.221 |
| building_group_recall@1 | 0.203 |
| MRR | 0.211 |
| strict_match_rate | 0.000 |

### Latency (autocomplete only)

| | n | p50 | p95 | p99 | mean | max |
|---|---|---|---|---|---|---|
| ms | 12651 | 3.9 | 77.6 | 89.7 | 12.7 | 170.2 |

### By prefix length (chars)

| Length | n | recall@1 | recall@5 | bgid_recall@1 | MRR | strict |
|---|---|---|---|---|---|---|
| 2 | 3187 | 0.016 | 0.016 | 0.016 | 0.016 | 0.000 |
| 4 | 3185 | 0.136 | 0.145 | 0.136 | 0.140 | 0.000 |
| 6 | 3170 | 0.276 | 0.304 | 0.276 | 0.289 | 0.000 |
| 8 | 3109 | 0.391 | 0.424 | 0.391 | 0.406 | 0.000 |

### By region

| Region | n | recall@1 | recall@5 | bgid_recall@1 | MRR | strict |
|---|---|---|---|---|---|---|
| BE-BRU | 3101 | 0.332 | 0.344 | 0.332 | 0.337 | 0.000 |
| BE-VLG | 6766 | 0.211 | 0.235 | 0.211 | 0.222 | 0.000 |
| BE-WAL | 2784 | 0.042 | 0.048 | 0.042 | 0.045 | 0.000 |

### By variant

| Variant | n | recall@1 | recall@5 | bgid_recall@1 | MRR | strict |
|---|---|---|---|---|---|---|
| abbreviated_prefix | 4718 | 0.230 | 0.248 | 0.230 | 0.238 | 0.000 |
| prefix_progression | 7933 | 0.187 | 0.205 | 0.187 | 0.195 | 0.000 |

_File 15 §7 DoD hypotheses (treat as guidance, first-run empirics establish ground truth):_
- `prefix_recall@10` at `length=8`: ≥ 0.85
- `prefix_recall@10` at `length=4`: ≥ 0.50
- `abbreviated_prefix recall@10` at `length=8`: ≥ 0.80

## Known-failure tracking

| ID | Subcategory | State | Top-1 (bgid prefix) | Top-1 match_type |
|---|---|---|---|---|
| `adv-known-001` | rue-de-la-gare-brussels | **still_failing** | `geodata.wallonie.be/id/Streetname|7717257|19` | strict |
| `adv-known-002` | place-de-l-eglise-ambiguous | **still_failing** | `https://data.vlaanderen.be/id/straatnaam/|74945|14` | strict |
| `adv-known-003` | anvers-grand-place-cross-region | **still_failing** | `geodata.wallonie.be/id/Streetname|7723083|8` | fuzzy |
