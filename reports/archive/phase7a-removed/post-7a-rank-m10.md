# crabfinder eval: single report

- Service: `http://127.0.0.1:8080`
- Total queries: **5895**
- Aggregated queries (excludes known_failure): 5892
- Transport/HTTP errors: 0

## Overall (Tier A)

| Metric | Value |
|---|---|
| n | 2705 |
| recall@1 | 0.925 |
| recall@5 | 0.971 |
| recall@10 | 0.984 |
| building_group_recall@1 | 0.933 |
| MRR | 0.946 |
| strict_match_rate | 0.995 |

## Latency (HTTP mode; under contention)

| | n | p50 | p95 | p99 | mean | max |
|---|---|---|---|---|---|---|
| ms | 5895 | 3.3 | 59.7 | 95.7 | 9.2 | 150.9 |

_Methodology: HTTP loopback. Loopback adds ~0.5–1 ms/query; significant on sub-3 ms strict queries, negligible on fuzzy. Measures user-perceived end-to-end latency, not search-engine latency._

## §3.2 contract: cross_language_recall@1

- Random samples (n=121): **0.992**
- Boosted samples (n=200): **0.985**

## By region

| Region | n | recall@1 | recall@5 | bgid_recall@1 | MRR | strict |
|---|---|---|---|---|---|---|
| BE-BRU | 492 | 0.963 | 0.988 | 0.963 | 0.975 | 0.994 |
| BE-VLG | 1388 | 0.929 | 0.976 | 0.929 | 0.950 | 0.999 |
| BE-WAL | 730 | 0.940 | 0.979 | 0.940 | 0.958 | 0.997 |

## By query variant

| Variant | n | recall@1 | recall@5 | bgid_recall@1 | MRR | strict |
|---|---|---|---|---|---|---|
| abbreviated | 299 | 0.977 | 0.997 | 0.977 | 0.986 | 0.983 |
| canonical | 500 | 0.974 | 0.996 | 0.974 | 0.985 | 0.998 |
| casing | 500 | 0.974 | 0.996 | 0.974 | 0.985 | 0.998 |
| cross_language | 311 | 0.990 | 0.997 | 0.990 | 0.993 | 1.000 |
| minimal | 500 | 0.754 | 0.906 | 0.754 | 0.819 | 1.000 |
| with_postal | 500 | 0.998 | 0.998 | 0.998 | 0.998 | 1.000 |

## By street primary language

| Language | n | recall@1 | recall@5 | bgid_recall@1 | MRR | strict |
|---|---|---|---|---|---|---|
| de | 16 | 1.000 | 1.000 | 1.000 | 1.000 | 1.000 |
| fr | 689 | 0.939 | 0.978 | 0.939 | 0.957 | 0.997 |
| nl | 1905 | 0.938 | 0.980 | 0.938 | 0.956 | 0.997 |

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
| n | 3187 |
| recall@1 | 0.242 |
| recall@5 | 0.259 |
| recall@10 | 0.259 |
| building_group_recall@1 | 0.242 |
| MRR | 0.249 |
| strict_match_rate | 0.000 |

### Latency (autocomplete only)

| | n | p50 | p95 | p99 | mean | max |
|---|---|---|---|---|---|---|
| ms | 3187 | 4.9 | 84.3 | 103.4 | 14.5 | 150.9 |

### By prefix length (chars)

| Length | n | recall@1 | recall@5 | bgid_recall@1 | MRR | strict |
|---|---|---|---|---|---|---|
| 2 | 799 | 0.020 | 0.023 | 0.020 | 0.021 | 0.000 |
| 4 | 799 | 0.149 | 0.156 | 0.149 | 0.152 | 0.000 |
| 6 | 798 | 0.335 | 0.357 | 0.335 | 0.344 | 0.000 |
| 8 | 791 | 0.468 | 0.501 | 0.468 | 0.483 | 0.000 |

### By region

| Region | n | recall@1 | recall@5 | bgid_recall@1 | MRR | strict |
|---|---|---|---|---|---|---|
| BE-BRU | 767 | 0.459 | 0.467 | 0.459 | 0.462 | 0.000 |
| BE-VLG | 1725 | 0.220 | 0.243 | 0.220 | 0.230 | 0.000 |
| BE-WAL | 695 | 0.058 | 0.066 | 0.058 | 0.062 | 0.000 |

### By variant

| Variant | n | recall@1 | recall@5 | bgid_recall@1 | MRR | strict |
|---|---|---|---|---|---|---|
| abbreviated_prefix | 1195 | 0.282 | 0.299 | 0.282 | 0.289 | 0.000 |
| prefix_progression | 1992 | 0.218 | 0.234 | 0.218 | 0.226 | 0.000 |

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
