<h1 align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/assets/crabfinder-lockup-dark.svg">
    <img alt="crabfinder" src="docs/assets/crabfinder-lockup.svg" width="420">
  </picture>
</h1>

A Belgium-only geocoder, written in Rust. Indexes the federal **BeSt
Address** open dataset (6.9M addresses, three regions) into a single
Tantivy index and serves forward, reverse and batch geocoding over HTTP
and from the command line.

**BeSt Address is the only source.** Every address, identifier,
coordinate and label crabfinder returns comes from BeSt. OpenStreetMap
can optionally be switched on (`--places`, `--pois`, both off by default)
to *understand* queries that name a hamlet or a landmark. It is only used
to decide where to look: the answer is still the nearest BeSt address,
flagged `confidence: locality` / `landmark`.

```
GET /geocode?q=Rue de la Loi 16 bte 3, B-1000 Bruxelles
GET /geocode?street=Grote Markt&housenumber=1&city=Antwerpen&lang=nl
GET /reverse?lat=50.8466&lng=4.3525&lang=de
GET /lookup?id=https://databrussels.be/id/address/210225/1
GET /validate?street=Av Louise&housenumber=32&postcode=1000&city=Ixelles
GET /autocomplete?q=rue de la l&focus_point=50.85,4.35
POST /batch/geocode            (CSV in, CSV or GeoJSON out, streamed)
POST /batch/jobs               (the same as a background job, for large files)
```

What it handles:

- **Belgian address formats**: `B-1050`, `1050-Ixelles`, box numbers
  (`bte`, `bus`, `box`, `12/3`), ranges (`12-14`), 4-digit house numbers
  (`Chaussée de Waterloo 1357 1180 Uccle`), streets with numbers in their
  name (`Rue du 11 Novembre 5`).
- **Cross-language queries**: FR/NL/DE names are interchangeable
  (`Anvers Grote Markt`, `Wetstraat 16 Bruxelles`).
- **Typos**: edit-distance matching on unknown street words
  (`Avenue Loiuse 32`).
- **Missing house numbers**: the nearest existing number on the same
  side of the street, else the street itself, with an explicit
  `confidence` and `house_number_delta`.
- **Wrong postal codes**: when nothing matches inside the given code,
  the address is found next door (`Avenue Louise 54 1000` → Avenue Louise
  54, 1050) and flagged `adjacent_commune`.
- **Exact names first**: `Markt` is Bruges' Markt, not every Grote
  Markt; `Grand-Place` is Brussels.
- **Municipalities and villages**: a query naming only a place
  (`Spontin`, `Celles Houyet`, `Han-sur-Lesse 5580`) returns the BeSt
  addresses at its centre, flagged `confidence: locality`. Optional
  OpenStreetMap helpers (off by default) extend the names understood to
  hamlets (`--places`: `Maredsous`) and landmarks (`--pois`: `Manneken
  Pis`, `Gare du Midi`, `Gravensteen Gent`).
- **Reverse interpolation**: `/reverse?interpolate=true` estimates the
  house number of a point lying between two known numbers of a street.
- **Positions for every address**: the ~140k Walloon addresses BeSt
  ships without coordinates are imputed (same number, interpolation,
  locality centroid) and flagged with `position_source`.
- **Old ids still resolve**: `/lookup` and the batch `best_id` column
  accept ids with or without version, from any earlier dump. A retired
  address leads to the current BeSt address that took its place: same
  number and box, same number, or the same position within 5 m.
- **Validation**: `/validate` and the batch `corrections` columns compare
  an input with the BeSt address it resolves to and say what differs
  (postal code, spelling, box, ...).
- **Which one?**: for an ambiguous query (a street and number without a
  municipality), `/geocode?alternatives=true` adds the best answer of
  each municipality, so a UI can offer one option per place.
