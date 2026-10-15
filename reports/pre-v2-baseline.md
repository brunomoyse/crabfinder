# crabfinder eval — single report

- Service: `http://127.0.0.1:8081`
- Total queries: **3840**
- Aggregated queries (excludes known_failure): 3837
- Transport/HTTP errors: 0

## Overall (Tier A)

| Metric | Value |
|---|---|
| n | 3837 |
| recall@1 | 0.842 |
| recall@5 | 0.893 |
| recall@10 | 0.910 |
| building_group_recall@1 | 0.842 |
| MRR | 0.864 |
| strict_match_rate | 0.726 |

## Latency (HTTP mode; under contention)

| | n | p50 | p95 | p99 | mean | max |
|---|---|---|---|---|---|---|
| ms | 3840 | 1.8 | 19.0 | 51.1 | 5.1 | 80.4 |

_Methodology: HTTP loopback. Loopback adds ~0.5–1 ms/query — significant on sub-3 ms strict queries, negligible on fuzzy. Measures user-perceived end-to-end latency, not search-engine latency._

## §3.2 contract — cross_language_recall@1

- Random samples (n=116): **0.983**
- Boosted samples (n=200): **0.995**

## By region

| Region | n | recall@1 | recall@5 | bgid_recall@1 | MRR | strict |
|---|---|---|---|---|---|---|
| BE-BRU | 764 | 0.817 | 0.869 | 0.817 | 0.839 | 0.694 |
| BE-VLG | 1925 | 0.833 | 0.878 | 0.833 | 0.853 | 0.731 |
| BE-WAL | 1053 | 0.883 | 0.939 | 0.883 | 0.907 | 0.720 |

## By query variant

| Variant | n | recall@1 | recall@5 | bgid_recall@1 | MRR | strict |
|---|---|---|---|---|---|---|
| abbreviated | 289 | 0.990 | 1.000 | 0.990 | 0.993 | 0.990 |
| box_number | 59 | 0.983 | 1.000 | 0.983 | 0.992 | 0.000 |
| canonical | 500 | 0.986 | 1.000 | 0.986 | 0.992 | 1.000 |
| casing | 500 | 0.986 | 1.000 | 0.986 | 0.992 | 1.000 |
| cross_language | 306 | 0.997 | 1.000 | 0.997 | 0.998 | 1.000 |
| high_house_number | 101 | 0.158 | 0.188 | 0.158 | 0.170 | 1.000 |
| messy_punctuation | 500 | 0.884 | 0.948 | 0.884 | 0.915 | 0.006 |
| minimal | 500 | 0.776 | 0.906 | 0.776 | 0.832 | 1.000 |
| typo | 487 | 0.366 | 0.503 | 0.366 | 0.423 | 0.000 |
| with_postal | 500 | 0.996 | 0.998 | 0.996 | 0.997 | 1.000 |

## By street primary language

| Language | n | recall@1 | recall@5 | bgid_recall@1 | MRR | strict |
|---|---|---|---|---|---|---|
| de | 21 | 0.905 | 0.952 | 0.905 | 0.929 | 0.762 |
| fr | 997 | 0.886 | 0.943 | 0.886 | 0.910 | 0.719 |
| nl | 2724 | 0.828 | 0.875 | 0.828 | 0.849 | 0.721 |

## Adversarial — by category

| Category | n | recall@1 | recall@5 | bgid_recall@1 | MRR | strict |
|---|---|---|---|---|---|---|
| cross_language (address) | 10 | 0.800 | 1.000 | 0.800 | 0.900 | 1.000 |
| de_zone (street) | 9 | 0.778 | 0.778 | 0.778 | 0.778 | 0.889 |
| facilites (street) | 6 | 0.667 | 0.667 | 0.667 | 0.667 | 0.833 |
| hamlet (locality, street) | 30 | 0.667 | 0.733 | 0.667 | 0.699 | 0.900 |
| notability (building) | 30 | 0.933 | 0.933 | 0.933 | 0.933 | 1.000 |
| special_chars (address) | 10 | 0.800 | 1.000 | 0.800 | 0.900 | 0.900 |

## Known-failure tracking

| ID | Subcategory | State | Top-1 (bgid prefix) | Top-1 match_type |
|---|---|---|---|---|
| `adv-known-001` | rue-de-la-gare-brussels | **still_failing** | `geodata.wallonie.be/id/Streetname|7706419|7` | strict |
| `adv-known-002` | place-de-l-eglise-ambiguous | **still_failing** | `geodata.wallonie.be/id/Streetname|7736393|32A` | strict |
| `adv-known-003` | anvers-grand-place-cross-region | **still_failing** | `geodata.wallonie.be/id/Streetname|7723083|58` | fuzzy |
