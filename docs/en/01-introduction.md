# 1. Introduction

## What crabfinder does

crabfinder turns Belgian addresses into geographic coordinates (latitude / longitude WGS84) and vice versa. It uses the official **BeSt Address** dataset, covering all three regions and ~6.8M addresses (as of the 2026-05 dump). Everything is local: no third-party calls, no quota, no data leaving your machine.

There are two concrete ways to use it:

### Mode 1 — online search (interactive)

A user types an address into a form. The service replies with a top-N ranked by relevance, in GeoJSON. Typical latency under 20 ms.

```text
Query:  "Grand-Place 1 1000 Bruxelles"
Reply:  top 10 candidate addresses, the actual Brussels Grand-Place
        ranked first, with lat/lng and all metadata (FR/NL street name,
        municipality, postal code, BeSt identifiers).
```

This is the right mode for an autocomplete or an interactive map. See [`04-online-search.md`](04-online-search.md).

### Mode 2 — batch geocoding (list processing)

A CSV file with clean column headers (`id`, `street`, `house_number`, `postal_code`). An enriched CSV out with lat/lng and quality flags per row, **same row count, same order** as the input.

```text
addresses.csv (3 rows)         →  addresses-geocoded.csv (3 rows)
id,street,house_number,postal_code   id,...,lat,lng,confidence,fallback_reason,...
1,Avenue Louise,32,1050              1,...,50.832,4.367,exact,,...
2,Grand-Place,,1000                  2,...,50.847,4.353,median_street,house_number_missing,...
3,Nonexistent Street,99,1000         3,...,,,not_found,no_match,...
```

This is the right mode for enriching an existing database, geocoding an Excel export, or feeding a spatial join. See [`03-batch.md`](03-batch.md).

## What crabfinder **does not** do

Worth knowing before adopting it:

- **No countries beyond Belgium.** The index is fixed on BeSt Address. No France, no Luxembourg, no Netherlands even across the border.
- **No geocoding of non-addressed places.** No POIs, no station names, no building landmark names. Searching for "Atomium" will match its postal address if it's in BeSt — not the Atomium-as-a-monument.
- **No public hosted API.** crabfinder is a binary you install and run yourself. No SaaS.
- **No authentication.** The service listens on `127.0.0.1:8080` by default. To expose it on the internet, front it with a reverse proxy + auth. Local or intra-LAN use is fine as-is.
- **No automatic data refresh.** When BeSt publishes a new dump, you download the new archives and rebuild the index (~2-3 min).

## Spatial precision: what you actually get

Depending on the case, crabfinder returns coordinates at different precision levels. See [`05-understanding-results.md`](05-understanding-results.md) for the details.

| Level | When | `confidence` field |
|---|---|---|
| Exact building | House number present in BeSt | `exact` |
| Street centroid (median) | House number absent or unknown | `median_street` |
| Exact building, fuzzy-matched street | Typo on the street name | `fuzzy_street` |
| No coordinates | Neither street nor postal code matched | `not_found` |

The recipient of a geocoded CSV can therefore sort rows by confidence and inspect the tail manually.

## Multilingual coverage

Bilingual addresses (Brussels, "facility" municipalities) are indexed in **all their variants** (FR, NL, DE when available). You can type either "Avenue Louise 1050" or "Louizalaan 1050" — the same address surfaces. German-speaking municipalities (Eupen, St. Vith…) are covered too.

## What's next

- First-time setup → [`02-installation.md`](02-installation.md).
- Already installed, want to geocode a CSV → [`03-batch.md`](03-batch.md).
- Want a REST API → [`04-online-search.md`](04-online-search.md).
