# crabfinder eval: single report

- Service: `http://127.0.0.1:8080`
- Total queries: **23063**
- Aggregated queries (excludes known_failure): 23060
- Transport/HTTP errors: 0

## Overall (Tier A)

| Metric | Value |
|---|---|
| n | 10481 |
| recall@1 | 0.934 |
| recall@5 | 0.974 |
| recall@10 | 0.986 |
| building_group_recall@1 | 0.936 |
| MRR | 0.952 |
| strict_match_rate | 0.997 |

## Latency (HTTP mode; under contention)

| | n | p50 | p95 | p99 | mean | max |
|---|---|---|---|---|---|---|
| ms | 23063 | 2.7 | 71.2 | 85.8 | 8.4 | 250.6 |

_Methodology: HTTP loopback. Loopback adds ~0.5–1 ms/query; significant on sub-3 ms strict queries, negligible on fuzzy. Measures user-perceived end-to-end latency, not search-engine latency._

## §3.2 contract: cross_language_recall@1

- Random samples (n=433): **0.986**
- Boosted samples (n=800): **0.988**

## By region

| Region | n | recall@1 | recall@5 | bgid_recall@1 | MRR | strict |
|---|---|---|---|---|---|---|
| BE-BRU | 1968 | 0.955 | 0.993 | 0.955 | 0.971 | 0.993 |
| BE-VLG | 5495 | 0.931 | 0.971 | 0.931 | 0.949 | 0.998 |
| BE-WAL | 2923 | 0.936 | 0.974 | 0.936 | 0.953 | 0.999 |

## By query variant

| Variant | n | recall@1 | recall@5 | bgid_recall@1 | MRR | strict |
|---|---|---|---|---|---|---|
| abbreviated | 1163 | 0.972 | 0.997 | 0.972 | 0.983 | 0.979 |
| canonical | 2000 | 0.977 | 0.997 | 0.977 | 0.986 | 0.999 |
| casing | 2000 | 0.977 | 0.997 | 0.977 | 0.986 | 0.999 |
| cross_language | 1223 | 0.988 | 0.998 | 0.988 | 0.993 | 1.000 |
| minimal | 2000 | 0.748 | 0.886 | 0.748 | 0.809 | 1.000 |
| with_postal | 2000 | 0.997 | 0.999 | 0.997 | 0.998 | 1.000 |

## By street primary language

| Language | n | recall@1 | recall@5 | bgid_recall@1 | MRR | strict |
|---|---|---|---|---|---|---|
| de | 46 | 0.978 | 1.000 | 0.978 | 0.986 | 1.000 |
| fr | 2818 | 0.936 | 0.974 | 0.936 | 0.953 | 0.999 |
| nl | 7522 | 0.937 | 0.977 | 0.937 | 0.955 | 0.997 |

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
| n | 12579 |
| recall@1 | 0.202 |
| recall@5 | 0.221 |
| recall@10 | 0.221 |
| building_group_recall@1 | 0.202 |
| MRR | 0.211 |
| strict_match_rate | 0.000 |

### Latency (autocomplete only)

| | n | p50 | p95 | p99 | mean | max |
|---|---|---|---|---|---|---|
| ms | 12579 | 4.1 | 78.5 | 91.1 | 13.6 | 250.6 |

### By prefix length (chars)

| Length | n | recall@1 | recall@5 | bgid_recall@1 | MRR | strict |
|---|---|---|---|---|---|---|
| 2 | 3163 | 0.014 | 0.016 | 0.014 | 0.015 | 0.000 |
| 4 | 3163 | 0.137 | 0.150 | 0.137 | 0.143 | 0.000 |
| 6 | 3155 | 0.270 | 0.295 | 0.270 | 0.282 | 0.000 |
| 8 | 3098 | 0.392 | 0.427 | 0.392 | 0.408 | 0.000 |

### By region

| Region | n | recall@1 | recall@5 | bgid_recall@1 | MRR | strict |
|---|---|---|---|---|---|---|
| BE-BRU | 3068 | 0.375 | 0.390 | 0.375 | 0.382 | 0.000 |
| BE-VLG | 6781 | 0.194 | 0.220 | 0.194 | 0.206 | 0.000 |
| BE-WAL | 2730 | 0.029 | 0.033 | 0.029 | 0.031 | 0.000 |

### By variant

| Variant | n | recall@1 | recall@5 | bgid_recall@1 | MRR | strict |
|---|---|---|---|---|---|---|
| abbreviated_prefix | 4633 | 0.234 | 0.253 | 0.234 | 0.243 | 0.000 |
| prefix_progression | 7946 | 0.184 | 0.202 | 0.184 | 0.192 | 0.000 |

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
