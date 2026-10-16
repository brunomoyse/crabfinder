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
| recall@5 | 0.969 |
| recall@10 | 0.980 |
| building_group_recall@1 | 0.934 |
| MRR | 0.946 |
| strict_match_rate | 0.995 |

## Latency (HTTP mode; under contention)

| | n | p50 | p95 | p99 | mean | max |
|---|---|---|---|---|---|---|
| ms | 2708 | 1.7 | 4.6 | 11.3 | 2.2 | 122.2 |

_Methodology: HTTP loopback. Loopback adds ~0.5–1 ms/query; significant on sub-3 ms strict queries, negligible on fuzzy. Measures user-perceived end-to-end latency, not search-engine latency._

## §3.2 contract: cross_language_recall@1

- Random samples (n=121): **0.983**
- Boosted samples (n=200): **0.985**

## By region

| Region | n | recall@1 | recall@5 | bgid_recall@1 | MRR | strict |
|---|---|---|---|---|---|---|
| BE-BRU | 492 | 0.963 | 0.990 | 0.963 | 0.976 | 0.994 |
| BE-VLG | 1388 | 0.933 | 0.980 | 0.933 | 0.952 | 0.999 |
| BE-WAL | 730 | 0.942 | 0.974 | 0.942 | 0.957 | 0.997 |

## By query variant

| Variant | n | recall@1 | recall@5 | bgid_recall@1 | MRR | strict |
|---|---|---|---|---|---|---|
| abbreviated | 299 | 0.970 | 0.997 | 0.970 | 0.983 | 0.983 |
| canonical | 500 | 0.974 | 0.996 | 0.974 | 0.985 | 0.998 |
| casing | 500 | 0.974 | 0.996 | 0.974 | 0.985 | 0.998 |
| cross_language | 311 | 0.987 | 0.997 | 0.987 | 0.992 | 1.000 |
| minimal | 500 | 0.774 | 0.910 | 0.774 | 0.829 | 1.000 |
| with_postal | 500 | 0.998 | 0.998 | 0.998 | 0.998 | 1.000 |

## By street primary language

| Language | n | recall@1 | recall@5 | bgid_recall@1 | MRR | strict |
|---|---|---|---|---|---|---|
| de | 16 | 0.938 | 1.000 | 0.938 | 0.969 | 1.000 |
| fr | 689 | 0.940 | 0.972 | 0.940 | 0.955 | 0.997 |
| nl | 1905 | 0.942 | 0.983 | 0.942 | 0.959 | 0.997 |

## Adversarial: by category

| Category | n | recall@1 | recall@5 | bgid_recall@1 | MRR | strict |
|---|---|---|---|---|---|---|
| cross_language | 10 | 0.900 | 1.000 | 0.900 | 0.950 | 1.000 |
| de_zone | 9 | 0.667 | 0.667 | 0.667 | 0.678 | 0.889 |
| facilites | 6 | 0.833 | 0.833 | 0.833 | 0.833 | 0.833 |
| hamlet | 30 | 0.400 | 0.433 | 0.400 | 0.420 | 0.900 |
| notability | 30 | 0.400 | 0.667 | 0.933 | 0.538 | 1.000 |
| special_chars | 10 | 0.900 | 1.000 | 0.900 | 0.950 | 0.900 |

## Known-failure tracking

| ID | Subcategory | State | Top-1 (bgid prefix) | Top-1 match_type |
|---|---|---|---|---|
| `adv-known-001` | rue-de-la-gare-brussels | **still_failing** | `geodata.wallonie.be/id/Streetname|7706419|1` | strict |
| `adv-known-002` | place-de-l-eglise-ambiguous | **still_failing** | `geodata.wallonie.be/id/Streetname|7736393|21` | strict |
| `adv-known-003` | anvers-grand-place-cross-region | **still_failing** | `geodata.wallonie.be/id/Streetname|7723083|58` | fuzzy |
