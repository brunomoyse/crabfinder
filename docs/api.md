# HTTP API

Base URL: `http://127.0.0.1:8080` (default; configurable via `--host` / `--port`).

All responses are JSON. CORS is permissive (any origin, any method, any
header). There is no authentication — bind to localhost or front this
with your own gateway in production.

## `GET /health`

Readiness probe.

**Response**

```json
{
  "status": "ok",
  "index_path": "./index-belgium",
  "index_docs": 6841382,
  "uptime_secs": 42
}
```

## `GET /geocode`

Free-text → ranked address features.

**Query params**

| Param | Type | Default | Notes |
|---|---|---|---|
| `q` | string | required | The user query |
| `limit` | int | 10 | Max features returned (after dedup) |
| `expand` | bool | false | When true, returns one feature per BeSt address row (no dedup) |
| `region` | string | — | Hard filter: `brussels`/`flanders`/`wallonia` or `BE-BRU`/`BE-VLG`/`BE-WAL`. Aliases accepted: `bruxelles`, `brussel`, `vlaanderen`, `flandre`, `wallonie` |
| `postal_code` | string | — | Hard filter: Belgian 4-digit postal. Overrides any postal parsed out of `q` |
| `focus_point` | string | — | `"lat,lng"` (WGS84, inside Belgium). Soft re-rank: closer candidates outrank further ones. Half-life is 25 km |

**Behavior**

The handler tokenises `q` via the same `crab` analyzer the index uses,
expands abbreviations (`av` → `avenue`, `stationsstr` → `stationsstraat`),
and builds a phrase-first AND-of-tokens query. Strict-match results are
ranked by

```
BM25 × notability × (1 + 0.005 × ln(1 + addr_count_on_street))
```

If the strict pipeline returns zero hits, the handler falls back to an
OR-of-tokens query and marks the results `match_type: "fuzzy"`.

When `focus_point=lat,lng` is set, each candidate's score is then
multiplied by `exp(-distance_m / 25000)` before dedup, so closer
matches outrank further ones without filtering distant matches out
entirely. **Caveat:** focus_point only re-ranks candidates already in
the over-fetch pool. If one street dominates BM25 (e.g. a large town
with 200 identically-scored addresses on `Rue de la Gare`), the
candidate pool can be monoculture and focus_point has nothing to lift.
Use `region` as a hard filter in those cases.

Results are over-fetched (`limit × 20`, capped at 2000), then collapsed
by `building_group_id`. `score` on the returned feature is the
maximum score among collapsed members.

**Response shape**

```jsonc
{
  "type": "FeatureCollection",
  "query": {
    "q": "Avenue Louise 32 1050 Ixelles",
    "limit": 10,
    "expand": false,
    "parsed": {
      "street_tokens": ["Avenue", "Louise", "Ixelles"],
      "house_number": "32",
      "postal_code": "1050"
    }
  },
  "count": 10,                        // == features.length
  "match_type": "strict",             // "strict" if any feature is strict; else "fuzzy"
  "deduplicated": true,               // false when ?expand=true
  "raw_hits": 234,                    // raw rows considered before dedup
  "_note": "Phase 3: results collapsed by building_group_id; score = max(member.score). Use ?expand=true for raw rows.",
  "features": [
    {
      "type": "Feature",
      "geometry": { "type": "Point", "coordinates": [4.371, 50.825] },
      "properties": {
        "id":                 { "namespace": "...", "object_identifier": "...", "version_identifier": "..." },
        "street_id":          { ... },
        "municipality_id":    { ... },
        "postal_info_id":     { ... },
        "building_group_id":  "https://databrussels.be/id/streetname|...|...",
        "house_number":       "32",
        "box_number":         null,
        "postal_code":        "1050",
        "street_name":        { "fr": "Avenue Louise", "nl": "Louizalaan", "de": null },
        "municipality_name":  { "fr": "Ixelles",       "nl": "Elsene",     "de": null },
        "postal_name":        { ... },
        "part_of_municipality_name": null,
        "street_type":        "streetname",
        "region":             "brussels",
        "status":             "current",
        "officially_assigned": true,
        "coordinates":        { "crs": "EPSG:4326", "lat": 50.825, "lon": 4.371 }
      },
      "score": 30.04,
      "match_type": "strict",
      "unit_count": 4,                // count of underlying BeSt rows in this building
      "building_group_id": "https://databrussels.be/id/streetname|...|..."
    }
  ]
}
```

## `GET /reverse`

Coordinates → nearest features by great-circle distance.

**Query params**

| Param | Type | Default | Notes |
|---|---|---|---|
| `lat` | float | required | WGS84 latitude |
| `lng` | float | required | WGS84 longitude |
| `limit` | int | 1 | Max features (after dedup) |
| `radius_m` | float | — | If set, results outside `radius_m` are dropped |

Same response envelope as `/geocode` plus a `distance_m` field on each
feature. Results are sorted ascending by `distance_m`, then dedup'd by
`building_group_id`.

## `GET /building/{building_group_id}/units`

Enumerate every BeSt row in a building. `building_group_id` is the
canonical string returned by `/geocode` and `/reverse`, percent-encoded
in the URL path (it contains slashes and pipes).

Response is a flat array of address features (no top-level FeatureCollection
envelope). Order is stable: house number ascending, box number ascending.
Empty array if the building isn't found.

## Match-type semantics

| Value | Meaning |
|---|---|
| `strict` | Every query token matched the doc through the strict AND pipeline (phrase or AND-of-tokens) |
| `fuzzy` | Fallback path — at least one token didn't match strictly; the feature comes from the OR-fallback query |

The top-level `match_type` in the FeatureCollection is `strict` if any
returned feature is strict; `fuzzy` only when every returned feature is
fuzzy.

## Error responses

- `400 Bad Request` — missing or malformed query parameter
- `503 Service Unavailable` — startup not yet complete (spatial index
  build in progress)

There are no 4xx/5xx responses with a JSON body — error responses are
`text/plain`. Add a gateway in front if you need structured errors.
