# 1. Introduction

## What crabfinder does

crabfinder turns Belgian addresses into geographic coordinates (latitude / longitude WGS84) and vice versa. It uses the official **BeSt Address** dataset, covering all three regions and ~6.9M addresses (as of the 2026-10 dump). Everything is local: no third-party calls, no quota, no data leaving your machine.

**BeSt Address is the only official source.** Every address, identifier, coordinate and label crabfinder returns comes from BeSt Address. OpenStreetMap can optionally be switched on (off by default) to *interpret* queries naming a hamlet (`--places`, at indexing) or a landmark (`--pois`, in the service). It only tells crabfinder where to look: it never adds to or alters an address, and is never returned as a result.

There are two concrete ways to use it:

### Mode 1: online search (interactive)

A user types an address into a form. The service replies with a top-N ranked by relevance, in GeoJSON. Typical latency a few milliseconds (p95 ~7 ms).

```text
Query:  "Grand-Place 1 1000 Bruxelles"
Reply:  top 10 candidate addresses, the actual Brussels Grand-Place
        ranked first, with lat/lng and all metadata (FR/NL street name,
        municipality, postal code, BeSt identifiers).
```

This is the right mode for a search box or an interactive map (search on submit: crabfinder doesn't complete half-typed words). See [`04-online-search.md`](04-online-search.md).

### Mode 2: batch geocoding (list processing)

A CSV file, either with split columns (`id`, `street`, `house_number`, `postal_code`) or with one free-text `address` column. An enriched CSV (or GeoJSON) out with lat/lng and quality flags per row, **same row count, same order** as the input.

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
- **No POIs as results.** Nothing but BeSt addresses comes out: no stations, shops or monuments as such. By default, place names are understood as far as BeSt knows them: municipalities, parts of municipalities, postal areas ("Spontin", "Han-sur-Lesse"). The optional OpenStreetMap helpers extend that to hamlets ("Maredsous", `--places`) and landmarks ("Manneken Pis", `--pois`), and even then the answer is the nearest BeSt address.
- **No public hosted API.** crabfinder is a binary you install and run yourself. No SaaS.
- **No authentication.** The service listens on `127.0.0.1:8080` by default. To expose it on the internet, front it with a reverse proxy + auth. Local or intra-LAN use is fine as-is.
- **No automatic data refresh.** When BeSt publishes a new dump, you run `scripts/refresh_best.sh` (download, rebuild ~1.5-3 min, check, swap) and restart the service.

## Spatial precision: what you actually get

Depending on the case, crabfinder returns coordinates at different precision levels. See [`05-understanding-results.md`](05-understanding-results.md) for the details.

| Level | When | `confidence` field |
|---|---|---|
| Exact building | House number present in BeSt | `exact` |
| Neighbouring building | House number absent from BeSt, a close number exists | `nearest_same_side`, `nearest_any_side` |
| Street centroid (median) | No house number, or none close enough | `median_street` |
| Building or street in a neighbouring commune | Nothing inside the given postal code, the same street just next door | `adjacent_commune` |
| Exact building, approximately matched street | Typo or partial match on the street name | `fuzzy_street` |
| BeSt address nearest to a place | The query only names a place (municipality, part of municipality, postal area; hamlets with `--places`) | `locality` |
| BeSt address nearest to a landmark | The query only names a landmark ("Manneken Pis", "Gare du Midi"), with `--pois` | `landmark` |
| No coordinates | Nothing plausible | `not_found` |

The recipient of a geocoded CSV can therefore sort rows by confidence and inspect the tail manually. Two more flags complete the picture: `ambiguous` (another, almost as good answer exists elsewhere) and `position_source` (whether the point was published by BeSt or estimated from the neighbours, for the ~140k Walloon addresses BeSt publishes without a position).

## Multilingual coverage

Bilingual addresses (Brussels, "facility" municipalities) are indexed in **all their variants** (FR, NL, DE when available). You can type either "Avenue Louise 1050" or "Louizalaan 1050": the same address surfaces. German-speaking municipalities (Eupen, St. Vith…) are covered too.

## What's next

- First-time setup → [`02-installation.md`](02-installation.md).
- Already installed, want to geocode a CSV → [`03-batch.md`](03-batch.md).
- Want a REST API → [`04-online-search.md`](04-online-search.md).
