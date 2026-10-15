# Architecture

crabfinder is a Belgium-only geocoder backed by the federal BeSt Address
dump (~6.9M addresses, Brussels + Flanders + Wallonia), optionally
enriched with OpenStreetMap place names. One index, multilingual,
forward / reverse / batch.

## Crates

```
crates/
├── crabfinder-core          Domain types (Address, BestId, MultilingualName, Region,
│                            PositionSource, Address::display_name, …)
├── crabfinder-parser        Free text → ParsedQuery (+ alternative readings)
├── crabfinder-indexer       BeSt XML (+ OSM places CSV) → Tantivy index + side-files (lib + thin bin)
├── crabfinder-service       Resolver + Axum HTTP API (lib + thin bin)
├── crabfinder-batch         Standalone CSV CLI over the same resolver (lib + thin bin)
├── crabfinder-eval          Eval-set generator, HTTP runner, metrics, reports, diffs (lib + thin bin)
└── crabfinder-test-support  Test-only: AddressBuilder, fixtures, TestIndex, BeSt bundle writer
```

Dependency flow: `core` ← `parser`, `indexer` ← `service` ← `batch`,
`eval`. `test-support` depends on `core` + `indexer` only and is a
dev-dependency everywhere. Every binary's `main.rs` is argument parsing
and startup; the logic lives in the libraries (and is what the coverage
gate measures).

The indexer exposes `index::CrabSchema` and `register_tokenizers`, so
every reader uses the same field handles and the same `crab` analyzer.

## Data flow

```
BeSt zip bundle                     data/osm-places-be-v1.csv (optional, ODbL)
        │                                   │
        ▼                                   ▼
crabfinder-indexer  pass 1: dictionaries (Municipality, PostalInfo, StreetName,
                            PartOfMunicipality)
                    pass 2: stream Addresses, join, reproject Lambert72→WGS84,
                            attach nearest OSM place, notability multiplier
                    then:   impute the positions BeSt leaves at `0 0`
        │
        ▼
index dir/          Tantivy segments
                    .crab-meta.json       schema version guard + build time
                    mid-street-v1.bin     (street, postal) → median-number point
                    localities-v1.json    named places → centre + postal codes
        │
        ▼
AppState::load      open index, check sidecar, build R-tree + address-count
                    index, load abbreviations / mid-street / localities
        │
        ├── crabfinder-service  GET /geocode /reverse /building/{id}/units /health /metrics,
        │                       POST /batch/geocode
        ├── crabfinder-batch    CSV → CSV / GeoJSON
        └── crabfinder-eval     --batch-mode (in-process)
```

## Index (schema v9)

`crabfinder_indexer::meta::SCHEMA_VERSION` is **9**; every reader checks
`.crab-meta.json` and refuses a mismatch. Recent bumps:

- **v7** — `mid-street-v1.bin` side-file (lower-median house number per
  street and postal code) for `Confidence::MedianStreet`.
- **v8** — `house_number` raw field uppercased (`32a` → `32A`); OSM place
  names in the payload (`Address::place_name`); `localities-v1.json`.
- **v9** — positions BeSt publishes as Lambert `0 0` are imputed instead
  of reprojected into France; `Address::position_source` + a FAST
  `position_source` field.

Fields (`CrabSchema::build`):

| Field | Type | Used by |
|---|---|---|
| `best_id` | raw, stored | hydrate |
| `searchable` | multi-valued TEXT, `crab` analyzer, no fieldnorms | street/municipality word matching |
| `lat`, `lon` | f64 FAST + stored | response, R-tree, area queries |
| `notability_score` | f64 FAST | `tweak_score` boost |
| `position_source` | u64 FAST | R-tree skips non-point-accurate positions |
| `house_number` | raw (uppercased), stored | house-number filter |
| `box_number` | raw, stored | unit preference |
| `postal_code` | raw, stored | postal filter |
| `building_group_id` | raw, stored | dedup, `/building`, street listing (regex on `{street}\|`) |
| `status`, `region` | raw, stored | region filter, hydrate |
| `payload` | stored | full `Address` JSON |

`searchable` holds FR/NL/DE street, municipality, part-of-municipality
and postal names together (per-language fields broke cross-language
queries like "Anvers Grote Markt"). The analyzer is `SimpleTokenizer →
LowerCaser → AsciiFoldingFilter`; hyphens are hard separators.
OSM place names are deliberately **not** in `searchable`: they caused
false strict matches. They are reached through the localities file.

### Position imputation (`impute.rs`)

The 2026-10-15 dump has 140,263 addresses (all Walloon) at Lambert
`0 0`. The pipeline defers them; once every positioned address is
streamed, `PositionImputer` assigns, in order:

| Source | Rule | Count |
|---|---|---|
| `same_number` | mean of positioned units with the same leading number, street and postal code | 44,909 |
| `interpolated` | between the nearest lower/higher numbers, same side first | 87,293 |
| `street` | street's lower-median point (no leading number) | 0 |
| `locality` | centroid of the part of municipality, else municipality | 8,061 |

`/reverse` and locality centroids only use point-accurate sources
(measured, same_number, interpolated).

