# 2. Installation

You only do this once, then again whenever BeSt publishes a new dump (the index is frozen against the dump used at build time). For refreshes, `scripts/refresh_best.sh` automates steps 4 and 5, see [Refreshing the data](#refreshing-the-data).

## Hardware prerequisites

- **Rust 1.90 or newer** (install via `rustup`, see below).
- **~3 GB free disk** for the Tantivy index (~2.2 GB) + side-files (~13 MB) + headroom. A refresh keeps the previous index next to the new one until you delete it: plan ~5 GB if you use the refresh script.
- **~4 GB free RAM** during indexing (mostly to hold the dictionary join in memory). The running service uses less.
- **Linux, macOS or Windows**. Linux and macOS are the tested paths; on Windows, WSL2 makes life simpler.

## Step 1: Install Rust

If `cargo --version` returns nothing:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
# Follow default prompts, then reload your shell:
source "$HOME/.cargo/env"
cargo --version    # should show "cargo 1.90.x" or newer
```

On native Windows (no WSL): use [https://www.rust-lang.org/tools/install](https://www.rust-lang.org/tools/install) and restart your terminal.

## Step 2: Get the code

```bash
git clone https://github.com/brunomoyse/crabfinder.git
cd crabfinder
```

(If you were handed a `.tar.gz` archive instead of a git repo, extract it and `cd` into the extracted folder.)

## Step 3: Build the binaries

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

## Step 4: Download the BeSt Address data

BeSt publishes one ZIP bundle per region. You need all three to cover Belgium.

### Official sources

Download page: [https://opendata.bosa.be/](https://opendata.bosa.be/), search for "BeSt Address" in the catalogue.

Three archives to fetch:
- `BrusselsAddress_*.zip`
- `FlandersAddress_*.zip`
- `WalloniaAddress_*.zip`

Plus, **from the same catalogue page**, the sibling archives:
- `Municipality_*.zip`
- `PostalInfo_*.zip`
- `StreetName_*.zip`
- `PartOfMunicipality_*.zip` (Wallonia only: Brussels and Flanders don't publish this entity)

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

## Step 5: Build the index

```bash
./target/release/crabfinder-indexer \
    --best-dir ./best-full-latest \
    --output ./index-belgium \
    --notability-whitelist data/notability-whitelist-v1.yaml
```

Typical output (2026-10-15 dump, Apple Silicon laptop):

```
====== indexing summary ======
  Brussels: pass1=   49ms  pass2=   9772ms  parsed=848825 emitted=844058 skipped_status=4767
  Flanders: pass1=  265ms  pass2=  52383ms  parsed=4218617 emitted=3933743 skipped_status=284874
  Wallonia: pass1=  192ms  pass2=  25818ms  parsed=2107430 emitted=2107430 skipped_status=0 ... missing_position=140263 ...
     TOTAL: parsed=7174872 emitted=6885231 skipped_status=289641 skipped_unresolved=0 missing_position=140263 ...
  mid_street entries: 145123
  imputed positions: same_number=44909 interpolated=87293 street=0 locality=8061 dropped=0
  history records: 289641
  localities: 3065 (OSM places loaded: 0)
  commit: 856ms
  wall-clock total: 93063ms (1.6 min)
  index size on disk: 2394290026 bytes (2283.4 MB / 2.23 GB)
```

How to read it:

- `skipped_status`: retired, reserved and rejected addresses, left out of the search (current and proposed addresses are indexed). They are kept in the history file (`history records`) so old ids still resolve.
- `missing_position` / `imputed positions`: BeSt publishes some Walloon addresses (140,263 in this dump) without a position yet (Lambert 72 `0 0`, which would land in France). The indexer gives them a position from their neighbours instead, and records how in `position_source`, see [`05-understanding-results.md`](05-understanding-results.md#where-the-coordinates-come-from-position_source).
- `localities`: the place names the service understands in a place-name query: BeSt municipalities, parts of municipalities and postal areas ("Spontin", "Han-sur-Lesse"), plus OpenStreetMap villages and hamlets when built with `--places` (then `OSM places loaded` is non-zero).

The `./index-belgium/` folder now contains:

- Tantivy segment files (`*.idx`, `*.pos`, `*.term`, etc.).
- A `.crab-meta.json` sidecar with the schema version (currently **12**; an index built by an older version must be rebuilt) and the build time.
- `mid-street-v1.bin` (~10 MB): the street-level points used by the `median_street` fallback.
- `localities-v1.json` (~3 MB): the place names used to understand place-name queries.
- `history-v1.json.gz` (~10 MB): the retired, reserved and rejected addresses, so `/lookup` and the batch `best_id` column still resolve old ids.

**The BeSt dump used is frozen into the index.** When a new dump comes out, redo step 4 (replace the ZIPs) then step 5 (rebuild). This is not an incremental migration. The indexer refuses an `--output` folder that already holds an index (it would otherwise append a second copy of every address): delete it first, or pass `--overwrite`.

### Handy flags

| Flag | When to use it |
|---|---|
| `--region brussels` (or `flanders` / `wallonia`) | Tests, or you only need one region |
| `--limit 1000` | Quick smoke test (1000 addresses per region) |
| `--print-samples 5` | Print 5 parsed addresses before indexing to eyeball-check |
| `--overwrite` | Replace an index already in `--output` (checked only once every input has loaded, so a bad flag never costs you the old index) |
| `--notability-whitelist PATH` | Ranking boost for iconic places (see below) |
| `--places PATH` | Optional OpenStreetMap helper: hamlet and village names (see below) |

The whitelist ships in `data/`, so might as well use it: without `--notability-whitelist`, everything still works but iconic toponyms (Grand-Place, Atomium…) no longer get a notability boost in the ranking.

### OpenStreetMap helpers (optional)

BeSt Address is the only source of what crabfinder returns. Two optional OpenStreetMap files, both off by default, only help it *understand* queries that BeSt alone can't place:

| Helper | Flag | Lets crabfinder understand |
|---|---|---|
| `data/osm-places-be-v1.csv` (villages, hamlets) | indexer `--places` | "Maredsous", a hamlet with no BeSt entity of its own (part of Anhée) |
| `data/osm-pois-be-v1.csv` (landmarks) | service / batch `--pois` (step 6) | "Manneken Pis", "Gare du Midi", "Gravensteen Gent" |

Either way the answer is the nearest BeSt address (`confidence: locality` / `landmark`); no OSM name or position is ever returned, and no address is added or changed. Without them, only BeSt municipalities, parts of municipalities and postal areas are recognised as places.

To use the places, add `--places data/osm-places-be-v1.csv` to the indexer command above. The landmarks are loaded by the service at startup, so updating that file never needs a reindex. Both CSVs are derived from OpenStreetMap (© OpenStreetMap contributors, ODbL 1.0, see `data/README.md`).

## Step 6: Start the HTTP service (for online search)

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
INFO index sidecar OK schema_version=12 tokenizer=crab
INFO spatial index built points=6878576 elapsed_ms=1605
INFO abbreviation dictionary loaded path="data/street-type-abbreviations-v1.yaml" n_entries=17 ...
INFO address-count index built elapsed_ms=20048 n_docs=6885231 n_streets=144205 max_count=5649
INFO mid_street side-file loaded path=./index-belgium/mid-street-v1.bin entries=145123
INFO localities loaded entries=…
INFO listening at http://127.0.0.1:8080
```

The service only starts listening once everything is loaded (~20 s on a laptop), so the first request always sees a ready index. After that, every request is logged with its status and latency (`RUST_LOG=warn` silences that).

Quick test from another terminal:

```bash
curl "http://127.0.0.1:8080/health"
# {"status":"ok","index_path":"./index-belgium","index_docs":6885231,"uptime_secs":12,
#  "schema_version":12,"index_built_at_unix_secs":1792106077,"version":"0.1.0"}

curl "http://127.0.0.1:8080/geocode?q=Grand-Place+1000&limit=2"
```

### Service flags

| Flag | Default | Effect |
|---|---|---|
| `--index PATH` | n/a (required) | Index directory. |
| `--host IP` | `127.0.0.1` | Address to bind. `0.0.0.0` to listen on every interface (containers). |
| `--port N` | `8080` | TCP port. |
| `--abbreviation-dict PATH` | (none) | `av` → `avenue` expansion. **Recommended**: `data/street-type-abbreviations-v1.yaml`. |
| `--pois PATH` | (none) | Optional OpenStreetMap helper (`data/osm-pois-be-v1.csv`): understand landmark names ("Manneken Pis", "Gare du Midi"); the answer is the nearest BeSt address, `confidence: landmark`. Without it, landmark names aren't recognised. |
| `--request-timeout-ms N` | `10000` | Deadline of a request, time waiting for a free slot included. Past it the service answers `503` with a JSON error. |
| `--batch-timeout-secs N` | `600` | Same for `POST /batch/geocode`, for the whole request: past it the response is cut off. Also the upload deadline of `POST /batch/jobs`. |
| `--max-concurrency N` | `256` | Requests handled at once; the others wait for a slot (within their deadline). |
| `--cors-origin URL` | any (`*`) | Allowed browser origin. Repeat the flag (or separate with commas) for several. |
| `--no-metrics` | off | Don't serve Prometheus metrics at `/metrics`. |
| `--no-notability-boost` | off | Rank without the notability boost (A/B testing). |
| `--no-autocomplete` | off | Don't build the `/autocomplete` suggestion index (saves about 0.8 s at startup and 80 MB); the route then answers `503`. |
| `--jobs-dir PATH` | (none) | Turn on batch jobs (`/batch/jobs`), stored in this directory. Without it those routes answer `503`. |
| `--jobs-max-running N` | `2` | Batch jobs running at once; the others wait. |
| `--jobs-max-body-mb N` | `1024` | Largest file a batch job accepts, in MiB. |
| `--jobs-ttl-hours N` | `24` | How long a finished job and its result are kept. |
| `--jobs-threads N` | `0` | Threads the batch jobs share; `0` = half the cores. |

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

Same for `crabfinder-batch` (`CRABFINDER_INDEX`, `CRABFINDER_ABBREVIATION_DICT`, and `CRABFINDER_POIS` for the optional landmarks helper).

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

Inside the image the service listens on `0.0.0.0` (`CRABFINDER_HOST`) and the abbreviation dictionary is preconfigured. The optional OpenStreetMap helpers are not bundled. To index hamlet names, mount `data/` and add `--places`:

```bash
docker run --rm -v ./best-full-latest:/best:ro -v ./data:/data:ro -v crabfinder-index:/index crabfinder \
    crabfinder-indexer --best-dir /best --output /index \
    --notability-whitelist /usr/share/crabfinder/notability-whitelist-v1.yaml \
    --places /data/osm-places-be-v1.csv
```

Likewise for the optional landmarks helper, which the service loads at startup: mount `data/` on the serving container and pass `--pois`:

```bash
docker run -d --name crabfinder -p 8080:8080 -v crabfinder-index:/index -v ./data:/data:ro crabfinder \
    crabfinder-service --pois /data/osm-pois-be-v1.csv
```

## Refreshing the data

`scripts/refresh_best.sh` does a full refresh in one go:

1. downloads the BeSt bundle (`best-full-latest.zip` from opendata.bosa.be) into `best-<date>/` and unpacks it;
2. builds the release binaries and indexes into `index-<date>/` (with the whitelist; BeSt only, unless run with `WITH_OSM=1`, which adds `--places data/osm-places-be-v1.csv`);
3. starts a temporary service on port 18080 and runs `crabfinder-eval --validate-eval-set`: every address the evaluation set expects must still exist (the script stops if more than 5% are missing);
4. points the `index-current` symlink at the new index, atomically.

```bash
scripts/refresh_best.sh
# [refresh] index-current -> index-2026-10-15
# [refresh] restart the service with: crabfinder-service --index ./index-current
```

The running service keeps serving the old index until you restart it against `index-current`. Options (environment variables): `DATA_DIR=/srv/crab` to work elsewhere than the repo root, `SKIP_DOWNLOAD=1` to reuse today's `best-<date>/`, `SKIP_VALIDATE=1` to skip step 3, `WITH_OSM=1` to index the optional OSM places, `BEST_URL=…` to use another download URL, `STAMP=2026-10-15` to name the folders differently from today's date (an existing `index-<stamp>/` is deleted and rebuilt), `VALIDATE_PORT=N` to move the validation service off port 18080, `EVAL_SETS="a.yaml b.json"` to validate other sets, `RESTART_CMD="…"` to restart the service after the swap. Old `best-*` / `index-*` folders are not deleted: remove them yourself once the new index is in service.

### Scheduling the refresh

BeSt is republished regularly; a weekly run is a reasonable cadence. The script exits non-zero (and leaves `index-current` untouched) when the download, the build or the validation fails, so a scheduler can alert on failure. With cron:

```cron
# Every Monday at 03:00; the service restarts on the new index if all went well.
0 3 * * 1  cd /srv/crabfinder && DATA_DIR=/srv/crab RESTART_CMD="systemctl restart crabfinder" scripts/refresh_best.sh >> /var/log/crabfinder-refresh.log 2>&1
```

With systemd, a oneshot service plus a timer:

```ini
# /etc/systemd/system/crabfinder-refresh.service
[Service]
Type=oneshot
WorkingDirectory=/srv/crabfinder
Environment=DATA_DIR=/srv/crab
Environment="RESTART_CMD=systemctl restart crabfinder"
ExecStart=/srv/crabfinder/scripts/refresh_best.sh

# /etc/systemd/system/crabfinder-refresh.timer
[Timer]
OnCalendar=Mon *-*-* 03:00
Persistent=true
[Install]
WantedBy=timers.target
```

Each run needs ~3 GB free for the new `best-<date>/` and `index-<date>/`; clean up old ones (e.g. keep the last two) in the same job.

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
