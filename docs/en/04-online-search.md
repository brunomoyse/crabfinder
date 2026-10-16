# 4. Online search (HTTP API)

Use case: you're integrating crabfinder into an application (form, map, back-office tool) where a user types one query at a time. Each HTTP call is short (p50 ~1.5 ms, p95 ~7 ms on the evaluation set) and the service runs continuously.

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
| `GET` | `/lookup` | Addresses by BeSt id |
| `GET` | `/validate` | Check an address: valid, corrected (and how), ambiguous, incomplete |
| `POST` | `/batch/geocode` | See [`03-batch.md`](03-batch.md) |
| `GET` | `/metrics` | Prometheus metrics (unless started with `--no-metrics`) |

## `GET /health` — service health

```bash
curl "http://127.0.0.1:8080/health"
```

```json
{
  "status": "ok",
  "index_path": "./index-belgium",
  "index_docs": 6886637,
  "uptime_secs": 1234,
  "schema_version": 10,
  "index_built_at_unix_secs": 1792106077,
  "version": "0.1.0"
}
```

Use this from a load balancer / monitoring to verify the service responds. `index_docs` should be around 6.9M for a full Belgium index — if much lower, indexing partially failed. `index_built_at_unix_secs` tells you when the index was built (handy to spot a forgotten refresh).

## `GET /geocode` — address search

The main endpoint. One address in — as free text **or** as separate fields — ranked top-N out as **GeoJSON**.

### Parameters

| Param | Type | Default | Effect |
|---|---|---|---|
| `q` | string | — | Free-text query ("Avenue Louise 32 1050", "grand place bruxelles"…). |
| `street`, `housenumber`, `box`, `postcode`, `city` | string | — | Structured query, instead of `q` (see below). |
| `limit` | int | `10` | Max results. Min 1, max 100. |
| `lang` | string | each address's own | `fr`, `nl` or `de`: language of `properties.label`. |
| `expand` | bool | `false` | If `true`, returns one feature per BeSt row without building-level dedup. Useful for listing boxes in a single building. |
| `region` | string | — | Hard filter: `brussels`, `flanders`, `wallonia` (or `BE-BRU`, `BE-VLG`, `BE-WAL`, or variants `bruxelles`, `vlaanderen`, `wallonie`...). |
| `postal_code` | string | — | Hard filter on postal code (4 digits). Wins over any postal code parsed from `q`, and never falls back to a neighbouring postal code. |
| `bbox` | string | — | Hard filter: `min_lon,min_lat,max_lon,max_lat` (WGS84, GeoJSON order). Only addresses inside the rectangle. |
| `focus_point` | string | — | `"lat,lng"` WGS84 inside Belgium. Soft re-rank: candidates close to the point move up. Half-life 25 km. |

Give either `q` or the structured fields, not both.

### Examples

```bash
# Basic search
curl "http://127.0.0.1:8080/geocode?q=Avenue+Louise+32+1050&limit=3"

# Belgian formats: box, B- prefix, comma
curl "http://127.0.0.1:8080/geocode?q=Rue+de+la+Loi+16+bte+3,+B-1000"

# Typo
curl "http://127.0.0.1:8080/geocode?q=Avenue+Loiuse+32+1050"

# Structured, with Dutch labels
curl "http://127.0.0.1:8080/geocode?street=Grote+Markt&housenumber=1&city=Antwerpen&lang=nl"

# A hamlet (needs the optional --places index)
curl "http://127.0.0.1:8080/geocode?q=Maredsous"

# Region-restricted
curl "http://127.0.0.1:8080/geocode?q=Rue+de+la+Loi&region=wallonia&limit=5"

# Geographic disambiguation (Ghent centre)
curl "http://127.0.0.1:8080/geocode?q=Kerkstraat+1&focus_point=51.0543,3.7174"

# Cross-language: same address found via its NL name
curl "http://127.0.0.1:8080/geocode?q=Louizalaan+32+1050"
```

