# Architecture

crabfinder is a Belgium-only geocoding service backed by the official BeSt
address dump. Single binary, single index, multilingual.

## Crates

```
crates/
├── crabfinder-core      Domain types (Address, BestId, MultilingualName, Region, …)
├── crabfinder-indexer   BeSt XML → Tantivy pipeline (two-pass, lib + bin)
├── crabfinder-parser    Free-text → ParsedQuery (street tokens + house # + postal)
├── crabfinder-service   Axum HTTP server (lib-free, bin only)
└── crabfinder-eval      Eval generator + runner + diff (lib + bin)
```

`crabfinder-core` is depended on by every other crate. `crabfinder-indexer`
exposes its schema module to `crabfinder-service` so reads and writes use the
same field handles.

## Data flow

```
BeSt zip bundle (Brussels + Flanders + Wallonia)
        │
        ▼
crabfinder-indexer   two-pass build
   pass 1: dicts (Municipality, PostalInfo, StreetName, PartOfMunicipality)
   pass 2: stream Addresses, join dicts, reproject Lambert72→WGS84, emit doc
        │
        ▼
./index-belgium      Tantivy v0.26 segment tree + .crab-meta.json sidecar
        │                                          (schema version guard)
        ▼
crabfinder-service   on startup: open index, build R-tree from lat/lon,
                     load abbreviation dict, serve HTTP
        │
        ▼
HTTP clients         GET /geocode, /reverse, /building/{id}/units, /health
```

## Tantivy schema (v6)

Fields built in `crabfinder_indexer::index::CrabSchema::build`:

| Field | Type | Index | Store | Used by |
|---|---|---|---|---|
| `best_id` | raw text | yes | yes | response hydrate |
| `searchable` | multi-valued TEXT (crab analyzer) | yes | no | /geocode token match |
| `notability_score` | f64 FAST | yes | no | /geocode tweak_score |
| `lat` | f64 FAST | yes | yes | response + R-tree build |
| `lon` | f64 FAST | yes | yes | response + R-tree build |
| `house_number` | raw text | yes | yes | strict HN filter |
| `box_number` | raw text | yes | yes | response hydrate |
| `postal_code` | raw text | yes | yes | strict postal filter |
| `building_group_id` | raw text | yes | yes | dedup + grouping |
| `status` | raw text | yes | yes | response hydrate |
| `region` | raw text | yes | yes | response hydrate |
| `payload` | text | no | yes | full BestAddress JSON |

The `crab` analyzer is `SimpleTokenizer → LowerCaser → AsciiFoldingFilter`.
Hyphens behave as hard separators; phrase queries reconstitute adjacency.

`fieldnorms` are disabled on `searchable` so long street names aren't
penalised — geocoding cares about *whether* tokens matched, not *how
much* of the document was made of those tokens.

The schema version is encoded in `index-belgium/.crab-meta.json`. The
service refuses to open an index whose `schema_version` doesn't match
`crabfinder_indexer::meta::SCHEMA_VERSION` (currently `6`).

## Query pipeline (/geocode)

1. **Parse** — `crabfinder-parser` splits the input into street tokens, a
   numeric house number (with optional letter suffix), and a 4-digit Belgian
   postal code.
2. **Analyze** — street tokens are pushed through the `crab` analyzer to
   produce the same surface form the index sees.
3. **Abbreviation expansion** — each analyzed token is looked up in the
   abbreviation dictionary (`data/street-type-abbreviations-v1.yaml`),
   which expands `"av"` → `["av", "avenue"]`, `"stationsstr"` →
   `["stationsstr", "stationsstraat"]`, etc.
4. **Build query** — phrase query (preserves token adjacency, ranked
   first) + boolean AND of OR-of-forms per token (catches non-adjacent
   matches). Postal-code and house-number filters are applied as
   additional `Must` clauses when present.
5. **Tweak score** — notability multiplier is read from the FAST column
   and folded into BM25 via `TopDocs::tweak_score`.
6. **Over-fetch + dedup** — `limit * RAW_HIT_MULTIPLIER` raw hits are
   collapsed by `building_group_id` (file `crabfinder-service/src/dedup.rs`).
7. **Hydrate** — for each kept group, the payload field is parsed back
   into an `Address` and rendered as a GeoJSON-style feature.

If the strict AND-of-tokens pipeline returns nothing, the service falls
back to an OR-of-tokens query, marking each returned feature
`match_type: "fuzzy"`.

## Notability boost

`data/notability-whitelist-v1.yaml` is a curated map of
`building_group_id → multiplier` (currently ~48 entries → 431 unique
bgids). At index time `crabfinder-indexer` joins each address against
this map and writes the multiplier into the `notability_score` FAST
column (default `1.0`). At query time `tweak_score` multiplies BM25 by
this value, lifting iconic toponyms (Grand-Place, Atomium, …) above
look-alikes in lesser cities. Tier A metrics are unchanged within
±0.005 vs the non-boost baseline; adversarial `notability` recall@1
benefits substantially.

## Spatial index (/reverse, /building/.../units)

On startup, `crabfinder-service::spatial::SpatialIndex` walks every
live Tantivy doc once, reads `lat` / `lon` from the FAST columns, and
bulk-loads an `rstar::RTree<SpatialPoint>`. ~100 MB resident for 6.84M
docs. Build time ≈ 5–6 s; the service does not accept connections
until the tree is built.

`/reverse` uses planar squared distance for the R-tree's
`nearest_neighbor_iter`, oversearches by 2× to absorb the lat/lon
scale mismatch at Belgian latitude, then resorts the candidates by
exact Haversine distance.

## What the schema does *not* contain

- No per-language searchable fields. v3 collapsed FR/NL/DE into a single
  multi-valued `searchable` field — language splits had no ranking
  semantics and broke cross-language strict matching for queries like
  "Anvers Grote Markt" (FR municipality + NL street).
- No prefix / autocomplete field. A schema-v5 attempt was abandoned
  (see `reports/archive/phase7a-removed/`).
- No persistent reverse-index sidecar. The R-tree is rebuilt on every
  startup from the same Tantivy lat/lon FAST columns.
