# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

A Belgium-only geocoder in Rust. Indexes the federal **BeSt Address** open dataset (~6.9M addresses, Brussels + Flanders + Wallonia) into a single Tantivy index and serves forward/reverse/batch geocoding. Cargo workspace, edition 2024, Rust ≥ 1.90 (`rust-toolchain.toml`).

**BeSt Address is the only source of results.** Every address, id, coordinate and label returned (and every index document) is BeSt's. OpenStreetMap (`--places` indexer, `--pois` service) is an optional, off-by-default aid to *interpret* hamlet / landmark queries: it may only decide where to search, never be returned or stored in an address. Keep it that way, and say so in docs when touching these areas.

## Commands

```bash
cargo build --release                 # use release for anything touching a real index; debug Tantivy is too slow
cargo test --workspace                # unit + e2e tests; e2e tests build tiny in-memory/tempdir indices, no BeSt data needed
cargo test -p crabfinder-service      # one crate
cargo test -p crabfinder-service resolver::   # filter by test path/name
cargo test -p crabfinder-batch --test cli_e2e # one integration test file
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
scripts/coverage.sh                   # cargo-llvm-cov line coverage, gate 95% (thin main.rs + test-support excluded)
```

Tests share fixtures through the dev-only `crabfinder-test-support` crate (`AddressBuilder`, `fixtures::*`, `TestIndex::build / build_with_mid_street / build_with_places`, programmatic BeSt bundles in `best_bundle`). Service HTTP tests drive the real `http::router` via `tests/common::App` + `tower::ServiceExt::oneshot`; eval tests serve it on `127.0.0.1:0`. Insta snapshots need `INSTA_UPDATE=always` on first run.

Full pipeline (~3 GB disk). `scripts/refresh_best.sh` downloads BeSt into `best-<date>/`, builds `index-<date>`, validates the eval sets and swaps the `index-current` symlink. By hand:

```bash
./target/release/crabfinder-indexer --best-dir ./best-full-latest --output ./index-belgium \
    --notability-whitelist data/notability-whitelist-v1.yaml           # ~1.5 min; --region/--limit for smoke runs, --overwrite to replace an index
                                                                       # optional OSM aid: --places data/osm-places-be-v1.csv (refresh script: WITH_OSM=1)
./target/release/crabfinder-service --index ./index-belgium \
    --abbreviation-dict data/street-type-abbreviations-v1.yaml --port 8080   # optional OSM aid: --pois data/osm-pois-be-v1.csv
./target/release/crabfinder-batch --index ./index-belgium -i in.csv -o out.csv
```

The service binds **IPv4 only on 127.0.0.1** by default (`--host`): use `127.0.0.1`, not `localhost`. It doesn't accept connections until the R-tree is built (~5–6 s). Ops flags: `--request-timeout-ms`, `--batch-timeout-secs`, `--max-concurrency`, `--cors-origin`, `--no-metrics` (Prometheus at `/metrics`), `--no-autocomplete` (skip the ~0.8 s / ~80 MB `/autocomplete` name index; the route answers 503), `--jobs-dir` (+ `--jobs-max-running`, `--jobs-max-body-mb`, `--jobs-ttl-hours`, `--jobs-threads`) for the asynchronous `/batch/jobs` (503 without it), `--batch-max-body-mb`, and the off-by-default protection (Phase 11.7, `http/auth.rs`): `--api-keys keys.yaml` (`keys: [{name, key ≥16 chars, rate_per_minute?, batch?, jobs?}]`; every route but `/health` and `/metrics` needs a key; `batch`/`jobs` are opt-in rights; jobs scoped by key name), `--rate-limit-per-ip`, `--metrics-require-key`. Middleware order: `track` → `auth` → `guard` → handler. Docker: `docker compose up -d` (`refresh` one-shot runs `refresh_best.sh` with `BIN_DIR`/`PRUNE`/`REFRESH_IF_OLDER_THAN_DAYS=7` into the `data` volume, then `geocoder` serves `/data/index-current` on 127.0.0.1:8080).

## Evaluation (how ranking changes are judged)

`crabfinder-eval` is a multi-mode binary (see `docs/eval.md`):