### What the parser understands

`q` is split into street words, house number, box and postal code. Every row below is a real `/geocode` result:

| You type | Parsed as | Result |
|---|---|---|
| `Rue de la Loi 16 bte 3, B-1000` | street `Rue de la Loi`, number `16`, box `3`, PC `1000` | Rue de la Loi 16, 1000 Bruxelles |
| `Louizalaan 32 bus 2 1050` | box markers `bte`, `boîte`, `bt`, `bus`, `box`, `b` | Avenue Louise 32, 1050 Ixelles |
| `Rue Belliard 71/2 1040` | `71/2` = number 71, box 2 | Rue Belliard 71, 1040 Bruxelles |
| `Avenue Louise 32-34 1050` | a range keeps its first number | Avenue Louise 32, 1050 Ixelles |
| `1050-Ixelles Avenue Louise 32` | `1050-Ixelles` = PC + word; `B1050`, `BE-1050` work too | Avenue Louise 32, 1050 Ixelles |
| `Chaussée de Waterloo 1357 1180 Uccle` | two 4-digit numbers: the last is the PC, the other the house number | Chaussée de Waterloo 1357, 1180 Uccle |
| `Rue du 11 Novembre 5 1040` | of several numbers the **last** is the house number; `11` stays in the street name | street words `Rue du 11 Novembre`, number `5` |

When a query can be read more than one way, the parser also tries the other readings, in order, and the response says which one won (`query.reading`, `0` = the first). Two common cases:

- `Rue du 11 Novembre` (no other number): read first with house number 11, then with `11` as part of the street name.
- `Han-sur-Lesse 5580`, `Grand-Place 1000`: a lone 4-digit number is read first as the postal code, then as a house number.

House numbers are case-insensitive (`32a` = `32A`). The `query.parsed` object in the response shows how your query was split — the first thing to check when a result looks wrong.

### Typos

A street word that doesn't exist in the index is matched to the closest existing words: within 1 edit for words of 5–8 letters, 2 edits for 9 letters and more. Words of 4 letters or fewer must be spelled right. A correctly spelled word is never "corrected" into another street.

```
q = Avenue Loiuse 32 1050
→ Avenue Louise 32, 1050 Ixelles   confidence=fuzzy_street  fallback_reason=street_spelling_fuzzy  match_type=typo
```

Accents and case never count as typos (`etterbeek` = `Etterbeek`, `chaussee` = `Chaussée`).

### Structured parameters

When your data is already split, send the fields instead of `q`:

| Param | Effect |
|---|---|
| `street` | Street name, either language. |
| `housenumber` | House number (`12`, `12A`). |
| `box` | Box / unit. Never a filter: it picks which unit of the building is returned. |
| `postcode` | 4-digit postal code (anything else is a 400). Part of the address, like a postal code in `q`: when nothing matches inside it, the street is searched next door (`adjacent_commune`). |
| `city` | Municipality or locality name, matched together with the street words. |

At least one of `street`, `city` or `postcode` is required.

```bash
curl "http://127.0.0.1:8080/geocode?street=Avenue+Louise&housenumber=32&box=b002&postcode=1050"
# → "label": "Avenue Louise 32 bte b002, 1050 Ixelles"
```

### Labels and languages

Every feature carries `properties.label`, the address on one line: `"{street} {number}[ {box marker} {box}], {postal code} {municipality}"`, with `bte` / `bus` / `Bfk.` as box marker in French / Dutch / German.

Without `lang`, each address is labelled in its own language: French in Wallonia, Dutch in Flanders, French in Brussels, German in the German-speaking municipalities (postal codes 4700–4791). With `lang=nl` (or `fr`, `de`), the label uses that language when the address has a name in it, and its own language otherwise:

```
q=Rue de la Loi 16&lang=nl   → "Wetstraat 16, 1000 Brussel"
q=Klosterstraße 1 4700       → "Klosterstraße 1, 4700 Eupen"
q=Grand-Place 1000&lang=de   → "Grand-Place 2, 1000 Bruxelles"   (no German name: own language)
```

