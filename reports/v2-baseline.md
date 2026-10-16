# crabfinder eval — single report

- Service: `http://127.0.0.1:8083`
- Total queries: **4368**
- Aggregated queries (excludes known_failure): 4365
- Transport/HTTP errors: 0

## Overall (Tier A)

| Metric | Value |
|---|---|
| n | 4365 |
| recall@1 | 0.964 |
| recall@5 | 0.979 |
| recall@10 | 0.983 |
| building_group_recall@1 | 0.964 |
| MRR | 0.970 |
| strict_match_rate | 0.882 |

## Latency (HTTP mode; under contention)

| | n | p50 | p95 | p99 | mean | max |
|---|---|---|---|---|---|---|
| ms | 4368 | 1.7 | 7.7 | 12.8 | 2.7 | 122.1 |

_Methodology: HTTP loopback. Loopback adds ~0.5–1 ms/query — significant on sub-3 ms strict queries, negligible on fuzzy. Measures user-perceived end-to-end latency, not search-engine latency._

## §3.2 contract — cross_language_recall@1

- Random samples (n=116): **1.000**
- Boosted samples (n=200): **1.000**

## By region

| Region | n | recall@1 | recall@5 | bgid_recall@1 | MRR | strict |
|---|---|---|---|---|---|---|
| BE-BRU | 863 | 0.988 | 0.994 | 0.988 | 0.991 | 0.879 |
| BE-VLG | 2177 | 0.961 | 0.979 | 0.961 | 0.969 | 0.885 |
| BE-WAL | 1204 | 0.969 | 0.988 | 0.969 | 0.977 | 0.885 |

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
| typo | 487 | 0.988 | 0.994 | 0.988 | 0.990 | 0.000 |
| with_postal | 500 | 1.000 | 1.000 | 1.000 | 1.000 | 1.000 |
| wrong_postal | 500 | 0.958 | 0.966 | 0.958 | 0.962 | 1.000 |

## By street primary language

| Language | n | recall@1 | recall@5 | bgid_recall@1 | MRR | strict |
|---|---|---|---|---|---|---|
| de | 24 | 1.000 | 1.000 | 1.000 | 1.000 | 0.917 |
| fr | 1140 | 0.970 | 0.989 | 0.970 | 0.978 | 0.885 |
| nl | 3080 | 0.969 | 0.983 | 0.969 | 0.975 | 0.883 |

## Adversarial — by category

| Category | n | recall@1 | recall@5 | bgid_recall@1 | MRR | strict |
|---|---|---|---|---|---|---|
| cross_language (address) | 10 | 1.000 | 1.000 | 1.000 | 1.000 | 1.000 |
| de_zone (street) | 9 | 0.778 | 0.778 | 0.778 | 0.778 | 1.000 |
| facilites (street) | 6 | 0.833 | 0.833 | 0.833 | 0.833 | 1.000 |
| hamlet (locality, street) | 30 | 0.833 | 0.833 | 0.833 | 0.833 | 0.933 |
| landmark (locality) | 26 | 0.269 | 0.269 | 0.269 | 0.269 | 0.308 |
| notability (building) | 30 | 1.000 | 1.000 | 1.000 | 1.000 | 1.000 |
| special_chars (address) | 10 | 1.000 | 1.000 | 1.000 | 1.000 | 0.900 |

## Known-failure tracking

| ID | Subcategory | State | Top-1 (bgid prefix) | Top-1 match_type |
|---|---|---|---|---|
| `adv-known-001` | rue-de-la-gare-brussels | **still_failing** | `geodata.wallonie.be/id/Streetname|7706419|1` | strict |
| `adv-known-002` | place-de-l-eglise-ambiguous | **still_failing** | `geodata.wallonie.be/id/Streetname|7736393|1` | strict |
| `adv-known-003` | anvers-grand-place-cross-region | **still_failing** | `https://databrussels.be/id/streetname|3281|2` | fuzzy |
