# 6. FAQ and troubleshooting

Answers to the most common questions and errors.

## Service startup errors

### "index at … has schema_version=X, but this binary expects schema_version=Y"

You built the index with an older `crabfinder-indexer`, or you updated the code without rebuilding the index. Fix: rebuild the index with the current binary.

```bash
./target/release/crabfinder-indexer \
    --best-dir ./best-full-latest \
    --output ./index-belgium \
    --notability-whitelist data/notability-whitelist-v1.yaml
```

The operation overwrites the existing index. Expect ~2-3 min on a full dump.

### "index at … is missing .crab-meta.json"

The directory has no version sidecar. Either indexing aborted before the end, or this isn't a crabfinder index directory. Check it has Tantivy files (`*.idx`, `*.term`…) and a `.crab-meta.json`. Otherwise, redo step 5 of installation.

### "opening Tantivy index at ./index-belgium: No such file or directory"

The path passed to `--index` doesn't exist. `ls ./index-belgium` must show files. If it's a relative path, make sure you're running the command from the repo root.

### "Address already in use (os error 48)" at startup

Port (8080 by default) is already taken. Either another `crabfinder-service` is running:

```bash
pgrep -fa crabfinder-service
# kill if needed:
kill <PID>
```

Or another service holds the port:

```bash
lsof -i :8080
```

Switch port via `--port 8081` or `CRABFINDER_PORT=8081`.

### "connect: connection refused" on localhost but not on 127.0.0.1

The service binds IPv4 only. On some systems `localhost` resolves to IPv6 (`::1`) first. Always use `127.0.0.1` in your curls.

## Indexing errors

### "no ZIP found for region Brussels (Entity::Address) in ./best-full-latest"

An expected BeSt archive is missing. Redo step 4 of installation and ensure all ~10 archives are in `--best-dir` (Address × 3 regions, Municipality × 3, PostalInfo × 3, StreetName × 3, PartOfMunicipality for Wallonia). Do not unzip: the indexer reads ZIPs directly.

### "status-skip share drifted from the file 11 §9.3 baseline"

Informational warning only, **not an error**. crabfinder detected that the share of dropped addresses (non-current status) in the current BeSt dump diverges from the reference baselines. This happens when BeSt changes its status policy between dumps. Indexing completes normally. Investigate if the drift exceeds 2 percentage points (typically a BeSt-side decision to acknowledge by updating the baseline).

### Indexing takes much longer than 2-3 minutes

Likely reasons:
- **Didn't compile with `--release`**. Debug binary is 10× slower. `cargo build --release` then `./target/release/crabfinder-indexer`.
- **Saturated or slow disk**. Tantivy writes are I/O-heavy. On HDD or USB stick, plan 10-15 min.
- **Low RAM**. Pass 1 holds dictionaries in memory (~1 GB). On a 2 GB VM, swap kicks in and slows down massively.

## Search: surprising results

### "Avenu Louise" (typo) finds nothing

crabfinder has a fuzzy fallback but it's not omnipotent. The strict pass requires **every query token** to match at least one indexed token. If the typo is on a short token ("Avenu" instead of "Avenue"), the analyzer doesn't bridge them — no stemming.

Workarounds:
- Query-side: drop the typoed token, let the strict pass find via the others (`Louise 32 1050` matches).
- Spell-check upstream if you geocode in batch.
- The `fuzzy_street` score works well on minor typos (`Etterbeek` vs `Étterbeek`, handled by AsciiFoldingFilter).

### Searching "Avenue Louise" without a PC returns noise

Without `postal_code`, crabfinder can't disambiguate homonymous streets. "Avenue Louise" exists in Brussels, but also in Mons, Charleroi, Tournai, etc. Sorting is by BM25 + notability, so the Brussels Avenue Louise usually surfaces first (notability boost) — but it's a heuristic, not a guarantee.

Workarounds:
- Always supply `postal_code` when you have it.
- Use `region=brussels` when context allows.
- Use `focus_point=lat,lng` to bias toward an area.

### Abbreviations like "Av. Louise" aren't recognized

You forgot the `--abbreviation-dict` flag when starting the service or the CLI. Without it, abbreviations pass straight to the index and don't match the canonical form ("avenue").

Fix:

```bash
./target/release/crabfinder-service \
    --index ./index-belgium \
    --abbreviation-dict data/street-type-abbreviations-v1.yaml \
    ...
```

(Same for `crabfinder-batch`.)

### Top-1 isn't the correct answer

Rare but happens. Typical causes:
- Ambiguous query ("Rue de la Gare") → add PC or region.
- A local toponym not boosted in notability (the whitelist doesn't cover everything). You can add an entry to `data/notability-whitelist-v1.yaml` and re-index.
- Typo: the fuzzy pass can surface a wrong candidate.

Inspect `parsed.street_tokens`, `parsed.house_number`, `parsed.postal_code` in the `/geocode` response: if the parser carved the query wrong, everything downstream is off. Bump `limit` to check whether the correct answer is in the top-3 or top-5.

## Batch: performance and quality

### How many rows per second can I push?

Ballpark on a dev laptop (M1 8-core or x86_64 8-core, SSD):
- CLI in-process: ~1500 rows/s per core, so ~10-12k rows/s on 8 cores.
- HTTP: ~5-6k rows/s (HTTP serialize/deserialize overhead).

For 100k rows: ~10 s in CLI, ~20 s in HTTP. For 1M rows: ~2-3 min in CLI.

### My `not_found` rate is high

Check, in order:
1. **Encoding**: a `windows-1252` CSV ingested as UTF-8 corrupts accents → strict pass misses, fuzzy too. Convert with `iconv`.
2. **Delimiter**: if sniffing got it wrong (rare), force with `--delimiter ';'`.
3. **Missing postal code**: without PC, many homonymous streets come back in undefined order, and some rows may drop to `not_found` when BM25 finds nothing convincing.
4. **Non-Belgian addresses**: crabfinder only covers Belgium. Foreign addresses come out as `not_found`.

### `median_street` is very frequent in my output

You have many rows without `house_number` — expected. On the Tier A eval, ~38% of rows fall into `median_street`. If that's unintended (your data has numbers crabfinder doesn't recognize), inspect a few rows:

```bash
awk -F, '$11=="house_number_out_of_range" {print}' geocoded.csv | head -20
```

Typical cases: box numbers merged into the house_number field ("32 bte 1") instead of separated, unusual suffixes, numbers exceeding 4 digits.

### Turn off the median fallback

If you'd rather have rows without an exact number come out as `not_found` instead of `median_street`:

```bash
# CLI
./target/release/crabfinder-batch ... --no-mid-street ...

# HTTP
curl -X POST .../batch/geocode -H "Content-Type: application/json" \
     -d '{"addresses":[...],"options":{"mid_street_fallback":false}}'
```

## Multi-process and index sharing

### Can the service and the CLI run together against the same index?

**Yes.** Tantivy uses `MmapDirectory` for reads, which supports cross-process sharing. You can have `crabfinder-service` running in the background serving interactive `/geocode` *and* `crabfinder-batch` running alongside processing a 100k-row CSV, both against `./index-belgium`. No lock to manage.

### Can I run several `crabfinder-batch` instances in parallel?

Yes, same reason. Useful if you want to parallelize across machines sharing a common filesystem (NFS).

### Can I re-index while the service is running?

**No, avoid.** The indexer opens the index for exclusive writing; a service reading in parallel may serve inconsistent results while new segments are being committed. Clean workflow:

```bash
# 1. Build the new index in a different folder
./target/release/crabfinder-indexer \
    --best-dir ./best-full-latest \
    --output ./index-belgium-new \
    --notability-whitelist data/notability-whitelist-v1.yaml

# 2. Stop the service, swap, restart
kill $(cat crabfinder.pid)
mv ./index-belgium ./index-belgium-old
mv ./index-belgium-new ./index-belgium
./target/release/crabfinder-service ... &

# 3. Optional: drop the old
rm -rf ./index-belgium-old
```

## Data refresh

### How do I know when a new BeSt dump is out?

No automatic notification. BeSt publishes on [opendata.bosa.be](https://opendata.bosa.be/) typically monthly. Your index's `.crab-meta.json` sidecar carries `built_at_unix_secs` telling you when you last indexed.

### How do I know which BeSt dump is in my index?

The sidecar stores an **indexing** timestamp, not the dump source date. If you need to trace this, keep a naming convention for the BeSt archives and/or note the date in your refresh procedure.

### Incremental refresh?

No. crabfinder rebuilds the index in full at each refresh. ~2-3 min on a full dump. No delta.

## Memory and disk footprint

### How much RAM does the service eat?

At startup: peak ~3-4 GB (spatial R-tree build + address-count). In steady state: ~500 MB to 1 GB (mmap of the index — not really "consumed", OS page cache). The 6.8M-point R-tree adds ~400 MB of effective heap.

### How much RAM does the batch CLI eat?

Variable with `--parallel N` and the input CSV size held in RAM. At 8 cores and 100k rows: peak ~500 MB. At 1M rows: peak ~2-3 GB. If RAM is tight, use `--parallel 4` or `--parallel 2`.

### How much disk?

Tantivy index: ~2 GB for full Belgium. Side tables (mid_street): < 1 MB. Sidecar: a few bytes. All in: count 2.5 GB generously.

## Logs and observability

### Enable verbose logs

Logs are controlled via `RUST_LOG`. Levels: `error`, `warn`, `info` (default), `debug`, `trace`.

```bash
RUST_LOG=debug ./target/release/crabfinder-service ...
RUST_LOG=crabfinder_service=debug,tantivy=warn ./target/release/crabfinder-service ...
```

For the batch CLI:

```bash
RUST_LOG=debug ./target/release/crabfinder-batch ... 2> batch.log
```

(The CLI sends logs to `stderr` to keep the CSV output clean.)

### JSON format for centralized ingestion

The service uses `tracing-subscriber` with a default text format. To get JSON:

```bash
RUST_LOG=info ./target/release/crabfinder-service ... 2>&1 \
  | jq -R '. as $line | try fromjson catch {raw: $line}'
```

(No explicit flag at the moment; JSON format can be added via env var if needed — open an issue.)

## You didn't find your answer

Check:
- That you've read [`02-installation.md`](02-installation.md) if it's a setup problem.
- That your index is `schema_version=7` (`cat ./index-belgium/.crab-meta.json`).
- That the binaries are release builds (`./target/release/...`, not `./target/debug/...`).

Then capture:
- The version (`./target/release/crabfinder-service --version`).
- The exact command run.
- The full error message.
- The sidecar contents (`cat ./index-belgium/.crab-meta.json`).

And report on the repo or ask a developer on the team.