- **Autocomplete**: `/autocomplete` suggests streets from any prefix of
  their name or of a later word (`loi` → Rue de la Loi), BeSt localities,
  then the street's buildings once a number is typed, in about 1 ms. With
  `focus_point` at the municipality, the expected street is among 10
  suggestions after 8 characters 95% of the time.
- **Large batches**: `/batch/geocode` streams its output 5,000 rows at a
  time; `/batch/jobs` (`--jobs-dir`) runs files of up to 1 GiB in the
  background, survives restarts and keeps the result for download.
- **Labels in any language**: `lang=fr|nl|de` and a ready-to-print
  `label` on every result.
- **Operations**: request deadlines, a concurrency limit, CORS,
  Prometheus `/metrics`, a one-command BeSt refresh, and a Docker Compose
  setup that downloads BeSt and builds the index by itself.
- **Protection** (off by default): API keys with per-key rights and rate
  limits, per-address rate limits, body limits.

## Quick start

Requires Rust 1.90+ and ~3 GB free disk.

```bash
# 1. Build everything (release mode: debug is too slow for Tantivy)
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

# 5. Batch a CSV (`id,street,house_number,postal_code` or `id,address`)
./target/release/crabfinder-batch --index ./index-current -i in.csv -o out.csv
```

Or with Docker, nothing else to install (`docker-compose.yml`: downloads
BeSt, builds the index, serves it on `127.0.0.1:8080`):

```bash
docker compose up -d && docker compose logs -f refresh
```

To index a bundle you already have:

```bash
./target/release/crabfinder-indexer --best-dir ./best-full-latest --output ./index-belgium \
    --notability-whitelist data/notability-whitelist-v1.yaml   # ~1.5 min
```

Optional OpenStreetMap helpers (hamlet and landmark names; BeSt remains
the only source of results): add `--places data/osm-places-be-v1.csv` to
the indexer (or `WITH_OSM=1 scripts/refresh_best.sh`) and `--pois
data/osm-pois-be-v1.csv` to the service.

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
| hamlets (adversarial; with the optional OSM `--places`) | 0.667 | **1.000** |
| iconic places (adversarial notability) | 0.933 | **1.000** |
| p95 / p99 `/geocode` latency | 19.0 / 51.1 ms | **7.2 / 11.9 ms** |

