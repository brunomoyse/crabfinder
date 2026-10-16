# crabfinder eval: single report

- Service: `http://127.0.0.1:8080`
- Total queries: **2409**
- Aggregated queries (excludes known_failure): 2406
- Transport/HTTP errors: 0

## Overall (Tier A)

| Metric | Value |
|---|---|
| n | 2406 |
| recall@1 | 0.919 |
| recall@5 | 0.968 |
| recall@10 | 0.979 |
| building_group_recall@1 | 0.923 |
| MRR | 0.940 |
| strict_match_rate | 0.997 |

## Latency (HTTP mode; under contention)

| | n | p50 | p95 | p99 | mean | max |
|---|---|---|---|---|---|---|
| ms | 2409 | 1.4 | 7.7 | 24.9 | 2.5 | 93.3 |

_Methodology: HTTP loopback. Loopback adds ~0.5–1 ms/query; significant on sub-3 ms strict queries, negligible on fuzzy. Measures user-perceived end-to-end latency, not search-engine latency._

## §3.2 contract: cross_language_recall@1

- Random samples (n=121): **0.975**
- Boosted samples (n=200): **0.985**

## By region

| Region | n | recall@1 | recall@5 | bgid_recall@1 | MRR | strict |
|---|---|---|---|---|---|---|
| BE-BRU | 400 | 0.970 | 0.988 | 0.970 | 0.978 | 1.000 |
| BE-VLG | 1205 | 0.917 | 0.973 | 0.917 | 0.940 | 1.000 |
| BE-WAL | 706 | 0.941 | 0.977 | 0.941 | 0.956 | 0.997 |

## By query variant

| Variant | n | recall@1 | recall@5 | bgid_recall@1 | MRR | strict |
|---|---|---|---|---|---|---|
| canonical | 500 | 0.976 | 0.996 | 0.976 | 0.985 | 0.998 |
| casing | 500 | 0.976 | 0.996 | 0.976 | 0.985 | 0.998 |
| cross_language | 311 | 0.987 | 0.997 | 0.987 | 0.992 | 1.000 |
| minimal | 500 | 0.750 | 0.904 | 0.750 | 0.813 | 1.000 |
| with_postal | 500 | 0.998 | 0.998 | 0.998 | 0.998 | 1.000 |

## By street primary language

| Language | n | recall@1 | recall@5 | bgid_recall@1 | MRR | strict |
|---|---|---|---|---|---|---|
| de | 15 | 1.000 | 1.000 | 1.000 | 1.000 | 1.000 |
| fr | 671 | 0.940 | 0.976 | 0.940 | 0.955 | 0.997 |
| nl | 1625 | 0.930 | 0.977 | 0.930 | 0.950 | 1.000 |

## Adversarial: by category

| Category | n | recall@1 | recall@5 | bgid_recall@1 | MRR | strict |
|---|---|---|---|---|---|---|
| cross_language | 10 | 0.800 | 1.000 | 0.800 | 0.900 | 1.000 |
| de_zone | 9 | 0.889 | 0.889 | 0.889 | 0.889 | 0.889 |
| facilites | 6 | 0.667 | 0.833 | 0.667 | 0.750 | 0.833 |
| hamlet | 30 | 0.467 | 0.733 | 0.467 | 0.561 | 0.900 |
| notability | 30 | 0.367 | 0.600 | 0.633 | 0.490 | 1.000 |
| special_chars | 10 | 1.000 | 1.000 | 1.000 | 1.000 | 0.900 |

## Known-failure tracking

| ID | Subcategory | State | Top-1 (bgid prefix) | Top-1 match_type |
|---|---|---|---|---|
| `adv-known-001` | rue-de-la-gare-brussels | **still_failing** | `geodata.wallonie.be/id/Streetname|7717257|3` | strict |
| `adv-known-002` | place-de-l-eglise-ambiguous | **still_failing** | `https://data.vlaanderen.be/id/straatnaam/|74945|14` | strict |
| `adv-known-003` | anvers-grand-place-cross-region | **still_failing** | `https://data.vlaanderen.be/id/straatnaam/|74896|28` | fuzzy |
