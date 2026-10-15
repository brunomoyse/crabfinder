# 2. Installation

You only do this once, then again whenever BeSt publishes a new dump (the index is frozen against the dump used at build time).

## Hardware prerequisites

- **Rust 1.90 or newer** (install via `rustup`, see below).
- **~3 GB free disk** for the Tantivy index (~2 GB) + side-tables (~50 MB) + headroom.
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
    --notability-whitelist data/notability-whitelist-v1.yaml
```

Typical output:

```
INFO crabfinder-indexer starting; output="./index-belgium"
INFO regions selected; regions=[Brussels, Flanders, Wallonia]
INFO === ingesting region === Brussels
...
====== indexing summary ======
   Brussels: pass1=  450ms  pass2=  18430ms  parsed=234567 emitted=233834 ...
   Flanders: pass1=  890ms  pass2=  85420ms  ...
   Wallonia: pass1=  720ms  pass2=  42100ms  ...
      TOTAL: parsed=6864321 emitted=6841382 ...
   commit: 14820ms
   wall-clock total: 162145ms (2.7 min)
   index size on disk: 2123456789 bytes (2024.6 MB / 1.98 GB)
```

The `./index-belgium/` folder now contains:

- Tantivy segment tree files (`*.idx`, `*.pos`, `*.term`, etc.).
- A `.crab-meta.json` sidecar identifying the schema version.
- A `mid-street-v1.bin` file (~300-500 KB) used by the batch median fallback.

**The BeSt dump used is frozen into the index.** When a new dump comes out, redo step 4 (replace the ZIPs) then step 5 (rebuild). This is not an incremental migration.

### Handy flags

| Flag | When to use it |
|---|---|
| `--region brussels` (or `flanders` / `wallonia`) | Tests, or you only need one region |
| `--limit 1000` | Quick smoke test (1000 addresses per region) |
| `--print-samples 5` | Print 5 parsed addresses before indexing to eyeball-check |

Without `--notability-whitelist`, everything still works but iconic toponyms (Grand-Place, Atomium…) no longer get a notability boost in the ranking. The file ships in `data/`, so might as well keep it.

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
INFO starting crabfinder-service; index="./index-belgium" port=8080
INFO index sidecar OK; schema_version=7 tokenizer="crab"
INFO spatial index built; points=6841382 elapsed_ms=3420
INFO address-count index built; ...
INFO listening at http://127.0.0.1:8080
```

Quick test from another terminal:

```bash
curl "http://127.0.0.1:8080/health"
# {"status":"ok","index_path":"./index-belgium","index_docs":6841382,"uptime_secs":12}

curl "http://127.0.0.1:8080/geocode?q=Grand-Place+1000&limit=2"
```

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

`--index`, `--port`, `--abbreviation-dict` all accept env-var equivalents, handy for systemd/Docker setups:

```bash
export CRABFINDER_INDEX=./index-belgium
export CRABFINDER_PORT=8080
export CRABFINDER_ABBREVIATION_DICT=./data/street-type-abbreviations-v1.yaml

./target/release/crabfinder-service     # no flags needed
```

Same for `crabfinder-batch` (`CRABFINDER_INDEX`, `CRABFINDER_ABBREVIATION_DICT`).

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
