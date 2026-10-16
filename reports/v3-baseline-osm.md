# crabfinder eval: single report

- Service: `http://127.0.0.1:8081`
- Total queries: **4368**
- Aggregated queries (excludes known_failure): 4365
- Transport/HTTP errors: 0

## Overall (Tier A)

| Metric | Value |
|---|---|
| n | 4365 |
| recall@1 | 0.971 |
| recall@5 | 0.986 |
| recall@10 | 0.990 |
| building_group_recall@1 | 0.971 |
| MRR | 0.977 |
| strict_match_rate | 0.874 |

## Latency (HTTP mode; under contention)

| | n | p50 | p95 | p99 | mean | max |
|---|---|---|---|---|---|---|
| ms | 4368 | 1.3 | 5.9 | 13.7 | 2.2 | 83.0 |

_Methodology: HTTP loopback. Loopback adds ~0.5–1 ms/query; significant on sub-3 ms strict queries, negligible on fuzzy. Measures user-perceived end-to-end latency, not search-engine latency._

## §3.2 contract: cross_language_recall@1

- Random samples (n=116): **1.000**
- Boosted samples (n=200): **1.000**

## By region

| Region | n | recall@1 | recall@5 | bgid_recall@1 | MRR | strict |
|---|---|---|---|---|---|---|
| BE-BRU | 863 | 0.990 | 0.995 | 0.990 | 0.992 | 0.879 |
| BE-VLG | 2177 | 0.962 | 0.980 | 0.962 | 0.970 | 0.885 |
| BE-WAL | 1204 | 0.970 | 0.988 | 0.970 | 0.978 | 0.885 |

## By query variant

| Variant | n | recall@1 | recall@5 | bgid_recall@1 | MRR | strict |
|---|---|---|---|---|---|---|
| abbreviated | 289 | 1.000 | 1.000 | 1.000 | 1.000 | 0.990 |
| box_number | 61 | 1.000 | 1.000 | 1.000 | 1.000 | 0.967 |
| canonical | 500 | 1.000 | 1.000 | 1.000 | 1.000 | 1.000 |
| casing | 500 | 1.000 | 1.000 | 1.000 | 1.000 | 1.000 |
| cross_language | 306 | 1.000 | 1.000 | 1.000 | 1.000 | 1.000 |
| high_house_number | 101 | 1.000 | 1.000 | 1.000 | 1.000 | 1.000 |
| messy_punctuation | 500 | 1.000 | 1.000 | 1.000 | 1.000 | 1.000 |
| minimal | 500 | 0.792 | 0.912 | 0.792 | 0.844 | 1.000 |
| typo | 487 | 0.996 | 1.000 | 0.996 | 0.998 | 0.000 |
| with_postal | 500 | 1.000 | 1.000 | 1.000 | 1.000 | 1.000 |
| wrong_postal | 500 | 0.958 | 0.966 | 0.958 | 0.962 | 1.000 |

## By street primary language

| Language | n | recall@1 | recall@5 | bgid_recall@1 | MRR | strict |
|---|---|---|---|---|---|---|
| de | 24 | 1.000 | 1.000 | 1.000 | 1.000 | 0.917 |
| fr | 1140 | 0.971 | 0.989 | 0.971 | 0.979 | 0.885 |
| nl | 3080 | 0.969 | 0.984 | 0.969 | 0.976 | 0.883 |

## Adversarial: by category

| Category | n | recall@1 | recall@5 | bgid_recall@1 | MRR | strict |
|---|---|---|---|---|---|---|
| cross_language (address) | 10 | 1.000 | 1.000 | 1.000 | 1.000 | 1.000 |
| de_zone (locality, street) | 9 | 1.000 | 1.000 | 1.000 | 1.000 | 0.889 |
| facilites (street) | 6 | 1.000 | 1.000 | 1.000 | 1.000 | 1.000 |
| hamlet (locality, street) | 30 | 1.000 | 1.000 | 1.000 | 1.000 | 0.000 |
| landmark (locality) | 26 | 1.000 | 1.000 | 1.000 | 1.000 | 0.038 |
| notability (building) | 30 | 1.000 | 1.000 | 1.000 | 1.000 | 1.000 |
| special_chars (address) | 10 | 1.000 | 1.000 | 1.000 | 1.000 | 0.900 |

## Alternatives by municipality (`--alternatives`)

The expected building is the best answer of one of the first k municipality groups.

| Variant | n | recall@1 | recall@3 |
|---|---|---|---|
| abbreviated | 289 | 0.000 | 0.000 |
| box_number | 61 | 0.000 | 0.000 |
| canonical | 500 | 0.000 | 0.000 |
| casing | 500 | 0.000 | 0.000 |
| cross_language | 306 | 0.003 | 0.003 |
| high_house_number | 101 | 0.000 | 0.000 |
| messy_punctuation | 500 | 0.000 | 0.000 |
| minimal | 500 | 0.070 | 0.164 |
| typo | 487 | 0.012 | 0.012 |
| with_postal | 500 | 0.000 | 0.000 |
| wrong_postal | 500 | 0.006 | 0.008 |
| all | 4244 | 0.011 | 0.022 |

## Distance to the reference point (top-1)

| Slice | n | found | ≤25 m | ≤100 m | ≤250 m | median | p90 |
|---|---|---|---|---|---|---|---|
| **all** | 56 | 1.000 | 0.482 | 0.839 | 0.946 | 27 m | 190 m |
| adversarial | 56 | 1.000 | 0.482 | 0.839 | 0.946 | 27 m | 190 m |

## Known-failure tracking

| ID | Subcategory | State | Top-1 (bgid prefix) | Top-1 match_type |
|---|---|---|---|---|
| `adv-known-001` | rue-de-la-gare-brussels | **still_failing** | `geodata.wallonie.be/id/Streetname|7706419|1` | strict |
| `adv-known-002` | place-de-l-eglise-ambiguous | **still_failing** | `geodata.wallonie.be/id/Streetname|7736393|1` | strict |
| `adv-known-003` | anvers-grand-place-cross-region | **still_failing** | `https://databrussels.be/id/streetname|3281|2` | fuzzy |
