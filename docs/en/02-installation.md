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
| `--batch-max-body-mb N` | `50` | Largest body `POST /batch/geocode` accepts, in MiB. |
| `--api-keys PATH` | (none) | Require API keys (see [Protection](#protection)). |
| `--rate-limit-per-ip N` | (none) | At most N requests per minute per client address. |
| `--metrics-require-key` | off | With `--api-keys`, `/metrics` needs a key too. |

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

## Protection

Off by default: the service answers anyone who can reach it, which is fine on localhost or behind your own gateway. To expose it more widely, turn on API keys, rate limits, or both.

API keys go in a YAML file:

```yaml
# keys.yaml (keep it private: chmod 600)
keys:
  - name: website
    key: "6f1c0b9e4d2a8f7e3b5c1a9d"   # at least 16 characters, e.g. `openssl rand -hex 16`
    rate_per_minute: 600              # optional
  - name: nightly-import
    key: "b81e44c02f9a7d3e6c5b1f08"
    batch: true                       # may use POST /batch/geocode
    jobs: true                        # may use /batch/jobs
```

```bash
./target/release/crabfinder-service --index ./index-belgium --api-keys keys.yaml
curl -H "Authorization: Bearer 6f1c0b9e4d2a8f7e3b5c1a9d" "http://127.0.0.1:8080/geocode?q=Grote+Markt+1+Brugge"
```

- Clients send their key as `Authorization: Bearer <key>` or `X-API-Key: <key>`. No key or a wrong one gives `401`.
- `/health` stays open (for load balancers), and so does `/metrics` unless `--metrics-require-key`.
- A key can only make the interactive requests (`/geocode`, `/reverse`, `/autocomplete`, …) unless it has `batch: true` or `jobs: true`; otherwise `403`.
- Over its `rate_per_minute`, a key gets `429` with a `Retry-After` header. `--rate-limit-per-ip N` limits every client address the same way, with or without keys. Behind a reverse proxy all requests seem to come from the proxy: limit per address there instead.
- Each key only sees its own batch jobs.
- `/metrics` counts requests per key (`key` label) and refusals (`crabfinder_http_rejected_total`, reasons `unauthorized`, `forbidden`, `rate_limited`).

## Docker (alternative to steps 1, 3, 5 and 6)

The `Dockerfile` builds `crabfinder-service`, `crabfinder-indexer` and `crabfinder-batch` into one image (about 140 MB) with the two YAML files and `refresh_best.sh` bundled. The image has a health check on `/health`.

### With Docker Compose (recommended)

Nothing to download by hand: the `refresh` service fetches BeSt itself.

```bash
docker compose up -d
docker compose logs -f refresh      # first run: download + indexing, a few minutes
curl "http://127.0.0.1:8080/health"
```

What happens:

1. `refresh` runs `refresh_best.sh` in the `data` volume: downloads the BeSt dump, builds `index-<date>`, checks it holds at least 6 million addresses, points `index-current` at it, deletes the download, then exits. While the index is less than 7 days old it does nothing, so later `docker compose up` are quick.
2. `geocoder` starts once `refresh` succeeded: it serves `/data/index-current` on `127.0.0.1:8080` (loopback only), with batch jobs in the `jobs` volume.

To refresh every week (BeSt publishes weekly), from the host's cron:

```bash
0 6 * * 1  cd /path/to/crabfinder && docker compose run --rm -e REFRESH_IF_OLDER_THAN_DAYS= refresh && docker compose restart geocoder
```

To publish the service beyond the host, put a reverse proxy with TLS in front, and turn on API keys or rate limits: uncomment the `CRABFINDER_API_KEYS` / `CRABFINDER_RATE_LIMIT_PER_IP` lines in `docker-compose.yml` (see [Protection](#protection)). The optional OpenStreetMap landmarks helper has commented lines there too; it stays off by default.

### By hand

Only the BeSt data (step 4) is needed on the host. The index lives in a volume mounted at `/index`, which must be writable (Tantivy takes a lock file even to read).

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
docker run --rm -v ./best-full-latest:/best:ro -v ./data:/osm:ro -v crabfinder-index:/index crabfinder \
    crabfinder-indexer --best-dir /best --output /index \
    --notability-whitelist /usr/share/crabfinder/notability-whitelist-v1.yaml \
    --places /osm/osm-places-be-v1.csv
```

Likewise for the optional landmarks helper, which the service loads at startup: mount `data/` on the serving container and pass `--pois`:

```bash
docker run -d --name crabfinder -p 8080:8080 -v crabfinder-index:/index -v ./data:/osm:ro crabfinder \
    crabfinder-service --pois /osm/osm-pois-be-v1.csv
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

The running service keeps serving the old index until you restart it against `index-current`. Options (environment variables): `DATA_DIR=/srv/crab` to work elsewhere than the repo root, `SKIP_DOWNLOAD=1` to reuse today's `best-<date>/`, `SKIP_VALIDATE=1` to skip step 3, `WITH_OSM=1` to index the optional OSM places, `BEST_URL=…` to use another download URL, `STAMP=2026-10-15` to name the folders differently from today's date (an existing `index-<stamp>/` is deleted and rebuilt), `VALIDATE_PORT=N` to move the validation service off port 18080, `EVAL_SETS="a.yaml b.json"` to validate other sets, `RESTART_CMD="…"` to restart the service after the swap, `REFRESH_IF_OLDER_THAN_DAYS=7` to do nothing while `index-current` is younger than that, `PRUNE=1` to delete the download once indexed and every index but the new one and the previous one, `BIN_DIR=…` to use already-built binaries instead of building them (then, without `crabfinder-eval` there, step 3 only checks the index holds at least `MIN_DOCS` addresses, 6 million by default). The index being served is never rebuilt in place: a second run on the same day gets a time suffix. Without `PRUNE=1`, old `best-*` / `index-*` folders are kept: remove them yourself once the new index is in service.

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
