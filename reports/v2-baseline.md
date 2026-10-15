# crabfinder eval — single report

- Service: `http://127.0.0.1:8080`
- Total queries: **3840**
- Aggregated queries (excludes known_failure): 3837
- Transport/HTTP errors: 0

## Overall (Tier A)

| Metric | Value |
|---|---|
| n | 3837 |
| recall@1 | 0.967 |
| recall@5 | 0.986 |
| recall@10 | 0.991 |
| building_group_recall@1 | 0.967 |
| MRR | 0.975 |
| strict_match_rate | 0.863 |

## Latency (HTTP mode; under contention)

| | n | p50 | p95 | p99 | mean | max |
|---|---|---|---|---|---|---|
| ms | 3840 | 1.4 | 7.4 | 11.5 | 2.4 | 52.4 |

_Methodology: HTTP loopback. Loopback adds ~0.5–1 ms/query — significant on sub-3 ms strict queries, negligible on fuzzy. Measures user-perceived end-to-end latency, not search-engine latency._

## §3.2 contract — cross_language_recall@1

- Random samples (n=116): **1.000**
- Boosted samples (n=200): **1.000**

## By region

| Region | n | recall@1 | recall@5 | bgid_recall@1 | MRR | strict |
|---|---|---|---|---|---|---|
| BE-BRU | 764 | 0.992 | 0.999 | 0.992 | 0.995 | 0.864 |
| BE-VLG | 1925 | 0.957 | 0.981 | 0.957 | 0.968 | 0.870 |
| BE-WAL | 1053 | 0.969 | 0.989 | 0.969 | 0.978 | 0.868 |

## By query variant

| Variant | n | recall@1 | recall@5 | bgid_recall@1 | MRR | strict |
|---|---|---|---|---|---|---|
| abbreviated | 289 | 1.000 | 1.000 | 1.000 | 1.000 | 0.990 |
| box_number | 59 | 1.000 | 1.000 | 1.000 | 1.000 | 0.932 |
| canonical | 500 | 0.998 | 1.000 | 0.998 | 0.999 | 1.000 |
| casing | 500 | 0.998 | 1.000 | 0.998 | 0.999 | 1.000 |
| cross_language | 306 | 1.000 | 1.000 | 1.000 | 1.000 | 1.000 |
| high_house_number | 101 | 1.000 | 1.000 | 1.000 | 1.000 | 1.000 |
| messy_punctuation | 500 | 0.998 | 1.000 | 0.998 | 0.999 | 1.000 |
| minimal | 500 | 0.778 | 0.908 | 0.778 | 0.834 | 1.000 |
| typo | 487 | 0.988 | 0.994 | 0.988 | 0.990 | 0.000 |
| with_postal | 500 | 0.998 | 1.000 | 0.998 | 0.999 | 1.000 |

## By street primary language

| Language | n | recall@1 | recall@5 | bgid_recall@1 | MRR | strict |
|---|---|---|---|---|---|---|
| de | 21 | 1.000 | 1.000 | 1.000 | 1.000 | 0.905 |
| fr | 997 | 0.970 | 0.990 | 0.970 | 0.978 | 0.868 |
| nl | 2724 | 0.967 | 0.986 | 0.967 | 0.975 | 0.868 |

## Adversarial — by category

| Category | n | recall@1 | recall@5 | bgid_recall@1 | MRR | strict |
|---|---|---|---|---|---|---|
| cross_language (address) | 10 | 1.000 | 1.000 | 1.000 | 1.000 | 1.000 |
| de_zone (street) | 9 | 0.778 | 0.778 | 0.778 | 0.778 | 0.889 |
| facilites (street) | 6 | 0.833 | 0.833 | 0.833 | 0.833 | 1.000 |
| hamlet (locality, street) | 30 | 1.000 | 1.000 | 1.000 | 1.000 | 0.000 |
| notability (building) | 30 | 0.933 | 0.933 | 0.933 | 0.933 | 1.000 |
| special_chars (address) | 10 | 1.000 | 1.000 | 1.000 | 1.000 | 0.900 |

## Known-failure tracking

| ID | Subcategory | State | Top-1 (bgid prefix) | Top-1 match_type |
|---|---|---|---|---|
| `adv-known-001` | rue-de-la-gare-brussels | **still_failing** | `geodata.wallonie.be/id/Streetname|7706419|1` | strict |
| `adv-known-002` | place-de-l-eglise-ambiguous | **still_failing** | `geodata.wallonie.be/id/Streetname|7736393|1` | strict |
| `adv-known-003` | anvers-grand-place-cross-region | **still_failing** | `geodata.wallonie.be/id/Streetname|7723083|8` | fuzzy |