The promoted baseline [`reports/v3-baseline.md`](reports/v3-baseline.md)
(v3, schema v12, 4,365 queries) adds 500 `wrong_postal` queries (the
neighbouring commune's postal code): recall@1 0.004 before the
postal-code relaxation, **0.958** after. Against the v2 baseline
([`reports/v3-vs-v2.md`](reports/v3-vs-v2.md)): recall@1 0.964 → 0.965
(typos 0.988 → 0.996), no query worse, and p95 / p99 7.7 / 12.8 ms →
5.9 / 10.9 ms.

BeSt-only (the default, no OpenStreetMap aid) vs with both optional OSM
aids, on the same 4,365 queries
([`reports/v3-osm-aids.md`](reports/v3-osm-aids.md)):

| | BeSt only | + OSM `--places` / `--pois` |
|---|---|---|
| recall@1 | 0.965 | **0.971** |
| hamlets (30, adversarial) | 0.833 | **1.000** |
| landmarks (26, `Manneken Pis`, `Gare du Midi`, …) | 0.269 | **1.000** |
| p95 / p99 latency | 5.9 / 10.9 ms | 5.9 / 13.7 ms |

Either way every result is a BeSt address; the OSM aids only change
which BeSt addresses a hamlet or landmark name leads to.

Latency is HTTP loopback, 8 concurrent clients. The `minimal` variant
(street + number, no postal code or municipality: 0.792) is bounded by
genuine ambiguity: most Belgian street names exist in several
municipalities.

Run an eval yourself: see [`docs/eval.md`](docs/eval.md).

### Benchmark against other geocoders

`crabfinder-eval --benchmark` sends the same queries to other Belgian
geocoders and scores the distance of each top-1 to a reference point
([`reports/benchmark-2026-10-16.md`](reports/benchmark-2026-10-16.md)).
The fair comparison is the real-world set: 1,510 addresses from public
open-data lists (schools, pharmacies, post offices, shops, care
facilities), each with the publisher's own coordinate, which is often an
entrance rather than the BeSt point. Share of top-1 answers within 100 m:

| Geocoder | Covers | Within 100 m |
|---|---|---|
| crabfinder | Belgium (1,510 queries) | **0.929** |
| BePelias (Smals, self-hosted) | Belgium (1,510) | 0.908 |
| PhacochR | Belgium (1,510) | 0.907 |
| NGI-IGN national geocoder (`geosearch.ngi.be`) | Belgium (1,510) | 0.836 |
| Paradigm, Brussels regional geocoder | Brussels (496) | 0.925 |
| Wallonia regional geocoder (SPW) | Wallonia (550) | 0.847 |
| Flemish Geolocation API | Flanders + Brussels (960) | 0.505 |

Regional services are only asked the queries in their region, so compare
them to crabfinder within that region: Brussels 0.935, Wallonia 0.885,
Flanders 0.974 (Flemish API 0.963 there; its low overall figure comes
from French-language Brussels addresses it barely covers). The synthetic
and adversarial sets come from BeSt and from crabfinder's own failure
history, so they favour crabfinder and are not a fair comparison. Competitor
answers are only compared, never used.

## Docs

User-facing guides (install, run, batch, troubleshoot):

- [docs/en/](docs/en/README.md): English
- [docs/fr/](docs/fr/README.md): Français

Technical references:

| Doc | What |
|---|---|
| [docs/architecture.md](docs/architecture.md) | Crate layout, index schema, query pipeline, scoring |
| [docs/api.md](docs/api.md) | HTTP endpoints, parameters, response fields, flags |
| [docs/eval.md](docs/eval.md) | Eval methodology + how to run |

## Inputs

- **BeSt zip bundle**: the federal Belgian address dataset, downloaded
  by `scripts/refresh_best.sh` (or drop the `*Address*.zip`,
  `Municipality*`, `PostalInfo*`, `StreetName*`, `PartOfMunicipality*`
  zips into one directory and pass `--best-dir`).
- **Notability whitelist** (`data/notability-whitelist-v1.yaml`): a
  curated `building_group_id → multiplier` map that lifts iconic
  toponyms (Grand-Place, Atomium, …) above look-alikes elsewhere.
- **Abbreviation dictionary** (`data/street-type-abbreviations-v1.yaml`):
  expands user-typed abbreviations at query time
  (`Av. Louise` → `Avenue Louise`, `Stationsstr.` → `Stationsstraat`).

Both are optional; without them the engine still runs, with lower
scores on notability-sensitive and abbreviated queries.

Optional OpenStreetMap helpers, off by default. They are never a source
of results, only of query understanding:

- **OSM places** (`data/osm-places-be-v1.csv`, indexer `--places`):
  hamlet and village names (`scripts/build_osm_places.py`), so
  `Maredsous` finds the BeSt addresses of that hamlet.
- **OSM landmarks** (`data/osm-pois-be-v1.csv`, service `--pois`):
  ~5,000 sights, stations, castles, town halls… with a Wikidata entry
  (`scripts/build_osm_pois.py`), so `Manneken Pis` finds the BeSt
  addresses next to it. Loaded at startup, no reindex needed.

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
- `data/osm-places-be-v1.csv` and `data/osm-pois-be-v1.csv` contain
  data from © OpenStreetMap contributors, available under the
  [Open Database License (ODbL)](https://opendatacommons.org/licenses/odbl/).
  Indices built with `--places` contain derived data (place names in
  `localities-v1.json`) under the same licence; addresses and payloads
  are BeSt only.
