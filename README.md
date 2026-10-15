# crabfinder

A Belgium-only geocoder, written in Rust. Indexes the federal **BeSt
Address** open dataset (6.9M addresses, three regions) into a single
Tantivy index and serves forward, reverse and batch geocoding over HTTP
and from the command line.

```
GET /geocode?q=Rue de la Loi 16 bte 3, B-1000 Bruxelles
GET /geocode?street=Grote Markt&housenumber=1&city=Antwerpen&lang=nl
GET /reverse?lat=50.8466&lng=4.3525&lang=de
GET /lookup?id=https://databrussels.be/id/address/210225/1
POST /batch/geocode            (CSV in, CSV or GeoJSON out)
```

What it handles:

- **Belgian address formats** — `B-1050`, `1050-Ixelles`, box numbers
  (`bte`, `bus`, `box`, `12/3`), ranges (`12-14`), 4-digit house numbers
  (`Chaussée de Waterloo 1357 1180 Uccle`), streets with numbers in their
  name (`Rue du 11 Novembre 5`).
- **Cross-language queries** — FR/NL/DE names are interchangeable
  (`Anvers Grote Markt`, `Wetstraat 16 Bruxelles`).
- **Typos** — edit-distance matching on unknown street words
  (`Avenue Loiuse 32`).
- **Missing house numbers** — the nearest existing number on the same
  side of the street, else the street itself, with an explicit
  `confidence` and `house_number_delta`.
- **Wrong postal codes** — when nothing matches inside the given code,
  the address is found next door (`Avenue Louise 54 1000` → Avenue Louise
  54, 1050) and flagged `adjacent_commune`.
- **Exact names first** — `Markt` is Bruges' Markt, not every Grote
  Markt; `Grand-Place` is Brussels.
- **Hamlets and villages** — OpenStreetMap places (`Maredsous`,
  `Spontin`) resolve to the locality when no street is named.
- **Positions for every address** — the ~140k Walloon addresses BeSt
  ships without coordinates are imputed (same number, interpolation,
  locality centroid) and flagged with `position_source`.
- **Labels in any language** — `lang=fr|nl|de` and a ready-to-print
  `label` on every result.
- **Operations** — request deadlines, a concurrency limit, CORS,
  Prometheus `/metrics`, and a one-command BeSt refresh.

## Quick start

Requires Rust 1.90+ and ~3 GB free disk.

```bash
# 1. Build everything (release mode — debug is too slow for Tantivy)
cargo build --release

# 2. Download BeSt, build index-<date>, validate it, point index-current at it
scripts/refresh_best.sh

# 3. Start the HTTP service
./target/release/crabfinder-service \
    --index ./index-current \
    --abbreviation-dict data/street-type-abbreviations-v1.yaml \
    --port 8080 &

# 4. Query
curl "http://127.0.0.1:8080/health"
curl "http://127.0.0.1:8080/geocode?q=Grand-Place+1000+Bruxelles&limit=3"
curl "http://127.0.0.1:8080/reverse?lat=50.8466&lng=4.3525&limit=1"

# 5. Batch a CSV (id,street,house_number,postal_code — or id,address)
./target/release/crabfinder-batch --index ./index-current -i in.csv -o out.csv
```

To index a bundle you already have:

```bash
./target/release/crabfinder-indexer --best-dir ./best-full-latest --output ./index-belgium \
    --notability-whitelist data/notability-whitelist-v1.yaml \
    --places data/osm-places-be-v1.csv          # ~1.5 min
```

The service binds IPv4 loopback by default (`--host 0.0.0.0` in
containers), so hit it with `127.0.0.1`, not `localhost`. It accepts
connections once the reverse-geocoding R-tree is built (~5 s).

## Quality

On the BeSt dump of 2026-10-15 (`docs/eval.md`). v1 vs v2 on the shared
set (`eval/belgium-2026-10-15-v2.json` + 98 hand-written adversarial
cases, 3,840 queries; [`reports/v2-vs-pre-v2.md`](reports/v2-vs-pre-v2.md)):

| | v1 (pre-v2 baseline) | v2 |
|---|---|---|
| recall@1 | 0.842 | **0.971** |
| recall@10 | 0.910 | **0.991** |
| typo variant recall@1 | 0.366 | **0.988** |
| 4-digit house numbers | 0.158 | **1.000** |
| messy punctuation (`Street, 10, B-1150, City`) | 0.884 | **1.000** |
| hamlets (adversarial) | 0.667 | **1.000** |
| landmarks (adversarial notability) | 0.933 | **1.000** |
| p95 / p99 `/geocode` latency | 19.0 / 51.1 ms | **7.2 / 11.9 ms** |

The promoted baseline [`reports/v2-baseline.md`](reports/v2-baseline.md)
adds 500 `wrong_postal` queries (the neighbouring commune's postal code):
recall@1 0.004 before the postal-code relaxation, **0.958** after; 0.969 overall
on its 4,342 queries.

Latency is HTTP loopback, 8 concurrent clients. The `minimal` variant
(street + number, no postal code or municipality: 0.792) is bounded by
genuine ambiguity — most Belgian street names exist in several
municipalities.

Run an eval yourself: see [`docs/eval.md`](docs/eval.md).

## Docs

User-facing guides (install, run, batch, troubleshoot):

- [docs/en/](docs/en/README.md) — English
- [docs/fr/](docs/fr/README.md) — Français

Technical references:

| Doc | What |
|---|---|
| [docs/architecture.md](docs/architecture.md) | Crate layout, index schema, query pipeline, scoring |
| [docs/api.md](docs/api.md) | HTTP endpoints, parameters, response fields, flags |
| [docs/eval.md](docs/eval.md) | Eval methodology + how to run |

## Inputs

- **BeSt zip bundle** — the federal Belgian address dataset, downloaded
  by `scripts/refresh_best.sh` (or drop the `*Address*.zip`,
  `Municipality*`, `PostalInfo*`, `StreetName*`, `PartOfMunicipality*`
  zips into one directory and pass `--best-dir`).
- **OSM places** (`data/osm-places-be-v1.csv`) — hamlets, villages and
  localities extracted from OpenStreetMap by
  `scripts/build_osm_places.py`; powers locality queries.
- **Notability whitelist** (`data/notability-whitelist-v1.yaml`) — a
  curated `building_group_id → multiplier` map that lifts iconic
  toponyms (Grand-Place, Atomium, …) above look-alikes elsewhere.
- **Abbreviation dictionary** (`data/street-type-abbreviations-v1.yaml`)
  — expands user-typed abbreviations at query time
  (`Av. Louise` → `Avenue Louise`, `Stationsstr.` → `Stationsstraat`).

All three extra inputs are optional; without them the engine still runs,
with lower scores on locality, notability-sensitive and abbreviated
queries.

## Development

```bash
cargo test --workspace                          # unit + end-to-end, no BeSt data needed
cargo clippy --workspace --all-targets -- -D warnings
scripts/coverage.sh                             # line coverage, gate 95 %
```

## License

MIT OR Apache-2.0.

Data:

- BeSt address data is © the Belgian federal and regional authorities;
  redistribute per their open-data licence.
- `data/osm-places-be-v1.csv` contains data from © OpenStreetMap
  contributors, available under the
  [Open Database License (ODbL)](https://opendatacommons.org/licenses/odbl/).
  Indices built with `--places` contain derived data under the same
  licence.
