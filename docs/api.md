# HTTP API

Base URL: `http://127.0.0.1:8080` (default; `--host` / `--port`). The
service binds IPv4 only, and accepts connections once the index and the
R-tree are loaded (~5–6 s on the full index).

**Data source.** Every address, identifier, coordinate and label in a
response comes from BeSt Address. The optional OpenStreetMap helpers
(indexer `--places`, service `--pois`, both off by default) only help
*interpret* a query naming a hamlet or a landmark: the answer is then the
BeSt address nearest to it (`confidence: locality` / `landmark`), and the
`locality` echo says how the query was understood (`source: osm`), with no
OSM position.

There is no authentication, so bind to localhost or front this with your
own gateway in production. CORS allows any origin unless `--cors-origin`
is set (any method, any header).

| Route | Purpose |
|---|---|
| `GET /health` | Readiness probe + index metadata |
| `GET /geocode` | Free text or structured fields → ranked addresses |
| `GET /reverse` | Coordinates → nearest addresses |
| `GET /building/{building_group_id}/units` | Every BeSt row of one building |
| `GET /lookup?id=…` | Addresses by BeSt id, with each id's status (current, outdated, retired…) |
| `GET /validate` | Is the input a real BeSt address, and what differs |
| `POST /batch/geocode` | CSV / JSON in, CSV or GeoJSON out, 1 row per input row |
| `GET /metrics` | Prometheus text exposition (unless `--no-metrics`) |

## Service flags

| Flag | Env | Default | Notes |
|---|---|---|---|
| `--index` | `CRABFINDER_INDEX` | required | Tantivy index directory (schema must match the binary) |
| `--host` | `CRABFINDER_HOST` | `127.0.0.1` | `0.0.0.0` in containers |
| `--port` | `CRABFINDER_PORT` | `8080` | |
| `--abbreviation-dict` | `CRABFINDER_ABBREVIATION_DICT` | n/a | `data/street-type-abbreviations-v1.yaml`; without it, no `av` → `avenue` expansion |
| `--pois` | `CRABFINDER_POIS` | off | Optional OSM helper: `data/osm-pois-be-v1.csv` (landmarks, `scripts/build_osm_pois.py`) to understand queries naming a landmark; answers stay BeSt addresses. Without it, landmark names aren't recognised |
| `--no-notability-boost` | `CRABFINDER_NO_NOTABILITY_BOOST` | off | Plain BM25 (A/B the boost on the same index) |
| `--request-timeout-ms` | `CRABFINDER_REQUEST_TIMEOUT_MS` | `10000` | Deadline per request, queueing included → 503 |
| `--batch-timeout-secs` | `CRABFINDER_BATCH_TIMEOUT_SECS` | `600` | Deadline of `/batch/geocode` until its response starts streaming |
| `--max-concurrency` | `CRABFINDER_MAX_CONCURRENCY` | `256` | Requests served at once (all routes); the rest wait for a slot |
| `--cors-origin` | `CRABFINDER_CORS_ORIGIN` | any | Repeatable or comma-separated; `*` = any |
| `--no-metrics` | `CRABFINDER_NO_METRICS` | off | Don't serve `/metrics` |

Request logs (one span per request, with status and latency) go to
stderr at `info`; set `RUST_LOG` to change the level.

## Errors

Every error from `/geocode`, `/reverse`, `/lookup`, `/validate`, `/building` and `/health` is
JSON:

```json
{"error":"q cannot be combined with street, housenumber, box, postcode or city"}
```

| Status | When |
|---|---|
| `400` | Missing / malformed parameter (`unknown lang 'en' (expected fr, nl or de)`, `lat out of range [-90, 90]: 99`, `postcode must be 4 digits, got '105'`, …) |
| `503` | The request deadline passed (`request timed out after 10000 ms`), waiting for a concurrency slot included |
| `500` | Internal error (Tantivy failure) |

`/batch/geocode` errors are `text/plain` (`csv: csv header has neither a
`street` nor an `address` column`, `unknown format 'xml' (expected csv
or geojson)`): `400` for bad input or options, `413` for a JSON payload
over 10 000 addresses.

## `GET /health`

```json
{
  "status": "ok",
  "index_path": "./index-current",
  "index_docs": 6885231,
  "uptime_secs": 42,
  "schema_version": 12,
  "index_built_at_unix_secs": 1792106077,
  "version": "0.1.0"
}
```