- `--generate-eval-set --mvp --index … --output … --seed 1`: synthetic set from the index (reproducible by seed).
- `--eval-set <json|yaml> … --service http://127.0.0.1:8080 --output r.json --output-md r.md`: HTTP run against a live service. `.json` = synthetic, `.yaml` = adversarial (`eval/adversarial-belgium-v1.yaml`); flag is repeatable. `--dry-run` projects runtime from 50 queries.
- `--baseline reports/v3-baseline.json --current r.json --diff-output d.md`: diff vs. the promoted baseline. Per-query detail is in `r-detail.json.gz` (`level_rank` per query).
- `--batch-mode --index … --eval-set … --output …`: in-process resolver run (no HTTP) that writes confidence-threshold calibration (`crates/crabfinder-eval/data/confidence-thresholds-v1.json`).
- `--alternatives`: with an HTTP run, asks `/geocode?alternatives=true` and reports alternatives recall@1/@3 by variant (expected building = best answer of one of the first municipality groups; Phase 11.3).
- `--autocomplete-mode --eval-set <synthetic.json> --service … --output …`: types each canonical "street municipality" as 2/4/6/8 chars then whole into `/autocomplete`, bare then with `focus_point` at the municipality; recall@1/@10 and latency by length (Phase 11.5, `reports/phase11-5-autocomplete.md`).
- `--validate-eval-set`: checks expected buildings/addresses still exist (direct `/building/{bgid}/units` lookups) after a BeSt refresh.
- `--benchmark --geocoder label[:kind][=target] …`: the same queries through other geocoders (`adapters/`: crabfinder, bepelias (self-hosted), phacochr (R file adapter `scripts/bench/phacochr.R`), brussels, vlaanderen, wallonie; ngi (NGI-IGN national geocoder, `geosearch.ngi.be`, keyless, returns BeSt ids)). Scored by distance to the reference point (`--with-points` fills BeSt points for address-level queries); public services rate-limited (2/s) and cached in `eval/bench-cache/` (gitignored). Writes head-to-head + "queries crabfinder loses". Latest run: `reports/benchmark-2026-10-16.md`.
- Real-world set `eval/realworld-be-v1.yaml` (`kind: realworld`, 1,510 open-data addresses with published points, scored within 100 m): rebuild with `scripts/build_realworld_eval.py`; licences in `eval/realworld/SOURCES.md`. `scripts/pin_realworld_ids.py` pins Brussels ids from the Brussels geocoder.

Current sets: `eval/belgium-2026-10-15-v3.json` (synthetic, 11 variants incl. typo/box_number/high_house_number/messy_punctuation/wrong_postal; v2 = same minus wrong_postal, kept for the pre-v2 comparison) + `eval/adversarial-belgium-v1.yaml` (entries scored at a `match_level`: address/building/street/locality; `landmark` category needs `--pois`). Any change to parsing, query building, scoring, or dedup should be validated with an eval run + diff against `reports/v3-baseline.json` (default BeSt-only config; `v3-baseline-osm.json` = index with `--places` + service with `--pois`; v2 baselines in `reports/archive/v2/`; `pre-v2-baseline.json` = v1 code on the same data). Run the eval once warm before the run you keep (cold first run: p99 +5 ms). Per-query: compare `level_rank` in the `-detail.json.gz` files (1 = top-1, 0 = not found). Adversarial entries with `expected_to_fail: true` are excluded from aggregates. Old reports live in `reports/archive/`.

## Architecture

Crate dependency flow: `core` ← `parser`, `indexer` ← `service` (lib + bin) ← `batch`, `eval`. `test-support` (dev-only) depends on `core` + `indexer`. Every binary's `main.rs` is a thin wrapper; logic lives in the libraries (`indexer::cli`, `batch` lib, `eval::cli::run_with`, `service::http`).

