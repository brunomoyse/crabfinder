# 2. Installation

You only do this once, then again whenever BeSt publishes a new dump (the index is frozen against the dump used at build time). For refreshes, `scripts/refresh_best.sh` automates steps 4 and 5 — see [Refreshing the data](#refreshing-the-data).

## Hardware prerequisites

- **Rust 1.90 or newer** (install via `rustup`, see below).
- **~3 GB free disk** for the Tantivy index (~2.2 GB) + side-files (~13 MB) + headroom. A refresh keeps the previous index next to the new one until you delete it: plan ~5 GB if you use the refresh script.
- **~4 GB free RAM** during indexing (mostly to hold the dictionary join in memory). The running service uses less.
- **Linux, macOS or Windows**. Linux and macOS are the tested paths; on Windows, WSL2 makes life simpler.

## Step 1 — Install Rust

If `cargo --version` returns nothing:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
# Follow default prompts, then reload your shell:
source "$HOME/.cargo/env"
cargo --version    # should show "cargo 1.90.x" or newer
```

On native Windows (no WSL): use [https://www.rust-lang.org/tools/install](https://www.rust-lang.org/tools/install) and restart your terminal.

## Step 2 — Get the code

```bash
git clone https://github.com/brunomoyse/crabfinder.git
cd crabfinder
```

(If you were handed a `.tar.gz` archive instead of a git repo, extract it and `cd` into the extracted folder.)

## Step 3 — Build the binaries

```bash
cargo build --release
```

This compiles **all** binaries: `crabfinder-indexer`, `crabfinder-service`, `crabfinder-batch`, `crabfinder-eval`. First-time builds take 3-8 min depending on the machine (Tantivy is the heavy dep). The executables land in `target/release/`.

**Always use `--release`**: a debug `cargo build` is 10× slower and indexing won't complete in a reasonable time.

Sanity check:

```bash
./target/release/crabfinder-indexer --version
./target/release/crabfinder-service --version
./target/release/crabfinder-batch --version
```

## Step 4 — Download the BeSt Address data

BeSt publishes one ZIP bundle per region. You need all three to cover Belgium.

### Official sources

Download page: [https://opendata.bosa.be/](https://opendata.bosa.be/) — search for "BeSt Address" in the catalogue.

Three archives to fetch:
- `BrusselsAddress_*.zip`
- `FlandersAddress_*.zip`
- `WalloniaAddress_*.zip`

Plus, **from the same catalogue page**, the sibling archives:
- `Municipality_*.zip`
- `PostalInfo_*.zip`
- `StreetName_*.zip`
- `PartOfMunicipality_*.zip` (Wallonia only — Brussels and Flanders don't publish this entity)

### Layout

Create a `best-full-latest/` folder and **drop all ZIP archives into it without unzipping them**. The indexer reads ZIPs directly.

```bash
mkdir best-full-latest
mv ~/Downloads/*Address*.zip best-full-latest/
mv ~/Downloads/Municipality*.zip best-full-latest/
mv ~/Downloads/PostalInfo*.zip best-full-latest/
mv ~/Downloads/StreetName*.zip best-full-latest/
mv ~/Downloads/PartOfMunicipality*.zip best-full-latest/

ls best-full-latest/      # check the ~10 ZIPs are there
```

## Step 5 — Build the index

```bash
./target/release/crabfinder-indexer \
    --best-dir ./best-full-latest \
    --output ./index-belgium \
    --notability-whitelist data/notability-whitelist-v1.yaml \
    --places data/osm-places-be-v1.csv
```

Typical output (2026-10-15 dump, Apple Silicon laptop):

```
====== indexing summary ======
  Brussels: pass1=   49ms  pass2=   9772ms  parsed=848825 emitted=845464 skipped_status=3361 ... places_attached=414804
  Flanders: pass1=  265ms  pass2=  52383ms  parsed=4218617 emitted=3933743 skipped_status=284874 ... places_attached=3014584
  Wallonia: pass1=  192ms  pass2=  25818ms  parsed=2107430 emitted=2107430 skipped_status=0 ... missing_position=140263 ...
     TOTAL: parsed=7174872 emitted=6886637 skipped_status=288235 skipped_unresolved=0 missing_position=140263 ...
  mid_street entries: 145123
  imputed positions: same_number=44909 interpolated=87293 street=0 locality=8061 dropped=0
  localities: 16045 (OSM places loaded: 12980)
  commit: 856ms
  wall-clock total: 91279ms (1.5 min)
  index size on disk: 2312810065 bytes (2205.7 MB / 2.15 GB)
```

How to read it:

- `skipped_status`: retired / proposed addresses, deliberately left out (only current addresses are indexed).
- `missing_position` / `imputed positions`: BeSt publishes some Walloon addresses (140,263 in this dump) without a position yet (Lambert 72 `0 0`, which would land in France). The indexer gives them a position from their neighbours instead, and records how in `position_source` — see [`05-understanding-results.md`](05-understanding-results.md#where-the-coordinates-come-from-position_source).
- `localities`: municipalities, parts of municipalities, postal areas and OpenStreetMap villages/hamlets the service can answer for a place-name query ("Maredsous", "Spontin").

The `./index-belgium/` folder now contains:

- Tantivy segment files (`*.idx`, `*.pos`, `*.term`, etc.).
- A `.crab-meta.json` sidecar with the schema version (currently **9**) and the build time.
- `mid-street-v1.bin` (~10 MB): the street-level points used by the `median_street` fallback.
- `localities-v1.json` (~3 MB): the place names and centres used by locality queries.

**The BeSt dump used is frozen into the index.** When a new dump comes out, redo step 4 (replace the ZIPs) then step 5 (rebuild). This is not an incremental migration.

### Handy flags

| Flag | When to use it |
|---|---|
| `--region brussels` (or `flanders` / `wallonia`) | Tests, or you only need one region |
| `--limit 1000` | Quick smoke test (1000 addresses per region) |
| `--print-samples 5` | Print 5 parsed addresses before indexing to eyeball-check |
| `--notability-whitelist PATH` | Ranking boost for iconic places (see below) |
| `--places PATH` | OpenStreetMap villages and hamlets (see below) |

Both data files ship in `data/`, so might as well use them:

- Without `--notability-whitelist`, everything still works but iconic toponyms (Grand-Place, Atomium…) no longer get a notability boost in the ranking.
- Without `--places`, place-name queries only know BeSt municipalities, parts of municipalities and postal areas: a hamlet such as "Maredsous" (part of Anhée, no BeSt entity of its own) is no longer found. The CSV is derived from OpenStreetMap (© OpenStreetMap contributors, ODbL 1.0, see `data/README.md`).

## Step 6 — Start the HTTP service (for online search)

If you'll only use the batch CLI, skip this.

```bash
./target/release/crabfinder-service \
    --index ./index-belgium \
    --abbreviation-dict data/street-type-abbreviations-v1.yaml \
    --port 8080
```

Expected output:

```
INFO starting crabfinder-service index=./index-belgium port=8080
INFO index sidecar OK schema_version=9 tokenizer=crab
INFO spatial index built points=6878576 elapsed_ms=1605
INFO abbreviation dictionary loaded path="data/street-type-abbreviations-v1.yaml" n_entries=17 ...
INFO address-count index built elapsed_ms=20048 n_docs=6886637 n_streets=144205 max_count=5649
INFO mid_street side-file loaded path=./index-belgium/mid-street-v1.bin entries=145123
INFO localities loaded entries=16045
INFO listening at http://127.0.0.1:8080
```

The service only starts listening once everything is loaded (~20 s on a laptop), so the first request always sees a ready index. After that, every request is logged with its status and latency (`RUST_LOG=warn` silences that).

Quick test from another terminal:

```bash
curl "http://127.0.0.1:8080/health"
# {"status":"ok","index_path":"./index-belgium","index_docs":6886637,"uptime_secs":12,
#  "schema_version":9,"index_built_at_unix_secs":1792100762,"version":"0.1.0"}

curl "http://127.0.0.1:8080/geocode?q=Grand-Place+1000&limit=2"
```

### Service flags

| Flag | Default | Effect |
|---|---|---|
| `--index PATH` | — (required) | Index directory. |
| `--host IP` | `127.0.0.1` | Address to bind. `0.0.0.0` to listen on every interface (containers). |
| `--port N` | `8080` | TCP port. |
| `--abbreviation-dict PATH` | (none) | `av` → `avenue` expansion. **Recommended**: `data/street-type-abbreviations-v1.yaml`. |
| `--request-timeout-ms N` | `10000` | Deadline of a request, time waiting for a free slot included. Past it the service answers `503` with a JSON error. |
| `--batch-timeout-secs N` | `600` | Same for `POST /batch/geocode`, until the response starts streaming. |
| `--max-concurrency N` | `256` | Requests handled at once; the others wait for a slot (within their deadline). |
| `--cors-origin URL` | any (`*`) | Allowed browser origin. Repeat the flag (or separate with commas) for several. |
| `--no-metrics` | off | Don't serve Prometheus metrics at `/metrics`. |
| `--no-notability-boost` | off | Rank without the notability boost (A/B testing). |

To stop: `Ctrl+C` in the running terminal.

### Running in the background

```bash
nohup ./target/release/crabfinder-service \
    --index ./index-belgium \
    --abbreviation-dict data/street-type-abbreviations-v1.yaml \
    > crabfinder.log 2>&1 &
echo $! > crabfinder.pid
```

To stop: `kill $(cat crabfinder.pid)`.

## Environment variables (alternative to flags)

Every service flag has an env-var equivalent (`CRABFINDER_` + the flag name in capitals: `CRABFINDER_REQUEST_TIMEOUT_MS`, `CRABFINDER_MAX_CONCURRENCY`, `CRABFINDER_CORS_ORIGIN`, `CRABFINDER_NO_METRICS`…), handy for systemd/Docker setups:

```bash
export CRABFINDER_INDEX=./index-belgium
export CRABFINDER_HOST=127.0.0.1      # default; 0.0.0.0 to listen on all interfaces
export CRABFINDER_PORT=8080
export CRABFINDER_ABBREVIATION_DICT=./data/street-type-abbreviations-v1.yaml

./target/release/crabfinder-service     # no flags needed
```

Same for `crabfinder-batch` (`CRABFINDER_INDEX`, `CRABFINDER_ABBREVIATION_DICT`).

## Docker (alternative to steps 1, 3, 5 and 6)

The `Dockerfile` builds `crabfinder-service`, `crabfinder-indexer` and `crabfinder-batch` into one image with the two YAML files bundled. Only the BeSt data (step 4) is needed on the host. The index lives in a volume mounted at `/index`, which must be writable (Tantivy takes a lock file even to read).

```bash
docker build -t crabfinder .

# Build the index into a named volume (~2–3 min)
docker run --rm -v ./best-full-latest:/best:ro -v crabfinder-index:/index crabfinder \
    crabfinder-indexer --best-dir /best --output /index \
    --notability-whitelist /usr/share/crabfinder/notability-whitelist-v1.yaml

# Serve it
docker run -d --name crabfinder -p 8080:8080 -v crabfinder-index:/index crabfinder
curl "http://127.0.0.1:8080/health"

# Batch over stdin/stdout
docker run --rm -i -v crabfinder-index:/index crabfinder crabfinder-batch < input.csv > output.csv
```

Inside the image the service listens on `0.0.0.0` (`CRABFINDER_HOST`) and the abbreviation dictionary is preconfigured. The OpenStreetMap places CSV is not bundled: to index hamlets, mount `data/` and add `--places`:

```bash
docker run --rm -v ./best-full-latest:/best:ro -v ./data:/data:ro -v crabfinder-index:/index crabfinder \
    crabfinder-indexer --best-dir /best --output /index \
    --notability-whitelist /usr/share/crabfinder/notability-whitelist-v1.yaml \
    --places /data/osm-places-be-v1.csv
```

## Refreshing the data

`scripts/refresh_best.sh` does a full refresh in one go:

1. downloads the BeSt bundle (`best-full-latest.zip` from opendata.bosa.be) into `best-<date>/` and unpacks it;
2. builds the release binaries and indexes into `index-<date>/` (with the whitelist, and `--places` when `data/osm-places-be-v1.csv` is present);
3. starts a temporary service on port 18080 and runs `crabfinder-eval --validate-eval-set`: every address the evaluation set expects must still exist (the script stops if more than 5% are missing);
4. points the `index-current` symlink at the new index, atomically.

```bash
scripts/refresh_best.sh
# [refresh] index-current -> index-2026-10-15
# [refresh] restart the service with: crabfinder-service --index ./index-current
```

The running service keeps serving the old index until you restart it against `index-current`. Options (environment variables): `DATA_DIR=/srv/crab` to work elsewhere than the repo root, `SKIP_DOWNLOAD=1` to reuse today's `best-<date>/`, `SKIP_VALIDATE=1` to skip step 3, `BEST_URL=…` to use another download URL. Old `best-*` / `index-*` folders are not deleted: remove them yourself once the new index is in service.

## Uninstall

Nothing outside the repo. To remove everything:

```bash
rm -rf crabfinder/
# Optional: uninstall Rust if no longer used
rustup self uninstall
```

## What's next

- Geocode a CSV → [`03-batch.md`](03-batch.md).
- Query the service over HTTP → [`04-online-search.md`](04-online-search.md).
- An error at startup? → [`06-faq.md`](06-faq.md).