`lang` only changes the label. Search always works across languages.

### Place names: villages, hamlets, municipalities

A query that is only a place name — a BeSt municipality, part of municipality or postal area, or, if the index was built with the optional `--places` helper, an OpenStreetMap village or hamlet such as Maredsous — returns the BeSt addresses nearest to that place. A top-level `locality` object echoes how the query was understood (`source: "best"` or `"osm"`); it carries no position, and nothing in the features comes from OpenStreetMap:

```bash
curl "http://127.0.0.1:8080/geocode?q=Maredsous&limit=2"
```

```jsonc
{
  "locality": {
    "kind": "hamlet",            // municipality, part_of_municipality, postal_area, village, hamlet, locality, isolated_dwelling, landmark
    "name": { "fr": "Maredsous", "nl": null, "de": null },
    "postal_codes": ["5537"],
    "source": "osm"              // "best" for municipality, part_of_municipality, postal_area
  },
  "features": [
    { "properties": { "label": "Rue des Abbayes 20, 5537 Anhée", ... },
      "match_type": "spatial", "confidence": "locality", "fallback_reason": "street_missing", ... },
    { "properties": { "label": "Rue des Abbayes 37, 5537 Anhée", ... }, ... }
  ]
}
```

A street plus a place name also works when the place isn't the address's official municipality: `Rue de la Loi 17, 1040 Etterbeek` finds `Rue de la Loi 17, 1040 Bruxelles` (that stretch of 1040 belongs to the City of Brussels) as `exact`.

### Landmarks

With the optional `--pois` helper (OpenStreetMap landmarks: sights, castles, stations, airports, town halls, museums, parks, churches…), a query that only names a landmark returns the BeSt addresses nearest to it (within 600 m), with `confidence: landmark` and a `locality` echo of kind `landmark`. The landmark's name and position are only used to know where to look; they are never returned:

```bash
curl "http://127.0.0.1:8080/geocode?q=Manneken+Pis&limit=1"
```

```jsonc
{
  "locality": {
    "kind": "landmark",
    "name": { "fr": "Manneken-Pis", "nl": "Manneken Pis", "de": "Manneken Pis" },
    "postal_codes": [],
    "source": "osm"
  },
  "features": [
    { "match_type": "spatial", "confidence": "landmark", "fallback_reason": "street_missing", ... }
  ]
}
```

