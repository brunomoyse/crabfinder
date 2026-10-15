# crabfinder

A Belgium-only geocoder, written in Rust. Indexes the federal **BeSt
Address** open dataset (6.8M addresses, three regions) into a single
Tantivy index and serves geocoding + reverse geocoding over HTTP.

```
GET /geocode?q=Avenue Louise 32 1050 Ixelles
GET /reverse?lat=50.8466&lng=4.3525
```

## Quick start

Requires Rust 1.90+, ~3 GB free disk for the index, and the BeSt zip
bundle.

```bash
# 1. Build everything (release mode — debug is too slow for Tantivy)
cargo build --release

# 2. Index the full Belgian dump (~2.3 min, ~2 GB)
./target/release/crabfinder-indexer \
    --best-dir ./best-full-latest \
    --output ./index-belgium \
    --notability-whitelist data/notability-whitelist-v1.yaml

# 3. Start the HTTP service
./target/release/crabfinder-service \
    --index ./index-belgium \
    --abbreviation-dict data/street-type-abbreviations-v1.yaml \
    --port 8080 &

# 4. Query
curl "http://127.0.0.1:8080/health"
curl "http://127.0.0.1:8080/geocode?q=Grand-Place+1000+Bruxelles&limit=3"
curl "http://127.0.0.1:8080/reverse?lat=50.8466&lng=4.3525&limit=1"
```

The service binds IPv4-only, so always hit it with `127.0.0.1` (not
just `localhost`, which may resolve to IPv6 first on some platforms).

## Quality

Tier A recall@1 ≈ 0.93, recall@10 ≈ 0.99 on the synthetic eval set
(2700 queries across Brussels + Flanders + Wallonia, 6 variant families
including cross-language). p99 /geocode latency ~20 ms HTTP loopback.

Run an eval yourself: see [`docs/eval.md`](docs/eval.md).

## Docs

User-facing guides (install, run, batch, troubleshoot):

- [docs/en/](docs/en/README.md) — English
- [docs/fr/](docs/fr/README.md) — Français

Technical references:

| Doc | What |
|---|---|
| [docs/architecture.md](docs/architecture.md) | Crate layout, Tantivy schema, query pipeline, scoring |
| [docs/api.md](docs/api.md) | HTTP endpoints reference |
| [docs/eval.md](docs/eval.md) | Eval methodology + how to run |

In addition to `GET /geocode` and `GET /reverse`, crabfinder ships a
batch geocoding stack: `POST /batch/geocode` (HTTP) and the standalone
`crabfinder-batch` binary (CLI). Both consume a clean-columns CSV
(`id,street,house_number,postal_code`) and emit an enriched CSV with
per-row `confidence` + `fallback_reason`. See
[docs/en/03-batch.md](docs/en/03-batch.md) /
[docs/fr/03-batch.md](docs/fr/03-batch.md).

## Inputs

- **BeSt zip bundle** — the federal Belgian address dataset. Drop the
  `BrusselsAddress*.zip`, `FlandersAddress*.zip`, `WalloniaAddress*.zip`
  files (plus their `Municipality*`, `PostalInfo*`, `StreetName*`,
  `PartOfMunicipality*` siblings) into one directory and pass it via
  `--best-dir`.
- **Notability whitelist** (`data/notability-whitelist-v1.yaml`) — a
  curated `building_group_id → multiplier` map that lifts iconic
  toponyms (Grand-Place, Atomium, …) above look-alikes in lesser
  cities.
- **Abbreviation dictionary** (`data/street-type-abbreviations-v1.yaml`)
  — expands user-typed abbreviations at query time
  (`Av. Louise` → `Avenue Louise`, `Stationsstr.` → `Stationsstraat`).

Both extra inputs are optional. Without them the engine still runs;
just expect lower scores on notability-sensitive and abbreviated
queries.

## License

MIT OR Apache-2.0. BeSt address data is © the Belgian federal
authorities; redistribute per their open-data license.
