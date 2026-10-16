# 3. Batch geocoding

Use case: you have a list of addresses (CSV, Excel export, SQL query) and you want an enriched file out with lat/lng plus a per-row quality flag.

Two ways to do this, **with the exact same resolution logic underneath** (the same resolver as `/geocode`):

- **`crabfinder-batch` CLI** — no server, no port, you run a binary, feed it a CSV, get a CSV (or GeoJSON). Preferred for data pipelines, cron scripts, one-off exports.
- **`POST /batch/geocode` HTTP endpoint** — the service is already running, you push a CSV per HTTP request, receive a streamed CSV (or GeoJSON). Preferred for application integration (a backend that geocodes on demand).

If unsure: the CLI is simpler to start with. Jump to [CLI](#cli--crabfinder-batch).

---

## Input format

CSV with a header row. Columns are matched **by name**, in any order; unknown columns are ignored. Two shapes are accepted — pick whichever your data already has.

### Split columns

```csv
id,street,house_number,postal_code,box_number
1,Avenue Louise,32,1050,
2,Rue Belliard,71,1040,
3,Grand-Place,,1000,
4,Avenue Louise,33,1050,
5,Some Nonexistent Street,99,1000,
```

### Free text: one `address` column

```csv
id;address;postal_code
1;Av. Louise 32, B-1050 Ixelles;
2;Rue de la Loi 16 bte 3, 1000 Bruxelles;
3;Grote Markt 1 Antwerpen;
4;Spontin;
5;Chaussée de Waterloo 1357 1180 Uccle;
6;Rue Neuve 1;1000
```

Each `address` is parsed exactly like `/geocode?q=` (postal code, house number, box, Belgian formats — see [`04-online-search.md`](04-online-search.md#what-the-parser-understands)). A row with a non-empty `address` ignores its `street` / `house_number` / `box_number` columns. A `postal_code` column still wins over the postal code parsed from the text (row 6 above). Both shapes can be mixed in one file: rows with an empty `address` use the split columns.

### Columns

| Column | Required | Notes |
|---|---|---|
| `id` | yes | Free-form identifier. Copied verbatim into the output so you can join back to your source data. |
| `street` | `street` **or** `address` | Street name. Accents, hyphens, several words, either language: no normalization required. |
| `address` | `street` **or** `address` | The whole address as free text. |
| `house_number` | no | Building number. Alphabetic suffixes (`32A`, `12b`) are fine. |
| `postal_code` | no | Belgian 4-digit postal code. Strongly recommended: without it, homonymous streets ("Rue de la Gare", "Kerkstraat") can't be told apart. |
| `box_number` | no | Box / unit. Never filters: when the building has a unit with that box (spelled as in BeSt, e.g. `b002`), that unit becomes the row's `best_id` and `label` (`Avenue Louise 32 bte b002, 1050 Ixelles`); an unknown box is simply ignored. |

The header must contain `street` or `address`; otherwise the CLI stops with ``csv header has neither a `street` nor an `address` column`` (HTTP: `400`).

**Encoding**: UTF-8. If your CSV is `windows-1252` (typical French/Dutch Excel export), convert first:

```bash
iconv -f WINDOWS-1252 -t UTF-8 addresses-export.csv > addresses.csv
```

**Delimiter**: comma (`,`) or semicolon (`;`). Auto-detected on the first line; force with `--delimiter ';'` on the CLI if in doubt.

**Empty rows**: ignored. **Malformed rows**: the CLI aborts with an error pointing at the line number. The HTTP endpoint returns a 400 with the same message.

## Output format

### CSV

19 columns, always in this order. The first 15 are the v1 columns; the last 4 were added in v2 at the end, so scripts reading columns by position keep working.

```csv
id,street,house_number,postal_code,matched_street,matched_house_number,matched_postal_code,lat,lng,confidence,fallback_reason,house_number_delta,street_match_score,building_group_id,best_id,address,label,position_source,ambiguous
```

| Column | Meaning |
|---|---|
| `id`, `street`, `house_number`, `postal_code` | **Copied from input**, audit trail (empty `street` on free-text rows). |
| `matched_street`, `matched_house_number`, `matched_postal_code` | What crabfinder actually found in BeSt. Shows divergences (typo corrected, nearest number…). |
| `lat`, `lng` | WGS84 (EPSG:4326). Empty on `not_found`. |
| `confidence` | `exact`, `nearest_same_side`, `nearest_any_side`, `median_street`, `fuzzy_street`, `adjacent_commune`, `locality`, `landmark` or `not_found`. Detail in [`05-understanding-results.md`](05-understanding-results.md). |
| `fallback_reason` | Empty if `confidence == exact`. Otherwise `house_number_missing`, `house_number_out_of_range`, `street_spelling_fuzzy`, `postal_code_mismatch`, `street_missing` or `no_match`. |
| `house_number_delta` | Matched number minus requested number: `0` when the requested number was found, e.g. `26` when 33 was asked and 59 returned. Empty for street-level and locality results. |
| `street_match_score` | Raw BM25 score. Text-match quality indicator. Higher = better match. Empty on `not_found`. |
| `building_group_id` | crabfinder's building identifier (`{street id}|{house number}`). Stable across dumps as long as BeSt's street id doesn't change. |
| `best_id` | Official BeSt identifier of the chosen address (`namespace/object_identifier/version`). |
| `address` | The free-text input, echoed (empty on split-column rows). |
| `label` | The match on one line, e.g. `Avenue Louise 32, 1050 Ixelles`, in the address's own language or the one asked with `--lang` / `lang`. |
| `position_source` | Where the coordinates come from: `measured` (published by BeSt) or an imputation (`same_number`, `interpolated`, `street`, `locality`). See [`05-understanding-results.md`](05-understanding-results.md#where-the-coordinates-come-from-position_source). |
| `ambiguous` | `true` when another, almost as good answer lies on a different street or municipality. Worth a look. |

Real output for the split-column example above (`street_match_score`, ids shortened):

```csv
id,street,house_number,postal_code,matched_street,matched_house_number,matched_postal_code,lat,lng,confidence,fallback_reason,house_number_delta,...,label,position_source,ambiguous
1,Avenue Louise,32,1050,Avenue Louise,32,1050,50.83451422165412,4.356695401146948,exact,,0,...,"Avenue Louise 32, 1050 Ixelles",measured,false
2,Rue Belliard,71,1040,Rue Belliard,71,1040,50.84121570632554,4.374046595229909,exact,,0,...,"Rue Belliard 71, 1040 Bruxelles",measured,false
3,Grand-Place,,1000,Grand-Place,2,1000,50.846228509839484,4.353010703511631,median_street,house_number_missing,,...,"Grand-Place 2, 1000 Bruxelles",measured,false
4,Avenue Louise,33,1050,Avenue Louise,59,1050,50.83310464460224,4.357281635827657,nearest_same_side,house_number_out_of_range,26,...,"Avenue Louise 59, 1050 Bruxelles",measured,false
5,Some Nonexistent Street,99,1000,,,,,,not_found,no_match,,...,,,false
```

Row 4: 33 doesn't exist in 1050, so crabfinder returned the nearest odd number on that street (`nearest_same_side`, `house_number_delta=26`).

**Locality rows**: a row that only names a place (`Spontin`, `Han-sur-Lesse 5580`; a hamlet such as `Maredsous` needs the optional `--places` index) comes back as the **nearest BeSt address** to that place, at its own BeSt position: street, number, `best_id`, `building_group_id` and label are all BeSt's (`Spontin` → e.g. `Chaussée de Dinant 47, 5530 Yvoir`). `confidence=locality` and `fallback_reason=street_missing` tell you that address wasn't asked for by street.

**Landmark rows** (with the optional `--pois`): a row that only names a landmark (`Manneken Pis`, `Gare du Midi`, `Gravensteen Gent`) comes back the same way — the nearest BeSt address to it, e.g. `Rue du Chêne 2, 1000 Bruxelles` for `Manneken Pis` — with `confidence=landmark`. The landmark's own name and position are never returned.

**1-in-1-out guarantee**: same row count, same order in and out. A non-geocoded row exits with `confidence=not_found` and empty `lat,lng,matched_*` — **not** dropped from the file.

**Internal deduplication**: if your CSV contains the same address twice, crabfinder resolves it once and copies the result. Transparent optimization, no effect on the output.

### GeoJSON

With `--format geojson` (CLI) or `format=geojson` (HTTP), the output is one GeoJSON `FeatureCollection`: one `Point` feature per input row, in input order (`geometry: null` for `not_found` rows), whose `properties` are the 19 CSV columns.

```json
{
  "type": "FeatureCollection",
  "features": [
    {
      "type": "Feature",
      "geometry": { "type": "Point", "coordinates": [4.356695401146948, 50.83451422165412] },
      "properties": {
        "id": "1",
        "matched_street": "Avenue Louise",
        "matched_house_number": "32",
        "confidence": "exact",
        "house_number_delta": 0,
        "label": "Avenue Louise 32, 1050 Ixelles",
        "position_source": "measured",
        "ambiguous": false
      }
    }
  ]
}
```

(Properties shortened.) Coordinates are `[lng, lat]`, per the GeoJSON convention. The file opens directly in QGIS.

---

## CLI — `crabfinder-batch`

### Minimal invocation

```bash
./target/release/crabfinder-batch \
    --index ./index-belgium \
    --abbreviation-dict data/street-type-abbreviations-v1.yaml \
    --input addresses.csv \
    --output addresses-geocoded.csv
```

The binary opens the Tantivy index read-only, iterates the CSV, writes the result. No HTTP port. No service to start. The `crabfinder-service` can run in parallel against the same index — Tantivy supports multi-process reads.

### All flags

| Flag | Default | Description |
|---|---|---|
| `--index PATH` | — (required) | Tantivy index directory. Env var: `CRABFINDER_INDEX`. |
| `--abbreviation-dict PATH` | (none) | Abbreviations YAML. Without it, queries like "Av. Louise" don't expand to "Avenue Louise". **Recommended**: `data/street-type-abbreviations-v1.yaml`. |
| `--pois PATH` | (none) | Optional OpenStreetMap helper (`data/osm-pois-be-v1.csv`): understand rows naming a landmark; they get the nearest BeSt address, `confidence=landmark`. Env var: `CRABFINDER_POIS`. |
| `-i, --input PATH` | `stdin` | Input CSV. `-` = stdin. |
| `-o, --output PATH` | `stdout` | Output file. `-` = stdout. |
| `--delimiter ,` or `;` | auto | Force the input delimiter (otherwise auto-sniffed). |
| `--format csv` or `geojson` | `csv` | Output format. |
| `--lang fr`, `nl` or `de` | each address's own | Language of the `label` column. An address without a name in that language falls back to its own. |
| `--no-mid-street` | off | Disable the street-level fallbacks (see below). |
| `--parallel N` | all cores | Worker threads. `--parallel 1` for deterministic profiling. |

### Concrete examples

#### Geocoding a French Excel export (semicolon, latin-1)

```bash
iconv -f WINDOWS-1252 -t UTF-8 excel-export.csv \
  | ./target/release/crabfinder-batch \
        --index ./index-belgium \
        --abbreviation-dict data/street-type-abbreviations-v1.yaml \
        --delimiter ';' \
        --output geocoded.csv
```

#### Free-text addresses, Dutch labels

```bash
./target/release/crabfinder-batch \
    --index ./index-belgium \
    --abbreviation-dict data/street-type-abbreviations-v1.yaml \
    --input free-text.csv --lang nl
```

On the free-text example above, row 1 (`Av. Louise 32, B-1050 Ixelles`) comes out `exact` with the label `Louizalaan 32, 1050 Elsene`, and row 2 (`Rue de la Loi 16 bte 3, 1000 Bruxelles`) with `Wetstraat 16, 1000 Brussel`.

#### Straight to QGIS

```bash
./target/release/crabfinder-batch --index ./index-belgium \
    --abbreviation-dict data/street-type-abbreviations-v1.yaml \
    --input addresses.csv --format geojson --output addresses.geojson
```

#### Streaming, no intermediate file

```bash
psql -c "COPY (SELECT id, street, hn AS house_number, cp AS postal_code FROM clients) TO STDOUT CSV HEADER" \
  | ./target/release/crabfinder-batch \
        --index ./index-belgium \
        --abbreviation-dict data/street-type-abbreviations-v1.yaml \
  > clients-geocoded.csv
```

Or, if your table has a single address column: `SELECT id, full_address AS address FROM clients`.

#### Estimating throughput

```bash
time ./target/release/crabfinder-batch \
    --index ./index-belgium \
    --abbreviation-dict data/street-type-abbreviations-v1.yaml \
    --input addresses-100k.csv \
    --output /tmp/out.csv
```

Ballpark: ~1500 rows/s per core for in-process reads on a dev laptop. For 100k rows on 8 cores: ~10 s.

### Exit codes

- `0`: all good.
- non-`0`: error (invalid CSV, missing `street`/`address` column, missing index, wrong sidecar version…). Error message goes to `stderr`; `stdout` stays clean (useful in pipes).

---

## HTTP endpoint — `POST /batch/geocode`

Prerequisite: `crabfinder-service` is running (see [`02-installation.md`](02-installation.md) §6).

### Three body variants

The handler dispatches on `Content-Type`. All three accept both input shapes (split columns or `address`).

#### Variant 1 — multipart/form-data (recommended for large files)

```bash
curl -X POST http://127.0.0.1:8080/batch/geocode \
     -F file=@addresses.csv \
     -F 'options={"mid_street_fallback":true,"format":"csv"}' \
     -o addresses-geocoded.csv
```

Capacity: up to **50 MB body** per request. At ~500 bytes/row, that's ~100k rows.

#### Variant 2 — application/json (recommended for small batches and app integration)

```bash
curl -X POST http://127.0.0.1:8080/batch/geocode \
     -H "Content-Type: application/json" \
     -d '{
       "addresses": [
         {"id": "1", "street": "Avenue Louise", "house_number": "32", "postal_code": "1050"},
         {"id": "2", "address": "Grand-Place 1, 1000 Bruxelles"}
       ],
       "options": {"mid_street_fallback": true, "lang": "nl"}
     }' \
     -o batch.csv
```

Capacity: up to **10,000 addresses** per request (HTTP 413 beyond — switch to multipart).

#### Variant 3 — raw CSV body (any Content-Type, simplest to wire)

```bash
curl -X POST "http://127.0.0.1:8080/batch/geocode?format=geojson&lang=nl" \
     -H "Content-Type: text/csv" \
     --data-binary @addresses.csv \
     -o addresses.geojson
```

This is the leanest path, useful for testing with a minimal client.

### Options

Options go in the JSON body / the `options` multipart part, **or in the query string** (`?format=geojson&lang=nl&mid_street_fallback=false`) — handy for the raw CSV variant. The query string wins when both are given.

| Option | Type | Default | Effect |
|---|---|---|---|
| `mid_street_fallback` | bool | `true` | When `false`, disables the street-level fallbacks (same as `--no-mid-street` on the CLI). |
| `format` | string | `"csv"` | `"csv"` or `"geojson"`. Anything else is a 400. |
| `lang` | string | each address's own | `"fr"`, `"nl"` or `"de"`: language of the `label` column. Anything else is a 400. |

### Response

`text/csv; charset=utf-8` (or `application/geo+json`), **streamed**. The first bytes ship as soon as the first rows are resolved — this matters on large files.

### HTTP codes

- `200`: CSV / GeoJSON response (an input with only a header gives an empty CSV body, or an empty FeatureCollection).
- `400`: malformed CSV or JSON, no `street`/`address` column, unknown `format` or `lang`. Plain-text message, e.g. `unknown format 'xml' (expected csv or geojson)`.
- `413`: JSON payload > 10,000 addresses.
- `503`: the request didn't start answering within `--batch-timeout-secs` (default 600 s). JSON body `{"error": "..."}`.
- `500`: internal error (please report; shouldn't happen).

---

## Turning off the street-level fallbacks

By default, a row whose house number can't be matched still gets coordinates on its street (`median_street`, or a street-level `fuzzy_street`). If you'd rather have empty coordinates than a street approximation, use `--no-mid-street` (CLI) or `mid_street_fallback=false` (HTTP): those rows then come out `not_found`.

```bash
./target/release/crabfinder-batch --index ./index-belgium -i addresses.csv --no-mid-street
```

On the split-column example, row 3 (`Grand-Place`, no number) becomes `not_found, no_match`. Row 4 (`Avenue Louise 33`) is still `nearest_same_side`: the nearest-number result is a real address, not a street approximation, so it is kept.

## Edge cases

### Addresses without postal code

Works, but quality strongly degraded. crabfinder can't filter by PC, so homonymous streets ("Kerkstraat" exists in hundreds of municipalities) come back in undefined order — such rows are usually flagged `ambiguous=true`. **Always provide the PC when you have it**.

### Wrong postal code

A postal code that doesn't hold the address is not fatal: when nothing matches inside it (or the street is there but the number isn't, and the exact number exists just across the boundary), crabfinder searches the street within 20 km of that postal code's centre and reports `adjacent_commune` / `postal_code_mismatch`. `matched_postal_code` then shows the real one:

```
id,street,house_number,postal_code,matched_street,matched_house_number,matched_postal_code,lat,lng,confidence,fallback_reason,house_number_delta
1,Avenue Louise,54,1000,Avenue Louise,54,1050,50.83340378514029,4.358146544863564,adjacent_commune,postal_code_mismatch,0
```

See [`05-understanding-results.md`](05-understanding-results.md#adjacent_commune--found-next-door).

### Addresses with only a place name (no street)

Supported, in either shape: `address=Spontin`, or `street=Maredsous` with an empty `house_number`. When the whole text is a known place name — a BeSt municipality, part of municipality or postal area, or, with the optional `--places` index, an OpenStreetMap village or hamlet (`Maredsous`) — you get a `locality` row: the nearest BeSt address to the place. With the optional `--pois`, a landmark name (`Manneken Pis`, `Gare du Midi`, optionally followed by its locality: `Gravensteen Gent`) gives a `landmark` row, again the nearest BeSt address — unless a street has that name (`Atomium` → Avenue de l'Atomium, `median_street`).

### Box numbers

Use the `box_number` column, or write the box with the number — in `house_number` (`32 bte b002`, `32/b002`) or in the free text (`bte 3`, `bus 3`, `box 3`, `12/3`). The box never prevents a match: the building is found either way, and the box only decides which of its units is reported in `best_id` / `label`. Write the box as BeSt spells it (Brussels often uses forms like `b002`); a box that matches no unit is ignored.

### Duplicates in input

Preserved in output (1-in-1-out). Internally, crabfinder only resolves unique addresses and propagates the result to every occurrence.

### Recommended batch size

- CLI: no limit. 1M rows passes through (memory peaks at a few hundred MB).
- HTTP multipart: 50 MB cap (~100k rows).
- HTTP JSON: 10,000 addresses cap.

To go further over HTTP, chunk it client-side. Or switch to the CLI.

## What's next

- Understanding the `confidence` / `fallback_reason` / `position_source` columns → [`05-understanding-results.md`](05-understanding-results.md).
- Strange behaviour → [`06-faq.md`](06-faq.md).