- Any of its names works (`Gare du Midi`, `Zuidstation`), and so does a landmark followed by its locality (`Gravensteen Gent`).
- A street with that name wins: `Atomium` returns Avenue de l'Atomium (`median_street`), right next to it.
- A postal code in the query must lie within 15 km of the landmark (`Manneken Pis 1000` works, `Manneken Pis 9000` doesn't).
- Several landmarks with the same name: the one nearest `focus_point`, otherwise sights first, then stations and institutions, churches last.
- A house number turns the query back into an address search.

### Response shape

```jsonc
{
  "type": "FeatureCollection",
  "query": {
    "q": "Avenue Louise 32 1050",
    "limit": 2,
    "expand": false,
    "parsed": {
      "street_tokens": ["Avenue", "Louise"],
      "house_number": "32",
      "box_number": null,
      "postal_code": "1050"
    },
    "reading": 0                    // which reading of the query won (see above)
  },
  "count": 1,
  "ambiguous": false,               // see "Ambiguous queries"
  // "score_gap": 0.42,             // present when there are 2+ results
  "match_type": "strict",           // "strict" if at least one feature is strict; else "fuzzy"
  "deduplicated": true,             // false when expand=true
  "raw_hits": 11,                   // BeSt rows considered before building-level collapse
  "_note": "...",
  // "locality": { ... },           // only for place-name queries
  "features": [
    {
      "type": "Feature",
      "geometry": {
        "type": "Point",
        "coordinates": [4.356695401146948, 50.83451422165412]   // [lng, lat] per GeoJSON convention
      },
      "properties": {
        "label":         "Avenue Louise 32, 1050 Ixelles",
        "street_name":   { "fr": "Avenue Louise", "nl": "Louizalaan", "de": null },
        "house_number":  "32",
        "box_number":    null,
        "postal_code":   "1050",
        "municipality_name": { "fr": "Ixelles", "nl": "Elsene", "de": null },
        "region":        "brussels",
        "status":        "current",
        "coordinates":   { "crs": "EPSG:4326", "lat": 50.8345, "lon": 4.3567 },
        "id":            { "namespace": "...", "object_identifier": "...", "version_identifier": "..." }
        // ... (all BeSt metadata; "position_source" appears when the position was imputed)
      },
      "score": 186.87859,
      "match_type": "strict",
      "unit_count": 11,               // 11 BeSt rows (boxes) in this building
      "building_group_id": "https://databrussels.be/id/streetname|3919|32",
      "confidence": "exact",          // same vocabulary as the batch column
      "house_number_delta": 0         // matched minus requested number, when a number was honoured
      // "fallback_reason": "...",    // present when confidence isn't exact
    }
  ]
}
```

`confidence`, `fallback_reason` and `house_number_delta` mean exactly what they mean in batch mode — see [`05-understanding-results.md`](05-understanding-results.md). Asking `Avenue Louise 33 1050`, for instance, returns `Avenue Louise 59` with `confidence=nearest_same_side`, `house_number_delta=26`.

### Understanding `match_type`

| Value | Meaning |
|---|---|
| `strict` | Every query word matched (the "happy path"). |
| `typo` | A misspelled street word was corrected (see [Typos](#typos)). |
| `fuzzy` | The strict pass returned zero; the service fell back to "any of the words". Looser match, worth inspecting. |
| `spatial` | Found by position, not by text: `/reverse`, and addresses around a place for a place-name query. |
| `id` | Looked up by BeSt id (`/lookup`). |

The top-level `match_type` is `strict` if **at least one** returned feature is strict, `fuzzy` otherwise.

### Ambiguous queries

`ambiguous` is `true` when the second result scores at least 90% of the first **and** lies on a different street or municipality — typically a street name that exists in many municipalities, without postal code. `score_gap` (`1 − score₂/score₁`, present when there are at least two results) tells you how close the call was: `0` = tie.

```
q = Kerkstraat 1          → ambiguous=true,  score_gap=0.004
                            Kerkstraat 1, 8370 BLANKENBERGE/Uitkerke
                            Kerkstraat 1, 8420 DE HAAN/Klemskerke/Wenduine …
q = Kerkstraat 1 &focus_point=51.0543,3.7174
                          → ambiguous=false, score_gap=0.45
                            Kerkstraat 1, 9050 Gentbrugge/Ledeberg
```

When `ambiguous` is `true`, ask the user to pick, or add a postal code, `region` or `focus_point`.

### Turning off building-level dedup

By default, multiple boxes in the same building are collapsed into one feature with `unit_count > 1`. To get each box separately:

```bash
curl "http://127.0.0.1:8080/geocode?q=Avenue+Louise+32+1050&expand=true"
```

You get one feature per BeSt row (up to `limit`), no collapse.

Cleaner alternative: use `/building/{id}/units` (below) with the `building_group_id` returned by the first query.

## `GET /reverse` — coordinates → addresses

Use case: a user clicks on a map, you want the closest address.

```bash
curl "http://127.0.0.1:8080/reverse?lat=50.8467&lng=4.3525&limit=1&lang=nl"
# → "label": "Grote Markt 29, 1000 Brussel", "distance_m": 37.3
```

### Parameters

| Param | Type | Default | Effect |
|---|---|---|---|
| `lat` | float | required | WGS84 latitude |
| `lng` | float | required | WGS84 longitude |
| `limit` | int | `1` | Max results. Max 100. |
| `radius_m` | float | — | If set, drops results beyond `radius_m` metres. |
| `lang` | string | each address's own | Language of `properties.label` (and of `interpolation.label`). |
| `interpolate` | bool | `false` | `true`: estimate the house number of a point lying between two known addresses (see below). |

### Response

Same envelope as `/geocode`, plus a `distance_m` field (great-circle distance in metres) on each feature, `match_type: "spatial"`. Sorted ascending by distance, then deduplicated by building.

```jsonc
{
  "type": "FeatureCollection",
  "query": { "lat": 50.8467, "lng": 4.3525, "limit": 1 },
  "count": 1,
  "features": [
    {
      "type": "Feature",
      "geometry": { "type": "Point", "coordinates": [4.352720289567735, 50.84700502224868] },
      "properties": {
        "label": "Grote Markt 29, 1000 Brussel",
        "street_name": { "fr": "Grand-Place", "nl": "Grote Markt" },
        "house_number": "29",
        "postal_code": "1000"
        // ...
      },
      "distance_m": 37.28,
      "match_type": "spatial",
      "unit_count": 1,
      "building_group_id": "..."
    }
  ]
}
```

Only addresses with a building-accurate position are candidates: the few thousand addresses that only got a street or village position (see [`05-understanding-results.md`](05-understanding-results.md#where-the-coordinates-come-from-position_source)) never come out of `/reverse`.

### Restricting to a useful radius

For mobile geolocation (~10-50 m accuracy), filtering with `radius_m=50` avoids surfacing an address 200 m away when the user is in the middle of a park:

```bash
curl "http://127.0.0.1:8080/reverse?lat=50.8466&lng=4.3525&radius_m=50&limit=3"
```

### Estimating a missing number: `interpolate=true`

Sometimes the clicked point lies between two known addresses — a vacant plot, a new building BeSt doesn't list yet. With `interpolate=true`, the response gets an extra top-level `interpolation` object estimating the number that point would carry:

```bash
curl "http://127.0.0.1:8080/reverse?lat=50.86730&lng=4.38900&interpolate=true"
```

```jsonc
{
  "type": "FeatureCollection",
  "interpolation": {
    "house_number": "22",
    "between": ["6", "38"],                  // the known numbers on either side
    "street_name": { "fr": "Rue Caporal Claes", "nl": "Korporaal Claesstraat", "de": null },
    "postal_code": "1030",
    "municipality_name": { "fr": "Schaerbeek", "nl": "Schaarbeek", "de": null },
    "coordinates": [4.388972, 50.867267],    // [lng, lat] on the segment between them
    "distance_m": 3.47,                      // query point → segment, metres
    "label": "Rue Caporal Claes 22, 1030 Schaerbeek"
  },
  "features": [ ... ]                        // unchanged: the nearest existing addresses
}
```

- **Opt-in**: without `interpolate=true`, nothing changes. With it, `features` stay exactly the same; only `interpolation` is added.
- It only appears when the point lies in a **gap between two known numbers on the same side of a street** (same street, postal code and parity, e.g. 6 and 38 with nothing in between), at most 400 m apart, the point at most 60 m off the line joining them and within 200 m. The number is proportional to the position along that line, rounded to the side's parity.
- And only when the point is nearer to that gap than to any existing address: otherwise the first feature already is the answer, and `interpolation` is absent.
- `label` follows `lang`, like the features' labels.

**It's an estimate**: the number may not officially exist. Treat it like an approximation (comparable to `nearest_*`), never as a BeSt address — it has no `best_id` or `building_group_id`.

## `GET /building/{id}/units` — all units in a building

Use case: you have a `building_group_id` (from a prior `/geocode` call) and want to list every box in the building. Handy when a user clicks a building marker.

```bash
# The building_group_id contains / and | which need URL-encoding.
BGID="https://databrussels.be/id/streetname|3919|32"
ENCODED=$(printf '%s' "$BGID" | jq -sRr @uri)

curl "http://127.0.0.1:8080/building/${ENCODED}/units"
```

Response: a `FeatureCollection` with `building_group_id`, `count` and one feature per unit (each with its `label`), sorted by box number (the unit without a box first). An unknown id returns `count: 0`.

## `GET /lookup` — addresses by BeSt id

You stored `best_id`s (from a batch run or `properties.id`) and want the addresses back, without searching. Pass up to 50 ids separated by commas; `lang` works as for `/geocode`.

```bash
curl -G "http://127.0.0.1:8080/lookup" \
  --data-urlencode "id=https://databrussels.be/id/address/210225/1,nope/1/1" \
  --data lang=nl
```

Response: a `FeatureCollection` with `count`, the features **in the order asked** (`match_type: "id"`) and a `missing` list of the ids that don't exist (for example retired after a BeSt refresh):

```
count = 1
missing = ["nope/1/1"]
features[0].properties.label = "Wetstraat 16, 1000 Brussel"
```

An empty `id` is a 400 (`id is empty`), more than 50 too (`too many ids: 51 (max 50)`).

## `GET /validate` — check an address

You have an address from a form or a customer file and want to know: does it exist in BeSt, and if not exactly, what is the official one? `/validate` takes the same input as `/geocode` (`q`, or `street` / `housenumber` / `box` / `postcode` / `city`, plus `lang` and `region`) and answers with a `status`, the official BeSt address and the list of what differs.

```bash
curl -G "http://127.0.0.1:8080/validate" \
  --data-urlencode "street=Av Louise" --data housenumber=32 \
  --data postcode=1000 --data city=Ixelles
```

```
status = "corrected"
address.properties.label = "Avenue Louise 32, 1050 Ixelles"
corrections = [
  {field: "street",      kind: "abbreviation_expanded", input: "av",   official: "avenue"},
  {field: "postal_code", kind: "postal_code_changed",   input: "1000", official: "1050"}]
```

| `status` | What to do |
|---|---|
| `valid` | The address exists as written (case and accents don't count). Keep it. |
| `corrected` | It exists once corrected: show the official `label`, or store the BeSt address. |
| `ambiguous` | Two streets fit equally well: ask for the postal code or the municipality. |
| `incomplete` | Only the street (or a place) was found: no house number, or one that isn't on that street. Ask for the number. |
| `not_found` | Nothing matched. |

The corrections say which field changed and how: a misspelt word (`street_spelling`), an abbreviation written out (`abbreviation_expanded`), missing words of the street name (`street_completed`), a word that matches nothing (`word_ignored`), a wrong or misspelt municipality (`municipality_changed`, `municipality_spelling`), another postal code (`postal_code_changed`), the nearest existing number (`house_number_approximated`), a number not on the street (`house_number_not_found`), a box the building doesn't have (`box_not_found`), or another reading of the free text (`reading`). Three are only completions and leave a `valid` address valid: a missing postal code (`postal_code_added`), a missing box on a one-unit building (`box_added`), and a street written in another language than `lang` (`street_language`). Words are compared lowercase and without accents. The full table is in [`docs/api.md`](../api.md#get-validate).

For a whole file, the batch has the same check: the `corrections` option (see [`03-batch.md`](03-batch.md#checking-addresses-corrections)).

## Filters and disambiguation strategies

### When to use `region`

You know your addresses are all Flemish: adding `region=flanders` shrinks the candidate pool and structurally excludes homonymous Walloon streets.

### When to use `postal_code` as a hard filter

When your feed already has the PC clean and you want to forbid the service from looking elsewhere. The hard `postal_code` filter overrides any PC the parser pulled from `q`:

```bash
# Even if q contains "1000", the filter forces 1050
curl "http://127.0.0.1:8080/geocode?q=Grand-Place+1000&postal_code=1050"
```

Without it, a postal code in `q` (or `postcode`) is part of the address and may be corrected: `Avenue Louise 54 1000` returns `Avenue Louise 54, 1050` with `confidence: adjacent_commune`, because 54 is in 1050. With `q=Avenue Louise 54&postal_code=1000`, the service stays inside 1000 and only finds a fuzzy match there.

### When to use `bbox`

Your map shows one area, or your data is known to lie in a rectangle: `bbox=min_lon,min_lat,max_lon,max_lat` keeps only addresses inside it, for every tier.

```bash
curl -G "http://127.0.0.1:8080/geocode" --data-urlencode "q=Rue de la Loi 16" \
  --data-urlencode "bbox=4.36,50.84,4.37,50.85"
# → "Rue de la Loi 16, 1000 Bruxelles"
```

A malformed box (not 4 numbers, min ≥ max, out of range) is a 400: `invalid bbox '1,2,3' (expected min_lon,min_lat,max_lon,max_lat)`.

### When to use `focus_point`

You have the user's location (from the browser, GPS, IP): passing `focus_point=lat,lng` softly re-ranks nearby candidates without excluding distant ones. Half-life 25 km: a candidate 25 km away keeps half its score.

**Caveat**: `focus_point` re-ranks the candidates the text search found; it doesn't search around the point. Without a house number, a common street name ("Rue de la Gare") falls back to street level on the best-scoring street, and focus has little to work with. Adding a house number, `postal_code` or `region` helps more.

## CORS

The service answers with `Access-Control-Allow-Origin: *` by default, so a JS frontend in a browser can hit the API directly. To restrict it, start the service with `--cors-origin https://app.example.org` (repeatable). No auth — always assume the service runs locally or behind a trusted reverse proxy.

## Deadlines and load

Each request has a deadline (`--request-timeout-ms`, default 10 s), time waiting for a free slot included; at most `--max-concurrency` requests (default 256) are handled at once, the others wait. A request past its deadline gets a `503`. In practice a geocode takes a few milliseconds: a `503` means the service is overloaded.

## Metrics

`GET /metrics` serves Prometheus text format:

| Series | Content |
|---|---|
| `crabfinder_http_requests_total{route,method,status}` | Requests per route and status. |
| `crabfinder_http_request_duration_seconds{route}` | Latency histogram per route. |
| `crabfinder_http_rejected_total{route,reason}` | Requests answered `503` by the deadline. |
| `crabfinder_search_total{tier,confidence,match_type}` | One per search (each `/geocode`, each batch row), by outcome. |
| `crabfinder_search_capped_total` | Searches with so many candidates that the tail of the result may miss buildings. |

The `confidence` breakdown is a cheap quality monitor: a jump in `not_found` or `fuzzy_street` usually means an upstream data issue.

## Errors

Every error body is JSON: `{"error": "<message>"}`.

| Code | Cause |
|---|---|
| `400 Bad Request` | Missing or malformed parameter: no `q` nor structured field, `q` combined with structured fields, `postcode` not 4 digits, unknown `region` or `lang`, out-of-range `lat`/`lng`, malformed `focus_point` or `bbox`, empty or too many `/lookup` ids… |
| `404 Not Found` | Unknown path (empty body). |
| `503 Service Unavailable` | Deadline exceeded (`request timed out after 10000 ms`). |

Examples:

```
GET /geocode?q=x&street=y   → 400 {"error":"q cannot be combined with street, housenumber, box, postcode or city"}
GET /geocode?q=x&lang=en    → 400 {"error":"unknown lang 'en' (expected fr, nl or de)"}
GET /reverse?lat=95&lng=4   → 400 {"error":"lat out of range [-90, 90]: 95"}
```

The service only accepts connections once the index is fully loaded, so there is no "starting up" error.

## What's next

- Geocode a CSV → [`03-batch.md`](03-batch.md).
- Understanding scores and fields → [`05-understanding-results.md`](05-understanding-results.md).
- Service won't start → [`06-faq.md`](06-faq.md).
