# 4. Online search (HTTP API)

Use case: you're integrating crabfinder into an application (form, autocomplete, map) where an end user types one query at a time. Each HTTP call is short (~20 ms) and the service runs continuously.

> Looking for batch mode? See [`03-batch.md`](03-batch.md).

## Starting the service

See [`02-installation.md`](02-installation.md) §6. In short:

```bash
./target/release/crabfinder-service \
    --index ./index-belgium \
    --abbreviation-dict data/street-type-abbreviations-v1.yaml \
    --port 8080
```

The service listens on **IPv4 only** on `127.0.0.1:8080` by default. Always query via `127.0.0.1`, not `localhost` (which resolves to IPv6 first on some systems).

## Available endpoints

| Method | Path | Use |
|---|---|---|
| `GET` | `/health` | Service health check |
| `GET` | `/geocode` | Forward geocoding (address → coordinates) |
| `GET` | `/reverse` | Reverse geocoding (coordinates → address) |
| `GET` | `/building/{id}/units` | List all units in a building |
| `POST` | `/batch/geocode` | See [`03-batch.md`](03-batch.md) |

## `GET /health` — service health

```bash
curl "http://127.0.0.1:8080/health"
```

```json
{
  "status": "ok",
  "index_path": "./index-belgium",
  "index_docs": 6841382,
  "uptime_secs": 1234
}
```

Use this from a load balancer / monitoring to verify the service responds. `index_docs` should be around 6.8M for a full Belgium index — if much lower, indexing partially failed.

## `GET /geocode` — address search

The main endpoint. Free text in, ranked top-N out as **GeoJSON**.

### Parameters

| Param | Type | Default | Effect |
|---|---|---|---|
| `q` | string | **required** | The user query ("Avenue Louise 32 1050", "grand place bruxelles"…) |
| `limit` | int | `10` | Max results. Min 1, max 100. |
| `expand` | bool | `false` | If `true`, returns one feature per BeSt row without building-level dedup. Useful for listing boxes in a single building. |
| `region` | string | — | Hard filter: `brussels`, `flanders`, `wallonia` (or `BE-BRU`, `BE-VLG`, `BE-WAL`, or variants `bruxelles`, `vlaanderen`, `wallonie`...). |
| `postal_code` | string | — | Hard filter on postal code (4 digits). Wins over any postal code parsed from `q`. |
| `focus_point` | string | — | `"lat,lng"` WGS84. Soft re-rank: candidates close to the point move up. Half-life 25 km. |

### Examples

```bash
# Basic search
curl "http://127.0.0.1:8080/geocode?q=Avenue+Louise+32+1050&limit=3"

# Region-restricted
curl "http://127.0.0.1:8080/geocode?q=Rue+de+la+Gare&region=flanders&limit=5"

# Geographic disambiguation (Liège centre)
curl "http://127.0.0.1:8080/geocode?q=Rue+de+la+Gare&focus_point=50.6326,5.5797"

# Cross-language: same address found via its NL name
curl "http://127.0.0.1:8080/geocode?q=Louizalaan+32+1050"
```

### Response shape

```jsonc
{
  "type": "FeatureCollection",
  "query": {
    "q": "Avenue Louise 32 1050",
    "limit": 3,
    "expand": false,
    "parsed": {
      "street_tokens": ["Avenue", "Louise"],
      "house_number": "32",
      "postal_code": "1050"
    }
  },
  "count": 3,
  "match_type": "strict",          // "strict" if at least one feature is strict; else "fuzzy"
  "deduplicated": true,            // false when expand=true
  "raw_hits": 24,                  // BeSt rows considered before building-level collapse
  "_note": "...",
  "features": [
    {
      "type": "Feature",
      "geometry": {
        "type": "Point",
        "coordinates": [4.371, 50.825]    // [lng, lat] per GeoJSON convention
      },
      "properties": {
        "street_name":   { "fr": "Avenue Louise", "nl": "Louizalaan", "de": null },
        "house_number":  "32",
        "box_number":    null,
        "postal_code":   "1050",
        "municipality_name": { "fr": "Ixelles", "nl": "Elsene", "de": null },
        "region":        "brussels",
        "status":        "current",
        "coordinates":   { "crs": "EPSG:4326", "lat": 50.825, "lon": 4.371 },
        "id":            { "namespace": "...", "object_identifier": "...", "version_identifier": "..." }
        // ... (all BeSt metadata)
      },
      "score": 30.04,                    // BM25 × notability × ln(addr_count)
      "match_type": "strict",            // "strict" or "fuzzy"
      "unit_count": 4,                   // 4 boxes in this building
      "building_group_id": "..."
    }
    // ... 2 more features
  ]
}
```

### Understanding `match_type`

| Value | Meaning |
|---|---|
| `strict` | Every query token matched via the main AND pipeline (the "happy path"). |
| `fuzzy` | The strict pass returned zero, the service fell back to OR-of-tokens. Looser match, worth inspecting. |

