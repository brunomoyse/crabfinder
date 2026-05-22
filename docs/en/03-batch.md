# 3. Batch geocoding

Use case: you have a list of addresses (CSV, Excel export, SQL query) and you want an enriched file out with lat/lng plus a per-row quality flag.

Two ways to do this, **with the exact same resolution logic underneath**:

- **`crabfinder-batch` CLI** — no server, no port, you run a binary, feed it a CSV, get a CSV. Preferred for data pipelines, cron scripts, one-off exports.
- **`POST /batch/geocode` HTTP endpoint** — the service is already running, you push a CSV per HTTP request, receive a streamed CSV. Preferred for application integration (a backend that geocodes on demand).

If unsure: the CLI is simpler to start with. Jump to [CLI](#cli--crabfinder-batch).

---

## Input format

**Only one format supported in v1**: CSV with named, separated columns. Header row mandatory.

```csv
id,street,house_number,postal_code
1,Avenue Louise,32,1050
2,Rue Belliard,71,1040
3,Grand-Place,,1000
4,Stationsstraat,5,2000
5,Some Nonexistent Street,99,1000
```

Rules:

| Column | Type | Required | Notes |
|---|---|---|---|
| `id` | string | yes | Free-form identifier. Copied verbatim into the output so you can join back to your source data. |
| `street` | string | yes | Street name. Can contain accents, hyphens, multiple words. No normalization required. |
| `house_number` | string | no | Building number. Empty allowed. Alphabetic suffixes (`32A`, `12B`) are preserved. |
| `postal_code` | string | no | Belgian 4-digit postal code. Empty allowed (strongly discouraged: without postal code, homonymous streets won't be disambiguated). |

**Encoding**: UTF-8. If your CSV is `windows-1252` (typical French/Dutch Excel export), convert first:

```bash
iconv -f WINDOWS-1252 -t UTF-8 addresses-export.csv > addresses.csv
```

**Delimiter**: comma (`,`) or semicolon (`;`). Auto-detected on the first line; force with `--delimiter ';'` on the CLI if in doubt.

**Empty rows**: ignored. **Malformed rows**: the CLI aborts with an error pointing at the line number. The HTTP endpoint returns a 400 with the same message.

## Output format

15 columns, in this order (always):

```csv
id,street,house_number,postal_code,matched_street,matched_house_number,matched_postal_code,lat,lng,confidence,fallback_reason,house_number_delta,street_match_score,building_group_id,best_id
```

| Column | Meaning |
|---|---|
| `id`, `street`, `house_number`, `postal_code` | **Copied from input**, audit trail. |
| `matched_street`, `matched_house_number`, `matched_postal_code` | What crabfinder actually found in BeSt. Shows divergences (fuzzy_street etc.). |
| `lat`, `lng` | WGS84 (EPSG:4326). Empty on `not_found`. |
| `confidence` | One of seven labels: `exact`, `median_street`, `fuzzy_street`, `nearest_same_side`, `nearest_any_side`, `adjacent_commune`, `not_found`. Detail in [`05-understanding-results.md`](05-understanding-results.md). |
| `fallback_reason` | Empty if `confidence == exact`. Otherwise one of `house_number_missing`, `house_number_out_of_range`, `street_spelling_fuzzy`, `no_match`. |
| `house_number_delta` | Empty for `exact` (delta = 0 implied) and for street-only confidences. Filled when a neighbouring number was retained. |
| `street_match_score` | Raw BM25 score. Text-match quality indicator. Higher = better match. Empty on `not_found`. |
| `building_group_id` | crabfinder's synthetic building identifier. Stable across dumps as long as BeSt's `street_id` doesn't change. |
| `best_id` | Official BeSt identifier of the chosen address (`namespace/object_identifier/version`). Lets you trace back to the authoritative source. |

**1-in-1-out guarantee**: same row count, same order in and out. A non-geocoded row exits with `confidence=not_found` and empty `lat,lng,matched_*` — **not** dropped from the file.

**Internal deduplication**: if your CSV contains the same address twice (same `street, house_number, postal_code` triplet), crabfinder resolves once and propagates. Transparent optimization, no effect on the output format.

---

## CLI — `crabfinder-batch`

### Minimal invocation

```bash
./target/release/crabfinder-batch \
    --index ./index-belgium \
    --input addresses.csv \
    --output addresses-geocoded.csv
```

The binary opens the Tantivy index read-only, iterates the CSV, writes the result. No HTTP port. No service to start. The `crabfinder-service` can run in parallel against the same index — Tantivy supports multi-process reads.

### All flags

| Flag | Default | Description |
|---|---|---|
| `--index PATH` | — (required) | Tantivy index directory. Env var: `CRABFINDER_INDEX`. |
| `--abbreviation-dict PATH` | (none) | Abbreviations YAML. Without it, queries like "Av. Louise" don't expand to "Avenue Louise". **Recommended**: `data/street-type-abbreviations-v1.yaml`. |
| `-i, --input PATH` | `stdin` | Input CSV. `-` = stdin. |
| `-o, --output PATH` | `stdout` | Output CSV. `-` = stdout. |
| `--delimiter ,` or `;` | auto | Force the input delimiter (otherwise auto-sniffed). |
| `--no-mid-street` | off (=> median fallback on) | Disable the `median_street` fallback. Rows without exact house numbers come out as `not_found` instead of a street centroid. |
| `--parallel N` | all cores | rayon worker threads. `--parallel 1` for deterministic profiling. |

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

#### Streaming, no intermediate file

```bash
psql -c "COPY (SELECT id, street, hn AS house_number, cp AS postal_code FROM clients) TO STDOUT CSV HEADER" \
  | ./target/release/crabfinder-batch \
        --index ./index-belgium \
        --abbreviation-dict data/street-type-abbreviations-v1.yaml \
  > clients-geocoded.csv
```

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
- non-`0`: error (invalid CSV, missing index, wrong sidecar version…). Error message goes to `stderr`; `stdout` stays clean (useful in pipes).

---

## HTTP endpoint — `POST /batch/geocode`

Prerequisite: `crabfinder-service` is running (see [`02-installation.md`](02-installation.md) §6).

### Three body variants

The handler dispatches on `Content-Type`.

#### Variant 1 — multipart/form-data (recommended for large files)

```bash
curl -X POST http://127.0.0.1:8080/batch/geocode \
     -F file=@addresses.csv \
     -F 'options={"mid_street_fallback":true,"format":"csv"}' \
     -o addresses-geocoded.csv
```

Capacity: up to **50 MB body** per request (server default cap). At ~500 bytes/row, that's ~100k rows.

#### Variant 2 — application/json (recommended for small batches and app integration)

```bash
curl -X POST http://127.0.0.1:8080/batch/geocode \
     -H "Content-Type: application/json" \
     -d '{
       "addresses": [
         {"id": "1", "street": "Avenue Louise", "house_number": "32", "postal_code": "1050"},
         {"id": "2", "street": "Grand-Place", "postal_code": "1000"}
       ],
       "options": {"mid_street_fallback": true}
     }' \
     -o batch.csv
```

Capacity: up to **10,000 addresses** per request (HTTP 413 beyond — switch to multipart).

#### Variant 3 — raw CSV body (any Content-Type, simplest to wire)

```bash
curl -X POST http://127.0.0.1:8080/batch/geocode \
     -H "Content-Type: text/csv" \
     --data-binary @addresses.csv \
     -o addresses-geocoded.csv
```

This is the leanest path, useful for testing with a minimal client.

### JSON / multipart options

| Option | Type | Default | Effect |
|---|---|---|---|
| `mid_street_fallback` | bool | `true` | When `false`, disables the `median_street` fallback (same effect as `--no-mid-street` on the CLI). |
| `format` | string | `"csv"` | Reserved for future formats (GeoJSON). Only `"csv"` supported in v1. |

### Response

`Content-Type: text/csv`, **streamed row by row**. The first byte ships as soon as the first resolution results are ready — this matters on large files (TTFB in a few hundred ms instead of several seconds in buffered mode).

### HTTP codes

- `200`: CSV response (may be empty if input had only the header).
- `400`: malformed CSV, malformed JSON, inconsistent content-type.
- `413`: JSON payload > 10,000 addresses.
- `500`: internal error (please report; shouldn't happen).

---

## Edge cases

### Addresses without postal code

Works, but quality strongly degraded. crabfinder can't filter by PC, so homonymous streets ("Rue de la Gare" present in hundreds of municipalities) come back in undefined order. **Always provide the PC when you have it**.

### Addresses with only a municipality name (no street)

Not supported in v1. The `street` column is mandatory. If you just want to geocode "Bruxelles 1000", use the online-search mode with `q=Bruxelles 1000`.

### Duplicates in input

Preserved in output (1-in-1-out). Internally, crabfinder deduplicates the work: only unique addresses get resolved, the result is propagated to every occurrence. No action needed on your side.

### Recommended batch size

- CLI: no limit. 1M rows passes through (memory peaks at a few hundred MB).
- HTTP multipart: 50 MB cap (~100k rows).
- HTTP JSON: 10,000 addresses cap.

To go further over HTTP, chunk it client-side. Or switch to the CLI.

## What's next

- Understanding the `confidence` / `fallback_reason` columns → [`05-understanding-results.md`](05-understanding-results.md).
- Strange behaviour → [`06-faq.md`](06-faq.md).