`schema_version` and `index_built_at_unix_secs` come from the index's
`.crab-meta.json`; `version` is the binary's.

## `GET /geocode`

### Parameters

Either free text (`q`) **or** structured fields, not both (400), and
not neither (400).

| Param | Type | Default | Notes |
|---|---|---|---|
| `q` | string | n/a | Free text: `Rue de la Loi 16 bte 3, B-1000 Bruxelles` |
| `street` | string | n/a | Structured: street name |
| `housenumber` | string | n/a | Structured: `12`, `12A` |
| `box` | string | n/a | Structured: box/unit; picks the unit shown, never filters |
| `postcode` | string | n/a | Structured: 4 digits (else 400); part of the address, so relaxed like a parsed one when nothing matches inside it (`adjacent_commune`) |
| `city` | string | n/a | Structured: municipality or locality, matched like street words |
| `limit` | int | 10 | Features returned after dedup, clamped to 1–100 |
| `expand` | bool | false | One feature per BeSt row instead of per building |
| `region` | string | n/a | Hard filter: `BE-BRU`/`BE-VLG`/`BE-WAL`, or `brussels`/`bruxelles`/`brussel`/`bru`, `flanders`/`vlaanderen`/`flandre`/`vlg`, `wallonia`/`wallonie`/`wal` |
| `postal_code` | string | n/a | Hard filter, never relaxed; overrides a postal code parsed from `q` (a 4-digit token in `q` is then read as the house number) |
| `bbox` | string | n/a | Hard filter `min_lon,min_lat,max_lon,max_lat` (else 400 `invalid bbox '…' (expected min_lon,min_lat,max_lon,max_lat)`) |
| `focus_point` | string | n/a | `lat,lng` inside Belgium: soft re-rank, scores × `exp(-d / 25 km)` |
| `lang` | string | n/a | `fr`, `nl` or `de` for `properties.label`; default: the address's own language (Brussels/Wallonia fr, Flanders nl, 4700–4791 de) |
| `alternatives` | bool | false | Add `alternatives`: the best 50 buildings grouped by municipality (v3). Given anyway, from the features fetched, when `ambiguous` |

A structured query needs at least one of `street`, `city`, `postcode`.

### What the parser understands

`B-1050` / `B1050`, `1050-Ixelles`, commas and semicolons, box markers
(`bte`, `boîte`, `bt`, `bus`, `box`, `b`, `12/3`), ranges (`12-14` → 12),
a 4-digit house number next to a postal code (`Chaussée de Waterloo 1357
1180 Uccle`), numbers inside street names (`Rue du 11 Novembre 5`). When
a query is ambiguous the parser keeps alternative readings, tried in
order (`query.parsed.alternatives`; `query.reading` says which one won).

### How results are found

The search tries, in order, and the first step with hits wins:

1. **Locality**: the query only names a place (`Spontin`, `Celles
   Houyet`, `Han-sur-Lesse 5580`): the BeSt addresses around its centre
   (`confidence: locality`). Places are BeSt municipalities, parts of
   municipality and postal areas; with the optional `--places` index,
   OSM hamlets and villages too (`Maredsous`).
2. **Strict tiers**, for each parser reading: exact street + number →
   typo-corrected street + number → nearest existing number on the
   street → street without the number → typo-corrected street without
   the number.
3. **Street + locality**: `<street> <place>` where the place isn't the
   street's municipality (a neighbouring commune, or an OSM hamlet with
   `--places`): the
   street searched inside that place's area.