The top-level `match_type` is `strict` if **at least one** returned feature is strict. If every feature is fuzzy, it's `fuzzy`.

### Turning off building-level dedup

By default, multiple boxes in the same building are collapsed into one feature with `unit_count > 1`. To get each box separately (e.g. you want all 12 boxes of 32 Avenue Louise on a map):

```bash
curl "http://127.0.0.1:8080/geocode?q=Avenue+Louise+32&expand=true"
```

You get one feature per BeSt row (up to `limit`), no collapse.

Cleaner alternative: use `/building/{id}/units` (below) with the `building_group_id` returned by the first query.

## `GET /reverse` — coordinates → addresses

Use case: a user clicks on a map, you want the closest address.

```bash
curl "http://127.0.0.1:8080/reverse?lat=50.8466&lng=4.3525&limit=1"
```

### Parameters

| Param | Type | Default | Effect |
|---|---|---|---|
| `lat` | float | required | WGS84 latitude |
| `lng` | float | required | WGS84 longitude |
| `limit` | int | `1` | Max results. Max 100. |
| `radius_m` | float | — | If set, drops results beyond `radius_m` metres. |

### Response

Same envelope as `/geocode`, plus a `distance_m` field (great-circle distance in metres) on each feature. Sorted ascending by distance, then deduplicated by building.

```jsonc
{
  "type": "FeatureCollection",
  "query": { "lat": 50.8466, "lng": 4.3525, "limit": 1 },
  "count": 1,
  "features": [
    {
      "type": "Feature",
      "geometry": { "type": "Point", "coordinates": [4.3525, 50.8466] },
      "properties": {
        "street_name":   { "fr": "Grand-Place", "nl": "Grote Markt" },
        "house_number":  "1",
        "postal_code":   "1000",
        "municipality_name": { "fr": "Bruxelles", "nl": "Brussel" }
        // ...
      },
      "distance_m": 4.23,             // 4.23 m from the queried point
      "unit_count": 1,
      "building_group_id": "..."
    }
  ]
}
```

### Restricting to a useful radius

For mobile geolocation (~10-50 m accuracy), filtering with `radius_m=50` avoids surfacing an address 200 m away when the user is in the middle of a park:

```bash
curl "http://127.0.0.1:8080/reverse?lat=50.8466&lng=4.3525&radius_m=50&limit=3"
```

## `GET /building/{id}/units` — all units in a building

Use case: you have a `building_group_id` (from a prior `/geocode` call) and want to list every box in the building. Handy when a user clicks a building marker.

```bash
# The building_group_id contains / and | which need URL-encoding.
BGID="https://databrussels.be/id/streetname/3919/1|32"
ENCODED=$(printf '%s' "$BGID" | jq -sRr @uri)

curl "http://127.0.0.1:8080/building/${ENCODED}/units"
```

Response: a flat `FeatureCollection` (no top-N envelope), one feature per box, sorted by `house_number` then `box_number` ascending.

## Filters and disambiguation strategies

### When to use `region`

You know your addresses are all Flemish: adding `region=flanders` shrinks the candidate pool and structurally excludes homonymous Walloon streets.

### When to use `postal_code` as a hard filter

When your feed already has the PC clean and you want to forbid the service from looking elsewhere. The hard `postal_code` filter overrides any PC the parser pulled from `q`:

```bash
# Even if q contains "1000", the filter forces 1050
curl "http://127.0.0.1:8080/geocode?q=Grand-Place+1000&postal_code=1050"
```

### When to use `focus_point`

You have the user's location (from the browser, GPS, IP): passing `focus_point=lat,lng` softly re-ranks nearby candidates without excluding distant ones. Half-life 25 km.

**Caveat**: `focus_point` only helps if the candidate pool already contains close addresses. If you search "Rue de la Gare" and a single big city dominates BM25 with 200 identical results, focus_point has no one to lift in the pool. In that case, adding `region` or `postal_code` helps more.

## CORS

The service responds with `Access-Control-Allow-Origin: *` by default, so a JS frontend in a browser can hit the API directly. No auth — always assume the service runs locally or behind a trusted reverse proxy.

## Errors

| Code | Cause |
|---|---|
| `400 Bad Request` | Missing or malformed parameter (unknown `region`, out-of-range `lat`/`lng`, malformed `focus_point`…) |
| `503 Service Unavailable` | Startup not complete (spatial R-tree builds ~3 s at startup) |

Error bodies are `text/plain` (not JSON). For structured errors, front the service with a gateway.

## What's next

- Geocode a CSV → [`03-batch.md`](03-batch.md).
- Understanding scores and fields → [`05-understanding-results.md`](05-understanding-results.md).
- Service won't start → [`06-faq.md`](06-faq.md).
