# crabfinder eval — single report

- Service: `http://127.0.0.1:8080`
- Total queries: **2409**
- Aggregated queries (excludes known_failure): 2406
- Transport/HTTP errors: 0

## Overall (Tier A)

| Metric | Value |
|---|---|
| n | 2406 |
| recall@1 | 0.911 |
| recall@5 | 0.961 |
| recall@10 | 0.973 |
| building_group_recall@1 | 0.913 |
| MRR | 0.933 |
| strict_match_rate | 0.997 |

## Latency (HTTP mode; under contention)

| | n | p50 | p95 | p99 | mean | max |
|---|---|---|---|---|---|---|
| ms | 2409 | 0.7 | 2.6 | 8.0 | 1.1 | 65.3 |

_Methodology: HTTP loopback. Loopback adds ~0.5–1 ms/query — significant on sub-3 ms strict queries, negligible on fuzzy. Measures user-perceived end-to-end latency, not search-engine latency._

## §3.2 contract — cross_language_recall@1

- Random samples (n=121): **0.983**
- Boosted samples (n=200): **0.985**

## By region

| Region | n | recall@1 | recall@5 | bgid_recall@1 | MRR | strict |
|---|---|---|---|---|---|---|
| BE-BRU | 400 | 0.965 | 0.988 | 0.965 | 0.976 | 1.000 |
| BE-VLG | 1205 | 0.927 | 0.973 | 0.927 | 0.946 | 1.000 |
| BE-WAL | 706 | 0.932 | 0.977 | 0.932 | 0.953 | 0.997 |

## By query variant

| Variant | n | recall@1 | recall@5 | bgid_recall@1 | MRR | strict |
|---|---|---|---|---|---|---|
| canonical | 500 | 0.980 | 0.996 | 0.980 | 0.987 | 0.998 |
| casing | 500 | 0.980 | 0.996 | 0.980 | 0.987 | 0.998 |
| cross_language | 311 | 0.987 | 0.997 | 0.987 | 0.992 | 1.000 |
| minimal | 500 | 0.750 | 0.904 | 0.750 | 0.818 | 1.000 |
| with_postal | 500 | 0.998 | 0.998 | 0.998 | 0.998 | 1.000 |

## By street primary language

| Language | n | recall@1 | recall@5 | bgid_recall@1 | MRR | strict |
|---|---|---|---|---|---|---|
| de | 15 | 0.933 | 1.000 | 0.933 | 0.967 | 1.000 |
| fr | 671 | 0.933 | 0.976 | 0.933 | 0.953 | 0.997 |
| nl | 1625 | 0.936 | 0.977 | 0.936 | 0.954 | 1.000 |

## Adversarial — by category

| Category | n | recall@1 | recall@5 | bgid_recall@1 | MRR | strict |
|---|---|---|---|---|---|---|
| cross_language | 10 | 0.900 | 1.000 | 0.900 | 0.950 | 1.000 |
| de_zone | 9 | 0.000 | 0.556 | 0.000 | 0.200 | 0.889 |
| facilites | 6 | 0.000 | 0.167 | 0.000 | 0.102 | 0.833 |
| hamlet | 30 | 0.233 | 0.300 | 0.233 | 0.263 | 0.900 |
| notability | 30 | 0.233 | 0.633 | 0.333 | 0.378 | 1.000 |
| special_chars | 10 | 0.900 | 1.000 | 0.900 | 0.950 | 0.900 |

## Known-failure tracking

| ID | Subcategory | State | Top-1 (bgid prefix) | Top-1 match_type |
|---|---|---|---|---|
| `adv-known-001` | rue-de-la-gare-brussels | **still_failing** | `geodata.wallonie.be/id/Streetname|7717257|3` | strict |
| `adv-known-002` | place-de-l-eglise-ambiguous | **still_failing** | `https://data.vlaanderen.be/id/straatnaam/|74945|4` | strict |
| `adv-known-003` | anvers-grand-place-cross-region | **still_failing** | `https://data.vlaanderen.be/id/straatnaam/|74896|9` | fuzzy |