- **crabfinder-core**: domain types (`Address`, `BestId`, `BuildingGroupId`, `MultilingualName`, `Region`, `Status`, …).
- **crabfinder-indexer**: two-pass BeSt XML → Tantivy build: pass 1 loads dictionaries (Municipality, PostalInfo, StreetName, PartOfMunicipality), pass 2 streams Addresses, joins, reprojects Lambert72 → WGS84 (`proj4rs` with an explicit IGN 7-param Helmert proj-string, see `reproject.rs`), and joins the notability whitelist into a FAST `notability_score`. `impute.rs` places BeSt's ~140k zero-position Walloon addresses (same number → interpolation → street → locality centroid; FAST `position_source`). `places.rs` matches addresses to optional OSM places for the localities file only (nothing OSM in the payload or `searchable` since v11). Emits the `mid-street-v1.bin`, `localities-v1.json` and (v12) `history-v1.json.gz` side-files; the history file holds the retired/reserved/rejected records the status filter drops (`history.rs`). Exposes `index::CrabSchema` and `register_tokenizers` so every reader uses the same field handles and `crab` analyzer.
- **crabfinder-parser**: free-text splitter for Belgian formats: postal code (`B-1050`, `1050-Ixelles`), house number (uppercased), box (`bte`/`bus`/`box`/`12/3`), ranges, numbers inside street names; ambiguous inputs get up to 3 `alternatives`. Normalization is *not* done here: it's the `crab` analyzer's job.
- **crabfinder-service**: library + Axum binary. `resolver::search(AddressQuery, SearchOptions) → CandidateSet` is the single pipeline (locality step → strict tiers Exact/Typo/Nearest/Street/TypoStreet per reading → locality-suffix readings → landmark step (`--pois`, `landmarks.rs`) → postal-code relaxation (`adjacent_commune`) → OR tiers); locality/landmark answers are the nearest BeSt address; `resolve` (top-1) serves `POST /batch/geocode`, the `crabfinder-batch` CLI and eval batch mode, `/geocode` uses `search` directly. Handlers live in `http/` (`router_with` + `RouterOptions`: deadline/concurrency guard, CORS, `/metrics`); Tantivy work runs on the blocking pool (`http::blocking`). `AppState::load` is in `state.rs`; `telemetry` holds the Prometheus series. Key modules: `query_builder` (strict phrase + AND-of-OR-forms query, fuzzy OR fallback, `tweak_score` notability), `abbreviation` (query-time `av` → `avenue` expansion), `dedup` (over-fetch then collapse by `building_group_id`), `hydrate` (stored JSON payload → `Address`), `spatial` (in-memory `rstar` R-tree rebuilt at startup for `/reverse`), `interpolate` (`/reverse?interpolate=true` house-number estimate in a gap), `mid_street` (`Confidence::MedianStreet` fallback), `validate` (`/validate` + batch `corrections` columns: input vs. the resolved BeSt address, Phase 11.2), `history` (`resolve_id`: exact id → `best_oid` → other version → history file; successor derived at lookup time, `same_number_box` → `same_number` → `same_position` ≤ 5 m; behind `/lookup` `results` and the batch `best_id` column / `Confidence::Id`, Phase 11.4), `suggest` (`/autocomplete`, Phase 11.5: in-memory fst name index built at startup from the stored docs; streets per postal code + BeSt localities, OSM places/landmarks only with `--places`/`--pois` and only leading to the nearest BeSt address; prefixes shared by > 2,000 keys answered from precomputed top lists, overall and per 0.2°×0.3° cell), `batch/` (CSV I/O incl. the row-at-a-time `OutputSink`, intra-batch dedup, rayon fan-out, `run::resolve_chunks` (5,000 rows at a time with a `keep_going` check between chunks; every batch path uses it), HTTP façade: streams chunk by chunk, keeps the guard's `Permit` and checks its `Deadline` between chunks, cuts the body off past it), `jobs` (Phase 11.6: `/batch/jobs` under `--jobs-dir`; per-job dir with `input.csv`, `state.json` checkpoint (`rows_done` + `result_bytes`) after every chunk, `result.part` → `result.csv|geojson`; resume at startup, TTL sweep, own rayon pool; HTTP side in `http/jobs.rs`). Test hooks: `RouterOptions::batch_chunk_rows`/`batch_chunk_delay`, `JobsConfig::chunk_rows`/`chunk_delay`.
- **crabfinder-batch**: standalone CLI peer of the service (not a wrapper around it); opens the index directly and calls the same resolver. Safe to run alongside a live service on the same index (mmap read-sharing).
- **crabfinder-eval**: generator, HTTP runner, metrics, Markdown reports, diffs, in-process batch mode.

Batch I/O contract: input CSV `id,street,house_number,postal_code` (or `address`, or `best_id`; delimiter sniffed, `,` or `;`), strictly 1 output row per input row with `confidence` + `fallback_reason`.

### Index schema invariants

- Schema version lives in `crabfinder-indexer/src/meta.rs::SCHEMA_VERSION` (currently **12**) with a changelog in the doc comment. The `.crab-meta.json` sidecar in the index dir is checked by every reader (`meta::check_sidecar`); a mismatch is a hard error. **Bump the version (and add a changelog line) whenever a change makes existing indices or side-files incompatible**, then rebuild the index.
- A single multi-valued `searchable` field holds FR/NL/DE names together (per-language fields were removed in v3 because they broke cross-language queries like "Anvers Grote Markt"). Fieldnorms are disabled on it.
- The `crab` analyzer is `SimpleTokenizer → LowerCaser → AsciiFoldingFilter`; hyphens are hard separators. Query-side tokens must go through `query_builder::analyze` to stay in lockstep with the indexer.
- The autocomplete / edge-ngram field (v5) was deliberately removed: don't reintroduce it without discussion (see `reports/archive/phase7a-removed/`). `/autocomplete` is served by the in-memory `suggest.rs` index instead (no schema change; `/geocode` never reads it).

## Data & docs

- `data/notability-whitelist-v1.yaml` (index-time, `building_group_id → multiplier`) and `data/street-type-abbreviations-v1.yaml` (query-time) are optional inputs; without them the engine runs with no boost / identity expansion. Optional OSM aids, off by default: `data/osm-places-be-v1.csv` (index-time `--places`, ODbL, `scripts/build_osm_places.py`) and `data/osm-pois-be-v1.csv` (service `--pois`, `scripts/build_osm_pois.py`). `data/` is gitignored: curated files need `git add -f`. `scripts/build_whitelist.py` regenerates the whitelist from the adversarial set.
- `scripts/*.py` are one-off helpers that mostly hit a running service (`--base http://127.0.0.1:8080`).
- Technical references: `docs/architecture.md`, `docs/api.md`, `docs/eval.md`, `docs/design-batch-geocoding.md`, `docs/v1-batch-plan.md`. User guides exist in both `docs/en/` and `docs/fr/`: keep them in sync when changing user-facing behavior.
- Code comments reference planning stages as "Phase N" / "Lot A–G" and design-doc sections (e.g. `design-batch-geocoding §5.5`); follow that convention when extending those areas.
