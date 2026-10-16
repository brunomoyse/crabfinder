# crabfinder eval: single report

- Service: `http://127.0.0.1:8080`
- Total queries: **2708**
- Aggregated queries (excludes known_failure): 2705
- Transport/HTTP errors: 0

## Overall (Tier A)

| Metric | Value |
|---|---|
| n | 2705 |
| recall@1 | 0.928 |
| recall@5 | 0.974 |
| recall@10 | 0.985 |
| building_group_recall@1 | 0.933 |
| MRR | 0.948 |
| strict_match_rate | 0.995 |

## Latency (HTTP mode; under contention)

| | n | p50 | p95 | p99 | mean | max |
|---|---|---|---|---|---|---|
| ms | 2708 | 2.1 | 7.4 | 20.5 | 3.1 | 109.8 |

_Methodology: HTTP loopback. Loopback adds ~0.5–1 ms/query; significant on sub-3 ms strict queries, negligible on fuzzy. Measures user-perceived end-to-end latency, not search-engine latency._

## §3.2 contract: cross_language_recall@1

- Random samples (n=121): **0.983**
- Boosted samples (n=200): **0.980**

## By region

| Region | n | recall@1 | recall@5 | bgid_recall@1 | MRR | strict |
|---|---|---|---|---|---|---|
| BE-BRU | 492 | 0.955 | 0.988 | 0.955 | 0.971 | 0.994 |
| BE-VLG | 1388 | 0.931 | 0.979 | 0.931 | 0.951 | 0.999 |
| BE-WAL | 730 | 0.940 | 0.979 | 0.940 | 0.957 | 0.997 |

## By query variant

| Variant | n | recall@1 | recall@5 | bgid_recall@1 | MRR | strict |
|---|---|---|---|---|---|---|
| abbreviated | 299 | 0.970 | 0.997 | 0.970 | 0.983 | 0.983 |
| canonical | 500 | 0.974 | 0.996 | 0.974 | 0.985 | 0.998 |
| casing | 500 | 0.974 | 0.996 | 0.974 | 0.985 | 0.998 |
| cross_language | 311 | 0.984 | 0.997 | 0.984 | 0.990 | 1.000 |
| minimal | 500 | 0.758 | 0.914 | 0.758 | 0.821 | 1.000 |
| with_postal | 500 | 0.998 | 0.998 | 0.998 | 0.998 | 1.000 |

## By street primary language

| Language | n | recall@1 | recall@5 | bgid_recall@1 | MRR | strict |
|---|---|---|---|---|---|---|
| de | 16 | 1.000 | 1.000 | 1.000 | 1.000 | 1.000 |
| fr | 689 | 0.939 | 0.978 | 0.939 | 0.956 | 0.997 |
| nl | 1905 | 0.937 | 0.982 | 0.937 | 0.956 | 0.997 |

## Adversarial: by category

| Category | n | recall@1 | recall@5 | bgid_recall@1 | MRR | strict |
|---|---|---|---|---|---|---|
| cross_language | 10 | 0.900 | 1.000 | 0.900 | 0.950 | 1.000 |
| de_zone | 9 | 0.778 | 0.778 | 0.778 | 0.805 | 0.889 |
| facilites | 6 | 0.833 | 0.833 | 0.833 | 0.833 | 0.833 |
| hamlet | 30 | 0.567 | 0.700 | 0.567 | 0.621 | 0.900 |
| notability | 30 | 0.467 | 0.733 | 1.000 | 0.601 | 1.000 |
| special_chars | 10 | 0.900 | 1.000 | 0.900 | 0.950 | 0.900 |

## Known-failure tracking

| ID | Subcategory | State | Top-1 (bgid prefix) | Top-1 match_type |
|---|---|---|---|---|
| `adv-known-001` | rue-de-la-gare-brussels | **still_failing** | `geodata.wallonie.be/id/Streetname|7717257|19` | strict |
| `adv-known-002` | place-de-l-eglise-ambiguous | **still_failing** | `https://data.vlaanderen.be/id/straatnaam/|74945|16` | strict |
| `adv-known-003` | anvers-grand-place-cross-region | **still_failing** | `https://databrussels.be/id/streetname|3281|13` | fuzzy |