4. **Landmark**: with `--pois`, a query that only names a landmark
   (`Manneken Pis`, `Gare du Midi`, `Gravensteen Gent`): the BeSt
   addresses nearest to it, within 600 m (`confidence: landmark`). An
   exact landmark name beats a typo-corrected street (`Gravensteen` is not
   Galerie Ravenstein). A street of the
   same name found in step 2 wins (`Atomium` → Avenue de l'Atomium). A
   postal code must lie within 15 km of the landmark; among namesakes,
   the one nearest `focus_point`, else sights before stations and
   institutions, before churches.
5. **Around the postal code**: nothing matched inside the given postal
   code: the strict tiers again without it, within 20 km of the code's
   centre and nearest first (`confidence: adjacent_commune`,
   `fallback_reason: postal_code_mismatch`). `Avenue Louise 54 1000` →
   Avenue Louise 54, **1050**. This also runs when step 2 only found the
   street (or a nearest number on another street object, or one 20 or
   more away): the exact number just across the boundary wins. Not with
   `postal_code=` (a hard filter) or an unknown postal code.
6. **OR fallback**: any query word, with then without the number.

Strict ranking is `(BM25 + bonuses) × notability × (1 + 0.005 × ln(1 +
addresses on the street))`. Bonuses: adjacent word pairs, and a constant
10 when a leading run of the street words is a street's whole name, so
`Markt` finds *Markt* (Bruges) before *Grote Markt*. Results are
over-fetched (`limit × 20`, at most 2000; none with `expand`), then
collapsed by `building_group_id`; a feature's `score` is the best of its
units.

### Response

`Rue de la Loi 300 1040` (trimmed; `properties` holds the full BeSt
address):

```jsonc
{
  "type": "FeatureCollection",
  "query": {
    "q": "Rue de la Loi 300 1040",       // absent for a structured query
    "limit": 1,
    "expand": false,
    "parsed": {
      "street_tokens": ["Rue", "de", "la", "Loi"],
      "house_number": "300",
      "box_number": null,
      "postal_code": "1040"
      // "alternatives": [ … ]           // other readings, when any
    },
    "reading": 0                         // 0 = parsed, n = alternatives[n-1]
  },
  "count": 1,
  "ambiguous": false,
  // "score_gap": 0.004,                 // see below
  "match_type": "strict",
  "deduplicated": true,
  "raw_hits": 44,
  "_note": "…",
  // "locality": { … },                  // see below
  "features": [
    {
      "type": "Feature",
      "geometry": { "type": "Point", "coordinates": [4.38593, 50.84216] },
      "properties": {
        "id": { "namespace": "https://databrussels.be/id/address", "object_identifier": "…", "version_identifier": "…" },
        "street_name": { "fr": "Rue de la Loi", "nl": "Wetstraat", "de": null },
        "house_number": "248",
        "box_number": null,
        "postal_code": "1040",
        "municipality_name": { "fr": "Bruxelles", "nl": "Brussel", "de": null },
        "region": "brussels",
        "status": "current",
        "coordinates": { "crs": "EPSG:4326", "lat": 50.84216, "lon": 4.38593 },
        // … street_id, municipality_id, postal_info_id, postal_name,
        //   part_of_municipality_*, officially_assigned, street_type,
        //   building_group_id; position_source only when set
        "label": "Rue de la Loi 248, 1040 Bruxelles"
      },
      "score": 44.0,
      "match_type": "strict",
      "unit_count": 1,
      "building_group_id": "https://databrussels.be/id/streetname|1679|248",
      "confidence": "nearest_same_side",
      "fallback_reason": "house_number_out_of_range",
      "house_number_delta": -52
    }
  ]
}
```

Top-level fields:

| Field | Meaning |
|---|---|
| `count` | `features.length` |
| `ambiguous` | The two best features lie on different streets (or municipalities) with `score₂/score₁ ≥ 0.9`: the query doesn't say which one. `Rue de la Station 1` → Charleroi vs La Louvière, `ambiguous: true` |
| `score_gap` | `1 − score₂/score₁`; absent with fewer than two features or when scores aren't text relevance (nearest-number, locality answers) |
| `alternatives` | With `alternatives=true`, or when `ambiguous`: up to 10 municipalities, best first, each `{nis, municipality_name, postal_codes, count, feature}`; `feature` is the municipality's best answer (a labelled BeSt Feature). Lets a UI ask "which one?" with one option per municipality. `features` is unchanged |
| `match_type` | `strict` if any feature is strict, else `fuzzy` (empty result: `strict`) |
| `deduplicated` | `false` with `expand=true` |
| `raw_hits` | Raw BeSt rows the winning step considered, before dedup |
| `locality` | Only when the query names a place: how it was understood, `{kind, name, postal_codes, source}`. `kind` is `municipality`, `part_of_municipality`, `postal_area` (`source: best`) or `village`, `hamlet`, `locality`, `isolated_dwelling`, `landmark` (`source: osm`, from the optional helpers; a landmark has no postal codes). No position: the features are the BeSt addresses around the place |

Per-feature fields:

| Field | Meaning |
|---|---|
| `confidence` | `exact`, `nearest_same_side`, `nearest_any_side`, `median_street`, `fuzzy_street`, `adjacent_commune`, `locality`, `landmark` (see table below) |
| `fallback_reason` | Absent when `exact`; else `house_number_missing`, `house_number_out_of_range`, `street_spelling_fuzzy`, `postal_code_mismatch`, `street_missing` |
| `house_number_delta` | Matched − requested number when a number was honoured (`0` = exact); absent otherwise |
| `match_type` | `strict` (every street word matched), `typo` (edit-distance corrected), `fuzzy` (OR fallback), `spatial` (nearest buildings to a locality centre or landmark), `id` (`/lookup`) |
| `unit_count` | BeSt rows collapsed into this feature (1 with `expand`) |
| `building_group_id` | `{street namespace}\|{street id}\|{house number}`; pass it to `/building/…/units` |
| `properties.label` | The address on one line in `lang` (`Wetstraat 16 bus 3, 1000 Brussel`) |
| `properties.position_source` | Only when the position is not BeSt's own: `same_number`, `interpolated`, `street` or `locality` (~140k Walloon addresses BeSt publishes at Lambert `0 0`) |

| `confidence` | The point is… |
|---|---|
| `exact` | the requested address |
| `nearest_same_side` | the closest existing number with the same parity (≤ max(50, n/2) away) |
| `nearest_any_side` | the closest number on the other side (the street has no number of that parity) |
| `median_street` | the street (median house number); number missing or too far |
| `fuzzy_street` | a typo-corrected or OR-fallback street match (with the number when it exists) |
| `adjacent_commune` | found outside the given postal code, near it; `house_number_delta` says whether the number matched (`0`), was approximated, or was ignored (absent) |
| `locality` | the BeSt address nearest to the centre of the place the query names |
| `landmark` | the BeSt address nearest to the landmark the query names (`--pois`) |

More examples: `Avenue Loiuse 32 Ixelles` → Avenue Louise 32,
`match_type: typo`, `confidence: fuzzy_street`, `fallback_reason:
street_spelling_fuzzy`. `Spontin` →

```json
"locality": {"kind": "part_of_municipality", "name": {"fr": "Spontin", "nl": null, "de": null},
             "postal_codes": ["5530"], "source": "best"},
"features": [{"properties": {"label": "Chaussée de Dinant 47, 5530 Yvoir", "…": "…"},
              "match_type": "spatial", "confidence": "locality", "fallback_reason": "street_missing", "…": "…"}]
```

`?street=Grote Markt&housenumber=1&city=Antwerpen` → `Grote Markt 1,
2000 ANTWERPEN`, `confidence: exact`.

`focus_point` only re-ranks the over-fetched pool; when one street name
dominates nationally, use `region` or `postal_code` as a hard filter.

## `GET /lookup`

`GET /lookup?id=<best_id>[,<best_id>…]&lang=nl`: up to 50 BeSt ids
(400 beyond, or when empty), in the canonical
`{namespace}/{objectIdentifier}/{versionIdentifier}` form of the batch
`best_id` column, with or without the version. `features` lists the ids
that are addresses today, in the order asked; the others are under
`missing`.

`results` (v3 Phase 11.4) gives every id's `status`, in the order asked.
The lookup tries the exact id first, then the object under any version.
After that it checks the history of addresses BeSt no longer lists as
current (the indexer's `history-v1.json.gz`):

| `status` | Meaning | Carries |
|---|---|---|
| `current` | The address, with this id, or this object without version | `current_id`, `feature` |
| `outdated_version` | The object exists under another version | `current_id`, `feature` |
| `retired` / `reserved` / `rejected` | BeSt still lists the record, with that status | `record`, and `successor` if any |
| `unknown` | Not in this dump | n/a |

BeSt has no successor link. The successor is derived from BeSt alone, in
this order:

| `successor.rule` | Rule |
|---|---|
| `same_number_box` | A current address at the same street object, house number and box |
| `same_number` | Otherwise, any current address at the same street object and house number |
| `same_position` | Otherwise, the current address within 5 m of the record's last position (`distance_m`) |

```json
{"type": "FeatureCollection", "count": 1,
 "missing": ["https://data.vlaanderen.be/id/adres/200068/2023-11-01T16:15:58+01:00"],
 "features": [{"properties": {"label": "Goorbaan 59, 2230 HERSELT/Ramsel", "…": "…"}, "match_type": "id", "…": "…"}],
 "results": [
   {"id": "https://data.vlaanderen.be/id/adres/200068/2023-11-01T16:15:58+01:00",
    "status": "retired",
    "record": {"status": "retired", "house_number": "53", "postal_code": "9000",
               "end": "2023-11-01T15:15:58Z", "…": "…"},
    "successor": {"rule": "same_position", "distance_m": 4.9,
                  "feature": {"properties": {"label": "Wasstraat 120, 9000 GENT", "…": "…"}, "…": "…"}}},
   {"id": "https://data.vlaanderen.be/id/adres/200001", "status": "current",
    "current_id": "https://data.vlaanderen.be/id/adres/200001/2023-11-01T14:53:09+01:00",
    "feature": {"…": "…"}}]}
```

On the 2026-10-15 dump, 82% of a sample of 1,500 history ids got a successor:
36% `same_number`, 31% `same_number_box`, 15% `same_position` (half of
them at 0 m: a renamed street or a renumbering).

## `GET /validate`

Is the input a real BeSt address, and if not quite, which official
address is it and what differs (v3 Phase 11.2)? Same input as
`/geocode` (`q`, or `street` / `housenumber` / `box` / `postcode` /
`city`), plus `lang` and `region`. The answer is the batch's top-1
decision, compared field by field with the BeSt address it found:

```
GET /validate?street=Av Louise&housenumber=32&postcode=1000&city=Ixelles
```

```json
{"status": "corrected",
 "query": {"parsed": {"street_tokens": ["Av", "Louise", "Ixelles"], "house_number": "32", "box_number": null, "postal_code": "1000"}, "reading": 0},
 "address": {"type": "Feature", "properties": {"label": "Avenue Louise 32, 1050 Ixelles", "…": "…"},
             "confidence": "adjacent_commune", "house_number_delta": 0, "…": "…"},
 "corrections": [
   {"field": "street", "kind": "abbreviation_expanded", "input": "av", "official": "avenue"},
   {"field": "postal_code", "kind": "postal_code_changed", "input": "1000", "official": "1050"}],
 "confidence": "adjacent_commune", "fallback_reason": "postal_code_mismatch"}
```

| `status` | Meaning |
|---|---|
| `valid` | The address exists as written. Case, accents and completions don't count. |
| `corrected` | It exists once the `corrections` are applied. |
| `ambiguous` | Two near-equal answers on different streets (`/geocode`'s `ambiguous`): the input doesn't say which. |
| `incomplete` | Only a street or a place matched: no house number, or one BeSt doesn't have on that street. `address` is that street (at its median) or place, not an address. |
| `not_found` | Nothing matched; `address` is `null`. |

| Correction `kind` | `field` | When |
|---|---|---|
| `street_spelling` | street | A misspelt word, within the typo tier's edit distance (`louisse` → `louise`) |
| `abbreviation_expanded` | street | `av` → `avenue` (needs `--abbreviation-dict`) |
| `street_completed` | street | Words of the official name are missing (`louise` → `Avenue Louise`) |
| `street_language` | street | The street is written in another language than `lang` asks for (completion) |
| `word_ignored` | query | An input word that matches nothing in the address |
| `municipality_spelling` | city | A misspelt locality word |
| `municipality_changed` | city | `city` names none of the address's municipality, postal area or part of municipality |
| `postal_code_changed` | postal_code | The postal code given isn't the address's |
| `postal_code_added` | postal_code | No postal code was given (completion) |
| `house_number_approximated` | house_number | The number isn't on the street; the nearest one is |
| `house_number_not_found` | house_number | The number isn't on the street; only the street matched |
| `box_not_found` | box | The box given isn't one of the building's |
| `box_added` | box | No box was given; the building has one unit, with a box (completion) |
| `reading` | query | The free text was read another way than first parsed |

Words are compared in analyzer form (lowercase, no accents): `input` and
`official` are that form for word corrections, and the official name or
code for the others. Completions don't turn `valid` into `corrected`.
Every official value is BeSt's.

The batch gives the same verdict per row with the `corrections` option.

## `GET /reverse`

| Param | Type | Default | Notes |
|---|---|---|---|
| `lat` | float | required | WGS84, −90…90 |
| `lng` | float | required | WGS84, −180…180 |
| `limit` | int | 1 | Buildings returned, clamped to 1–100 |
| `radius_m` | float | n/a | Drop results farther than this (non-negative) |
| `lang` | string | n/a | As for `/geocode` |
| `interpolate` | bool | `false` | Also estimate the house number at the point when it lies in a gap between two known numbers (see below) |

```jsonc
// /reverse?lat=50.8467&lng=4.3525&lang=nl
{
  "type": "FeatureCollection",
  "query": { "lat": 50.8467, "lng": 4.3525, "limit": 1 },
  "count": 1,
  "deduplicated": true,
  "raw_hits": 20,
  "_note": "…",
  "features": [{
    "type": "Feature",
    "geometry": { "type": "Point", "coordinates": [4.35272, 50.84701] },
    "properties": { "label": "Grote Markt 29, 1000 Brussel", "…": "…" },
    "score": -37.28,                // −distance_m
    "match_type": "spatial",
    "unit_count": 1,
    "building_group_id": "https://databrussels.be/id/streetname|3281|29",
    "distance_m": 37.28
  }]
}
```

Nearest first, collapsed by building. Only addresses with a point-accurate
position (BeSt's own, or imputed from the same number / interpolation)
are candidates; street- and locality-level guesses are skipped.

### Interpolation

With `interpolate=true`, a top-level `interpolation` object estimates
the number the point would carry: a vacant plot, a building BeSt
doesn't list yet:

```jsonc
// /reverse?lat=50.86730&lng=4.38900&interpolate=true
"interpolation": {
  "house_number": "22",
  "between": ["6", "38"],               // the known numbers either side
  "street_name": {"fr": "Rue Caporal Claes", "nl": "Korporaal Claesstraat", "de": null},
  "postal_code": "1030",
  "municipality_name": {"fr": "Schaerbeek", "nl": "Schaarbeek", "de": null},
  "coordinates": [4.388972, 50.867267], // on the segment between them
  "distance_m": 3.47,                   // query point → segment
  "label": "Rue Caporal Claes 22, 1030 Schaerbeek"
}
```

Every street side (street object, postal code, parity) with numbers
within 200 m is tried: two consecutive numbers of the side with a gap
(`23 − 3 > 2`), at most 400 m apart, the point projecting between them
and at most 60 m off the segment; the number is proportional to the
position along it, rounded to the side's parity. The nearest such
segment wins, and only when the point is nearer to it than to any
existing address; otherwise that address (the first feature) is the
answer and `interpolation` is absent. `features` are unchanged.

## `GET /building/{building_group_id}/units`

Every BeSt row of one building. Percent-encode `building_group_id` in
the path (it contains `/` and `|`).

```jsonc
// /building/https%3A%2F%2Fdatabrussels.be%2Fid%2Fstreetname%7C3919%7C32/units
{
  "type": "FeatureCollection",
  "building_group_id": "https://databrussels.be/id/streetname|3919|32",
  "count": 11,
  "_note": "…",
  "features": [ { "type": "Feature", "properties": { "box_number": null, "label": "Avenue Louise 32, 1050 Ixelles", "…": "…" },
                  "match_type": "strict", "unit_count": 1, "…": "…" } ]
}
```

Order: box number ascending (no box first, lexicographic), then address
id. An unknown building gives `count: 0` and no features.

## `POST /batch/geocode`

Strictly one output row per input row, in input order. Identical input
rows are resolved once.

### Input

By `Content-Type`:

- `text/csv` (or anything else): the body is the CSV.
- `multipart/form-data`: a `file` part (the CSV, up to 50 MiB) and an
  optional `options` part (JSON).
- `application/json`: `{"addresses": [ {row}, … ], "options": {…}}`,
  at most 10 000 addresses (413 above).

CSV delimiter `,` or `;` (sniffed from the first line). Columns, matched
by header name:

| Column | Notes |
|---|---|
| `id` | Required; echoed |
| `street`, `house_number`, `postal_code` | Clean columns |
| `box_number` | Optional; picks the unit, never filters |
| `city` | Optional municipality / locality name, matched like street words (v3); ignored on free-text rows |
| `address` | Free text, parsed like `/geocode?q=`; when present `street` / `house_number` / `box_number` are ignored, `postal_code` still overrides |
| `best_id` | Optional BeSt address id, with or without version (v3). Looked up first, as `/lookup` does; the row is that address (`confidence: id`), or the successor of a retired / reserved / rejected one. The address columns are geocoded only when the id leads to no current address. |

The header must contain `street`, `address` or `best_id` (else 400).

### Options

In the JSON body / `options` part, or as query-string parameters (which
win): `?format=geojson&lang=nl`.

| Option | Default | Notes |
|---|---|---|
| `format` | `csv` | `csv` or `geojson` (else 400) |
| `lang` | n/a | `fr`/`nl`/`de` for the `label` column (else 400) |
| `mid_street_fallback` | `true` | `false`: rows whose number can't be matched come back `not_found` instead of a street-level answer |
| `corrections` | `false` | `true`: append the `/validate` columns `status` and `corrections` (v3) |
| `alternatives` | `false` | `true`: append `score_gap` and `alternatives`, up to 5 municipalities of the best answers, best first, `Charleroi (6000, 6001); La Louvière (7100)` (v3) |

### Output

CSV (`text/csv`), columns in this order: `id, street, house_number,
postal_code, matched_street, matched_house_number, matched_postal_code,
lat, lng, confidence, fallback_reason, house_number_delta,
street_match_score, building_group_id, best_id, address, label,
position_source, ambiguous`. The last four were appended in v2; the
first fifteen are unchanged from v1. With `corrections=true`, two more
columns follow: `status` (as `/validate`) and `corrections`, each one as
`kind:field:input->official`, separated by `; `
(`street_spelling:street:louisse->louise; postal_code_added:postal_code:->1050`).
With `alternatives=true`, `score_gap` and `alternatives` follow (after
`corrections` when both are on). When any row has a `best_id`, two last
columns follow: `id_status` (the `/lookup` status of the id, empty on
rows without one) and `successor_best_id` (the row's address when it
replaced a retired / reserved / rejected id). With `corrections`, an id
row is `valid` when the id is current. Otherwise it is `corrected`, with
`id_replaced:best_id:<input>-><today's id>`.

```
$ curl -X POST -H 'Content-Type: text/csv' --data-binary @in.csv '127.0.0.1:8080/batch/geocode?lang=nl'
id,street,house_number,postal_code,matched_street,…,confidence,fallback_reason,house_number_delta,…,address,label,position_source,ambiguous
1,,,,Rue de la Loi,16,1000,50.84616,4.36653,exact,,0,218.12,…|1679|16,…/address/210225/1,"Rue de la Loi 16 bte 3, B-1000 Bruxelles","Wetstraat 16, 1000 Brussel",measured,false
2,,,,Chaussée de Dinant,47,5530,50.32165,5.00100,locality,street_missing,,16.51,…|7750832|47,…/Address/322784/2,Spontin,"Chaussée de Dinant 47, 5530 Yvoir",measured,false
3,,,,,,,,,not_found,no_match,,,,,zzzz qqqq,,,false
```

`confidence` adds `not_found` (with `fallback_reason: no_match`) and
`id` (found by the `best_id` column) to the `/geocode` values. A `locality` or `landmark` row is the BeSt address
nearest to the place (street, number, ids, position and label all
BeSt's); its `confidence` says it was not asked for by street.

With `format=geojson`: one `application/geo+json` FeatureCollection,
one Point feature per row (`geometry: null` when not found), the CSV
columns as `properties`.

An empty input gives an empty body (CSV) or an empty FeatureCollection.

## `GET /metrics`

Prometheus text format (`text/plain; version=0.0.4`):

| Series | Labels |
|---|---|
| `crabfinder_http_requests_total` | `route` (template, e.g. `/building/{building_group_id}/units`), `method`, `status` |
| `crabfinder_http_request_duration_seconds` | `route`; histogram, 1 ms … 10 s buckets |
| `crabfinder_http_rejected_total` | `route`, `reason` (`timeout`) |
| `crabfinder_search_total` | `tier` (`exact`, `typo`, `nearest`, `street`, `typo_street`, `or_with_number`, `or_street`, `locality`, `area`, `none`), `confidence`, `match_type` of the top result; one per `/geocode` call and per batch row |
| `crabfinder_search_capped_total` | (none); searches whose over-fetch hit the 2000-row cap |

```
crabfinder_http_requests_total{route="/geocode",method="GET",status="200"} 3861
crabfinder_search_total{tier="exact",confidence="exact",match_type="strict"} 3264
crabfinder_search_total{tier="typo",confidence="fuzzy_street",match_type="typo"} 474
crabfinder_http_request_duration_seconds_bucket{route="/geocode",le="0.005"} 3375
```
