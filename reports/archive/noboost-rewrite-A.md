# crabfinder eval — single report

- Service: `http://127.0.0.1:8080`
- Total queries: **2409**
- Aggregated queries (excludes known_failure): 2406
- Transport/HTTP errors: 0

## Overall (Tier A)

| Metric | Value |
|---|---|
| n | 2406 |
| recall@1 | 0.917 |
| recall@5 | 0.970 |
| recall@10 | 0.980 |
| building_group_recall@1 | 0.918 |
| MRR | 0.939 |
| strict_match_rate | 0.997 |

## Latency (HTTP mode; under contention)

| | n | p50 | p95 | p99 | mean | max |
|---|---|---|---|---|---|---|
| ms | 2409 | 1.6 | 10.2 | 25.1 | 3.0 | 104.7 |

_Methodology: HTTP loopback. Loopback adds ~0.5–1 ms/query — significant on sub-3 ms strict queries, negligible on fuzzy. Measures user-perceived end-to-end latency, not search-engine latency._

## §3.2 contract — cross_language_recall@1

- Random samples (n=121): **0.983**
- Boosted samples (n=200): **0.980**

## By region

| Region | n | recall@1 | recall@5 | bgid_recall@1 | MRR | strict |
|---|---|---|---|---|---|---|
| BE-BRU | 400 | 0.960 | 0.993 | 0.960 | 0.975 | 1.000 |
| BE-VLG | 1205 | 0.922 | 0.973 | 0.922 | 0.943 | 1.000 |
| BE-WAL | 706 | 0.931 | 0.982 | 0.931 | 0.952 | 0.997 |

## By query variant

| Variant | n | recall@1 | recall@5 | bgid_recall@1 | MRR | strict |
|---|---|---|---|---|---|---|
| canonical | 500 | 0.976 | 0.998 | 0.976 | 0.986 | 0.998 |
| casing | 500 | 0.976 | 0.998 | 0.976 | 0.986 | 0.998 |
| cross_language | 311 | 0.984 | 0.997 | 0.984 | 0.990 | 1.000 |
| minimal | 500 | 0.742 | 0.912 | 0.742 | 0.810 | 1.000 |
| with_postal | 500 | 0.998 | 0.998 | 0.998 | 0.998 | 1.000 |

## By street primary language

| Language | n | recall@1 | recall@5 | bgid_recall@1 | MRR | strict |
|---|---|---|---|---|---|---|
| de | 15 | 0.933 | 1.000 | 0.933 | 0.967 | 1.000 |
| fr | 671 | 0.931 | 0.981 | 0.931 | 0.952 | 0.997 |
| nl | 1625 | 0.931 | 0.978 | 0.931 | 0.951 | 1.000 |

## Adversarial — by category

| Category | n | recall@1 | recall@5 | bgid_recall@1 | MRR | strict |
|---|---|---|---|---|---|---|
| cross_language | 10 | 0.900 | 1.000 | 0.900 | 0.933 | 1.000 |
| de_zone | 9 | 0.778 | 0.778 | 0.778 | 0.778 | 0.889 |
| facilites | 6 | 0.667 | 1.000 | 0.667 | 0.764 | 0.833 |
| hamlet | 30 | 0.533 | 0.667 | 0.533 | 0.590 | 0.900 |
| notability | 30 | 0.333 | 0.600 | 0.367 | 0.437 | 1.000 |
| special_chars | 10 | 0.900 | 1.000 | 0.900 | 0.950 | 0.900 |

## Known-failure tracking

| ID | Subcategory | State | Top-1 (bgid prefix) | Top-1 match_type |
|---|---|---|---|---|
| `adv-known-001` | rue-de-la-gare-brussels | **still_failing** | `geodata.wallonie.be/id/Streetname|7717257|18` | strict |
| `adv-known-002` | place-de-l-eglise-ambiguous | **still_failing** | `https://data.vlaanderen.be/id/straatnaam/|74945|16` | strict |
| `adv-known-003` | anvers-grand-place-cross-region | **still_failing** | `https://data.vlaanderen.be/id/straatnaam/|74896|26` | fuzzy |
