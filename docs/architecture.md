# Architecture

crabfinder is a Belgium-only geocoder backed by the federal BeSt Address
dump (~6.9M addresses, Brussels + Flanders + Wallonia). One index,
multilingual, forward / reverse / batch.

## Data sources

BeSt Address is the only source of what crabfinder returns: every
address, identifier, coordinate and label in a response, and every
document in the index, is BeSt's (positions BeSt lacks are imputed from
BeSt positions, `impute.rs`).

OpenStreetMap is an optional, off-by-default aid to *interpret* queries,
never a source of results:

- `--places` (indexer): hamlet and village names and their nodes go into
  `localities-v1.json` only, to recognise "Maredsous" and decide where to
  search.
- `--pois` (service): landmark names and positions, held in memory, to
  recognise "Manneken Pis".

Either way the answer is the nearest BeSt address with its BeSt position
(`Confidence::Locality` / `Landmark`). The `/geocode` `locality` echo
names the place as understood, with `source: osm`, and no OSM position.

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
BeSt zip bundle                     data/osm-places-be-v1.csv (optional, off by default)
        │                                   │
        ▼                                   ▼
crabfinder-indexer  pass 1: dictionaries (Municipality, PostalInfo, StreetName,
                            PartOfMunicipality)
                    pass 2: stream Addresses, join, reproject Lambert72→WGS84,
                            notability multiplier; OSM place → localities file only
                    then:   impute the positions BeSt leaves at `0 0`
        │
        ▼
index dir/          Tantivy segments
                    .crab-meta.json       schema version guard + build time
                    mid-street-v1.bin     (street, postal) → median-number point
                    localities-v1.json    named places → centre + postal codes
                    history-v1.json.gz    retired / reserved / rejected addresses (v12)
        │
        ▼
AppState::load      open index, check sidecar, build R-tree + address-count
                    index, load abbreviations / mid-street / localities
        │
        ├── crabfinder-service  GET /geocode /reverse /lookup /validate
        │                       /building/{id}/units /health /metrics,
        │                       POST /batch/geocode
        ├── crabfinder-batch    CSV → CSV / GeoJSON
        └── crabfinder-eval     --batch-mode (in-process)
```

## Index (schema v12)

`crabfinder_indexer::meta::SCHEMA_VERSION` is **12**; every reader checks
`.crab-meta.json` and refuses a mismatch. Recent bumps:

- **v7**: `mid-street-v1.bin` side-file (lower-median house number per
  street and postal code) for `Confidence::MedianStreet`.
- **v8**: `house_number` raw field uppercased (`32a` → `32A`); OSM place
  names in the payload (`Address::place_name`); `localities-v1.json`.
- **v9**: positions BeSt publishes as Lambert `0 0` are imputed instead
  of reprojected into France; `Address::position_source` + a FAST
  `position_source` field.
- **v10**: raw `street_exact` field for the exact-name bonus.
- **v11**: the OSM `Address::place_name` payload field (v8) is gone:
  addresses carry BeSt data only.
- **v12**: raw `best_oid` field (the id without its version) and the
  `history-v1.json.gz` side-file. BeSt's `rejected` status (1,406
  Brussels addresses) is no longer misread as `current`, so those
  addresses are no longer searchable.

### History side-file (`history.rs`, v12)

The status filter keeps `current` and `proposed` addresses in the index.
It writes the others (`retired`, `reserved`, `rejected`: 289,641 in the
2026-10-15 dump, nearly all Flemish history) to `history-v1.json.gz`.
Each record keeps:

- the versioned id and the status;
- the street object, house number, box and postal code;
- `endLifeSpanVersion`;
- the last position, reprojected.

The service loads the file into a map keyed by object key, about 1 s
at startup. `/lookup` and the batch `best_id` column use it to resolve
old ids. The successor (`same_number_box`, then `same_number`, then
`same_position` within 5 m) is derived at lookup time from the current
index, so it never goes stale against the dump it was built with.

The indexer refuses an output directory that already holds an index
(Tantivy would append a second copy) unless `--overwrite`.

Fields (`CrabSchema::build`):

| Field | Type | Used by |
|---|---|---|
| `best_id` | raw, stored | hydrate, `/lookup` |
| `best_oid` | raw: `{namespace}/{objectIdentifier}` (v12) | `/lookup` of an id without, or with another, version |
| `searchable` | multi-valued TEXT, `crab` analyzer, no fieldnorms | street/municipality word matching |
| `street_exact` | raw, multi-valued: each street name form analyzed and space-joined (`grand place`) | exact-name bonus |
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
isolated_dwelling` nodes. With `--places`, each address is matched to the
nearest place within 3 km (village), 1.5 km (hamlet) or 1 km (locality,
isolated dwelling), unless BeSt already names it, only to give the
place its postal codes in the localities file; the address itself is
left untouched.

