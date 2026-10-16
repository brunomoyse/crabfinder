# User documentation — crabfinder (EN)

Welcome. This documentation is written for someone who wants to **use** crabfinder, not develop it. Target reader: a GIS user comfortable with a command line, who can read a CSV, with no need to know Rust or Tantivy.

> French version: see [`../fr/`](../fr/README.md).

## crabfinder in one sentence

A Belgian geocoding service backed by the federal **BeSt Address** dataset (6.9M addresses across Brussels + Flanders + Wallonia). Two usage modes:

1. **Online search** — one query, one ranked top-N. For interactive integration (form, map).
2. **Batch geocoding** — CSV in, enriched CSV or GeoJSON out. For processing address lists in one go.

Everything runs locally. No external calls. Single ~2.2 GB index on disk.

**BeSt Address is the only source.** Every address, identifier, coordinate and label crabfinder returns comes from BeSt. OpenStreetMap is an optional aid, off by default, for understanding queries that name a hamlet (`--places`, indexer) or a landmark (`--pois`, service): it only decides where to look. It never adds or alters an address and is never returned as a result — the answer is still the nearest BeSt address, flagged `confidence: locality` / `landmark`.

## Quickstart

```bash
# 1. Build the binaries (release is mandatory)
cargo build --release

# 2. Build the index (~1.5-3 min, ~2.2 GB disk)
./target/release/crabfinder-indexer \
    --best-dir ./best-full-latest \
    --output ./index-belgium \
    --notability-whitelist data/notability-whitelist-v1.yaml

# 3a. Online search — start the service
./target/release/crabfinder-service \
    --index ./index-belgium \
    --abbreviation-dict data/street-type-abbreviations-v1.yaml &

curl "http://127.0.0.1:8080/geocode?q=Avenue+Louise+32+1050"

# 3b. Batch — from a CSV, no service required
./target/release/crabfinder-batch \
    --index ./index-belgium \
    --abbreviation-dict data/street-type-abbreviations-v1.yaml \
    --input addresses.csv \
    --output addresses-geocoded.csv
```

If any of these commands doesn't work, jump straight to [`02-installation.md`](02-installation.md).

## Table of contents

| File | Content | Read when |
|---|---|---|
| [01-introduction.md](01-introduction.md) | What crabfinder does, what it doesn't | Before installing |
| [02-installation.md](02-installation.md) | Install Rust, get BeSt data, build the index, service flags, refresh | First-time setup |
| [03-batch.md](03-batch.md) | Geocode a CSV (CLI + HTTP endpoint) | Main use case for list processing |
| [04-online-search.md](04-online-search.md) | HTTP API: `/geocode`, `/validate`, `/reverse`, `/lookup`, `/building/{id}/units`, `/metrics` | When integrating into an app |
| [05-understanding-results.md](05-understanding-results.md) | Reading outputs: `confidence`, `fallback_reason`, `position_source`, `ambiguous`, `score` | Before acting on a geocoded CSV |
| [06-faq.md](06-faq.md) | Common errors, performance, troubleshooting | When something goes wrong |

## Documentation conventions

- All commands assume you're at the repo root `crabfinder/`.
- Paths start with `./` when relative to the repo root.
- Performance figures (indexing time, latency…) are ballpark estimates on a recent laptop (Apple Silicon or 8-core x86_64 with SSD).
- When a feature has both a CLI flag and an HTTP option (e.g. `--no-mid-street` vs `mid_street_fallback=false`), both are documented in the same place.

## License

crabfinder code: MIT OR Apache-2.0. BeSt Address data: © Belgian federal authorities, open-data licence (redistribute per their terms). Optional OpenStreetMap helpers `data/osm-places-be-v1.csv` and `data/osm-pois-be-v1.csv`: © OpenStreetMap contributors, ODbL 1.0.