### OSM places (`places.rs`) and localities (`localities.rs`)

`scripts/build_osm_places.py` extracts `place=village|hamlet|locality|
isolated_dwelling` nodes. With `--places`, each address gets the nearest
place within 3 km (village), 1.5 km (hamlet) or 1 km (locality,
isolated dwelling), unless BeSt already names it.

`localities-v1.json` (JSON) lists every name a query can mean without a
street — municipalities, Walloon parts of municipality, Flemish postal
areas, OSM places — with a centre (address mean, or the OSM node) and
postal codes. The service loads it into a name → localities map
(exact and name-prefix lookup).

### Notability

`data/notability-whitelist-v1.yaml` (`building_group_id → multiplier`)
is joined at index time into `notability_score` (default 1.0) and
multiplied into BM25 at query time, lifting landmarks (Grand-Place,
Atomium, …) above namesakes.

## Resolver (`crabfinder_service::resolver`)

`search(AddressQuery, SearchOptions, ResolverContext) → CandidateSet` is
the one pipeline behind `/geocode`; `resolve` (= `search` with limit 1,
reduced to a `ResolvedAddress`) serves batch rows, the batch CLI and eval
batch mode.

1. **Parse** (`crabfinder-parser`): street tokens, house number, box,
   4-digit postal code, plus up to 3 alternative readings (a 4-digit
   number as house number vs postal code, a number inside the street
   name, …). Normalization is left to the analyzer.
2. **Locality step**: the words name a place (whole name, or inner place
   + containing locality / province, matching postal code if any). An
   OSM centre → nearest buildings (`Tier::Area`); a centroid-only
   locality → its name matched inside its area (`Tier::Locality`).
   `Confidence::Locality`.
3. **Strict tiers, per reading**, first with hits wins:

   | Tier | Query | Confidence |
   |---|---|---|
   | `Exact` | strict street + number | `exact` |
   | `Typo` | `FuzzyTermQuery` per word (distance 0/1/2 for <5 / 5–8 / ≥9 chars) + number | `fuzzy_street` |
   | `Nearest` | street listing → closest number, same parity first, within max(50, n/2) | `nearest_same_side` / `nearest_any_side` |
   | `Street` | strict street, no number | `median_street` (point from `mid-street-v1.bin`) |
   | `TypoStreet` | typo street, no number | `fuzzy_street` |

   `mid_street_fallback = false` skips the street-level tiers.
4. **Locality-suffix readings**: `<street> <place>` where the place isn't
   in the street's text (an OSM hamlet, another commune): the street
   searched inside the place's area (≥ 3 km), focused on its centre.
5. **OR tiers**: any word, with then without the number
   (`fuzzy_street`).

The strict query is a phrase + AND-of-OR-forms (`analyze` + abbreviation
expansion, `av` → `avenue`) with an adjacent-pair bonus; postal code,
region, house number and area are `Must` filters. Scores are
`BM25 × notability × (1 + 0.005 × ln(1 + street address count))`.
Each tier over-fetches `limit × 20` (≤ 2000) rows, applies focus decay
(`exp(-d/25 km)` for `focus_point`, `exp(-d/1 km)` around a locality
centre), collapses by `building_group_id` (`dedup.rs`: max score, lowest box
as representative — the resolver swaps in the unit with the requested
box), and hydrates the payload. `CandidateSet::ambiguity` flags near-ties (≥ 0.9) on different
streets.

## HTTP service (`crabfinder_service::http`)

`router_with(AppState, RouterOptions)`:

- **Handlers** run their Tantivy work on the blocking pool
  (`http::blocking`), so the async runtime never stalls.
- **Guard** middleware: a deadline (`--request-timeout-ms`; batch:
  `--batch-timeout-secs`) covering both waiting for one of
  `--max-concurrency` shared slots and handling; past it, JSON 503.
- **Telemetry** (`telemetry.rs`): per-route counters and latency
  histograms, rejected requests, search outcomes by tier / confidence /
  match type (recorded in `resolver::search`), over-fetch-cap hits;
  served at `/metrics`. The `metrics` macros are no-ops in the CLIs.
- `TraceLayer` request spans; CORS (`--cors-origin`, default any).

## Spatial index (`/reverse`)

At startup `SpatialIndex` walks every doc's `lat`/`lon` FAST columns
(skipping non-point-accurate positions) and bulk-loads an
`rstar::RTree` (~100 MB, ~5–6 s for the full index; the service binds
after). Queries over-search, then re-sort by Haversine distance.

## Batch (`crabfinder_service::batch`)

`io` reads CSV (delimiter sniffed; clean columns or free-text
`address`) and writes CSV or GeoJSON; `dedup` resolves each distinct
input once and fans out with rayon; `http` is the `POST /batch/geocode`
façade (streamed body). `crabfinder-batch` is a peer CLI opening the
index directly (mmap read-sharing makes it safe next to a live service).

## What the index does *not* contain

- Per-language searchable fields (removed in v3).
- A prefix / autocomplete field (v5 attempt removed, see
  `reports/archive/phase7a-removed/`).
- OSM names in `searchable` (see above).
- A persisted R-tree: it is rebuilt at every startup.