`localities-v1.json` (JSON) lists every name a query can mean without a
street: municipalities, Walloon parts of municipality, Flemish postal
areas, and with `--places` OSM places, with a centre (address mean, or
the OSM node) and postal codes. Centres only steer the search; they are
never returned. The service loads it into a name → localities map
(exact and name-prefix lookup).

### Notability

`data/notability-whitelist-v1.yaml` (`building_group_id → multiplier`)
is joined at index time into `notability_score` (default 1.0) and
multiplied into BM25 at query time, lifting landmarks (Grand-Place,
Atomium, …) above namesakes. Most landmark squares get 3.0; Brussels'
Grand-Place, Antwerp's Grote Markt and Bruges' Markt get 3.5, so their
bare names mean them rather than Tournai's Grand'Place or Leuven's Grote
Markt.

## Service modules (`crabfinder-service/src`)

| Module | Role |
|---|---|
| `state` | `AppState::load`: open the index, check the sidecar, build every in-memory structure below |
| `resolver` | `search` (the tiered pipeline, below) and `resolve` (top-1, for batch) |
| `query_builder` | Tantivy queries: strict, typo, OR, area/bbox filters; `analyze` (the `crab` analyzer, query side) |
| `abbreviation` | Query-time `av` → `avenue` expansion from `street-type-abbreviations-v1.yaml` |
| `addr_count` | Per-doc street address count, the ranking tiebreak |
| `dedup` | Over-fetch, then collapse hits by `building_group_id` |
| `focus` | `focus_point` parsing and distance decay |
| `hydrate` | Stored payload → `HydratedHit` / `Address` |
| `history` | `history-v1.json.gz` and `resolve_id`: an id's status today and its derived successor, for `/lookup` and the batch `best_id` column (v3 Phase 11.4) |
| `mid_street` | `mid-street-v1.bin`: the street's median-number point (`median_street`) |
| `localities` | `localities-v1.json`: place name → localities, postal-code centres |
| `landmarks` | Optional OSM landmarks (`--pois`), query interpretation only |
| `spatial` | R-tree over point-accurate positions for `/reverse` |
| `interpolate` | `/reverse?interpolate=true` house-number estimate (pure geometry) |
| `telemetry` | Prometheus series for `/metrics` |
| `validate` | `/validate` and the batch `corrections` columns: input vs. the resolved BeSt address, field by field (v3 Phase 11.2) |
| `suggest` | `/autocomplete` (v3 Phase 11.5): the in-memory name index built at startup, and the prefix lookup |
| `http` | Router, guard (deadline, concurrency), CORS and the handlers |
| `batch` | CSV/GeoJSON I/O, intra-batch dedup, rayon fan-out, the chunked runner (`run`), `POST /batch/geocode` |
| `jobs` | Asynchronous batch jobs (`--jobs-dir`, v3 Phase 11.6): queue, workers, checkpoints, resume, TTL |

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
   Then the **village words dropped** (`without_city_words`, v3): when
   the trailing words name a locality *of the query's postal code*, they
   only say where in the code the street is, and BeSt may file it under
   another part. The street is searched without them, inside the postal
   code ("Rue Rogier 44, 5000 Beez"). The same reading also replaces a
   step-3 nearest-number or street-level answer when it finds the exact
   number (under the step-6 rules: "Chaussée de Marche 797, 5100 Jambes",
   where 797 isn't on the Jambes stretch).
5. **Landmark step** (`search_landmark`, `--pois`): no house number and
   the words are a landmark name, or a landmark + a locality within
   15 km of it (`landmarks::LandmarkIndex`, keyed by every analyzed
   name form and each side of a bilingual `"A - B"` name). A postal code
   must have its centre within 15 km. Among namesakes: nearest the focus
   point, else kind rank (attractions, castles/monuments, stations and
   institutions, museums, other heritage, churches). Answered like an
   OSM locality (`LocalityKind::Landmark`, rings up to 600 m,
   `Tier::Area`), labelled `Confidence::Landmark`. Loaded by the service,
   not indexed: refreshing the CSV needs no reindex.
6. **Postal-code relaxation** (`search_postal_relaxed`): nothing matched
   inside the query's postal code → the strict tiers without it, inside
   20 km of the code's centre (`indexer::localities::postal_centres`)
   with a 5 km distance decay, labelled `adjacent_commune` /
   `postal_code_mismatch`. It also replaces a step-3 answer when that is
   street-level, a nearest number on another street object, or one ≥ 20
   off, and the exact number exists across the boundary; and a parser
   alternative that doesn't match its number exactly. Skipped for the
   hard `postal_code=` filter (`AddressQuery::postal_code_strict`).
   After it, two last strict readings (v3), which come later because the
   same street across the boundary is the likelier answer:
   - a leading French street type BeSt doesn't have, dropped
     (`without_leading_type`: "Rue Faubourg de Charleroi" is *Faubourg de
     Charleroi*);
   - the typo tiers with "unknown" meant *inside the postal code*
     (`search_local_typo`). A word found elsewhere in Belgium but nowhere
     in the code is corrected too: "Schildknaapstraat 19, 1000" is the
     Brussels *Schildknaapsstraat*, which Koksijde's *Schildknaapstraat*
     hides from the regular typo tier.
7. **OR tiers**: any word, with then without the number
   (`fuzzy_street`).

The strict query is a phrase + AND-of-OR-forms (`analyze` + abbreviation
expansion, `av` → `avenue`) with an adjacent-pair bonus and an
exact-name bonus (a constant 10 when a leading run of the street words
equals a `street_exact` value: "Markt" → *Markt* over *Grote Markt*);
postal code, region, house number, area and `bbox` are `Must` filters.
Scores are `(BM25 + bonuses) × notability × (1 + 0.005 × ln(1 + street
address count))`.
Each tier over-fetches `limit × 20` (≤ 2000) rows, applies focus decay
(`exp(-d/25 km)` for `focus_point`, `exp(-d/1 km)` around a locality
centre), collapses by `building_group_id` (`dedup.rs`: max score, lowest box
as representative; the resolver swaps in the unit with the requested
box), and hydrates the payload. `CandidateSet::ambiguity` flags near-ties (≥ 0.9) on different
streets. `CandidateSet::municipality_groups` groups the candidates by NIS code
in rank order (v3 Phase 11.3): `/geocode?alternatives=true` and the batch
`alternatives` option fetch 50 buildings for it (`resolve_with_alternatives`),
without changing the decision.

## HTTP service (`crabfinder_service::http`)

`router_with(AppState, RouterOptions)`:

- **Handlers** run their Tantivy work on the blocking pool
  (`http::blocking`), so the async runtime never stalls.
- **Guard** middleware: a deadline (`--request-timeout-ms`; batch:
  `--batch-timeout-secs`) covering both waiting for one of
  `--max-concurrency` shared slots and handling; past it, JSON 503. The
  slot and the deadline also go to the handler as request extensions
  (`Permit`, `Deadline`): the batch route keeps the slot and checks the
  deadline while its body streams.
- **Telemetry** (`telemetry.rs`): per-route counters and latency
  histograms, rejected requests, search outcomes by tier / confidence /
  match type (recorded in `resolver::search`), over-fetch-cap hits;
  served at `/metrics`. The `metrics` macros are no-ops in the CLIs.
- `TraceLayer` request spans; CORS (`--cors-origin`, default any).

## Autocomplete (`suggest.rs`, v3 Phase 11.5)

An in-memory name index, built at startup from the stored documents and
not part of the Tantivy index: no schema change, and `/geocode` never
reads it. The v5 edge-ngram field it replaces is still banned (see
below).

- **Entries.** One per BeSt street and postal code (fed by the
  address-count pass, so the store is read once): its FR/NL/DE names,
  the place words of its municipality, postal area and part of
  municipality, its building count, notability and the median-number
  point (`mid-street-v1.bin`). Then one per BeSt locality
  (`localities-v1.json`) and, only with `--places` / `--pois`, the OSM
  places and landmarks, which lead to the nearest BeSt address (R-tree).
- **Keys.** The analyzed name (`query_builder::analyze`) from each of its
  first 4 words, so "loi" finds "rue de la loi", in an `fst::Map` whose
  values index posting lists (entry, whole-name flag, language of the
  name).
- **Lookup.** Earlier words are expanded by the abbreviation map; the
  last one is a prefix unless followed by a space. The exact key is
  always read, then the fst is streamed from the prefix. The 63 prefixes
  shared by more than 2,000 keys ("r", "rue ", "st", …) have precomputed
  best lists instead: the top 64 overall and the top 32 per 0.2° × 0.3°
  cell, of which the 3 × 3 cells around `focus_point` are read. Place
  words or a postal code switch back to streaming, since they filter.
- **Score.** Building count (`0.5 ln(1 + n)`), notability, whole name
  from its first word (+3), whole name typed (+2), each place word
  matched (+1.5), and `focus_point` (+4 `exp(-d / 25 km)`).
- **Numbers.** After a street name a number lists that street's buildings
  (a `building_group_id` range query, one unit per building). A number
  inside a name ("Rue du 11 Novembre") is read as a street word when no
  street name ends before it.
- **Cost on the full index.** 149,054 entries, 166,123 keys, about 0.8 s
  and about 80 MB at startup (`--no-autocomplete` skips both).

## Spatial index (`/reverse`)

At startup `SpatialIndex` walks every doc's `lat`/`lon` FAST columns
(skipping non-point-accurate positions) and bulk-loads an
`rstar::RTree` (~100 MB, ~5–6 s for the full index; the service binds
after). Queries over-search, then re-sort by Haversine distance.

`interpolate=true` (`interpolate.rs`, pure geometry): the addresses
within 200 m are grouped by street side (street key + postal code +
parity); for consecutive numbers with a gap, the point is projected on
the segment between them (equirectangular metres) and the number taken
proportionally, rounded to the parity. The nearest segment wins if the
point is nearer to it than to any address.

## Batch (`crabfinder_service::batch`)

`io` reads CSV (delimiter sniffed; clean columns or free-text
`address`) and writes CSV or GeoJSON, one row at a time (`OutputSink`);
`dedup` resolves each distinct input once and fans out with rayon;
`run::resolve_chunks` (v3 Phase 11.6) drives both: 5,000 rows at a time,
dedup within the chunk, each chunk written and flushed before the next,
with a `keep_going` check in between. Every batch path uses it:

- `http`, the `POST /batch/geocode` façade: a blocking task sends each
  chunk down a bounded channel into the response body (backpressure, and
  a closed channel stops the work). The handler answers once the first
  chunk is out, so a first chunk past the deadline is still a 503; later,
  the deadline cuts the body off with an error.
- `crabfinder-batch`, a peer CLI opening the index directly (mmap
  read-sharing makes it safe next to a live service): `--progress`
  reports after each chunk.
- `jobs`, below.

## Batch jobs (`jobs.rs`, v3 Phase 11.6)

Off unless `--jobs-dir`. `POST /batch/jobs` (`http/jobs.rs`) streams the
upload into `<dir>/.upload-<id>/input.csv` (JSON bodies are written out
as CSV), checks the options and the header, writes `state.json` and
renames the directory to `<dir>/<id>`, then queues the job. Ids are 128
random bits (`getrandom`).

Each job is a Tokio task waiting for one of `--jobs-max-running` slots,
then a blocking task running on the jobs' own rayon pool
(`--jobs-threads`, half the cores), so jobs share CPU with interactive
requests rather than take it. A run:

1. counts the rows (first run only; a malformed row fails the job before
   any output) and notes whether any has a `best_id` (the id columns are
   on for all rows or none);
2. opens `result.part`, cuts it back to the checkpointed length and
   skips the rows already done (a file shorter than its checkpoint means
   lost writes: start over);
3. runs `resolve_chunks` into it; between chunks, flushes and saves
   `rows_done` and the file length in `state.json` (temp file + rename),
   and stops if the job was cancelled;
4. at the end, renames `result.part` to `result.csv` / `result.geojson`.

At startup, `Jobs::open` deletes leftover uploads, queues again every
job left `queued` or `running`, and deletes finished jobs past
`--jobs-ttl-hours`; a sweep repeats every 10 minutes. Cancelling a job
sets a flag the worker reads between chunks and marks the job
`cancelled` at once; its partial result is deleted when the worker
stops.

## What the index does *not* contain

- Per-language searchable fields (removed in v3).
- A prefix / autocomplete field (v5 edge-ngram attempt removed, see
  `reports/archive/phase7a-removed/`). `/autocomplete` uses the
  in-memory `suggest` index instead.
- Anything from OSM in `searchable` or in the payload.
- A persisted R-tree: it is rebuilt at every startup.
