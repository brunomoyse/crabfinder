# Evaluation

`crabfinder-eval` produces synthetic and adversarial eval sets, runs them
against a live `crabfinder-service`, and emits comparable reports.

## Eval sets

Two on-disk formats, dispatched by file extension:

| Extension | Type | Source |
|---|---|---|
| `.json` | **Synthetic** — generated from the live index | `--generate-eval-set --mvp` |
| `.yaml` / `.yml` | **Adversarial** — hand-edited | `eval/adversarial-belgium-v1.yaml` |

You can pass multiple `--eval-set` flags on the runner; the loader
auto-detects each file.

### Synthetic stratification (MVP profile)

Per-region random sample:

| Region | Bgids drawn |
|---|---|
| Brussels | 100 |
| Flanders | 250 |
| Wallonia | 150 |
| Boosted cross-language | 200 |
| Boosted high house number (≥ 1000) | 100 |

Each random bgid generates 6–9 query variants:

- `canonical` — `Street HN, Postal Muni`
- `minimal` — `Street HN`
- `with_postal` — `Street HN Postal`
- `casing` — lowercased canonical
- `cross_language` — emitted when the firing policy detects a usable
  alias (e.g. FR municipality + NL street name)
- `abbreviated` — emitted when the street type matches the abbreviation
  map (`Avenue Louise` → `Av. Louise`)
- `typo` — one edit (substitution, deletion, transposition or insertion)
  on the longest street word of 5+ characters, seeded per address
- `messy_punctuation` — commas between every part and a `B-` postal
  prefix (`Franciskanenlaan, 10, B-1150, Sint-Pieters-Woluwe`)
- `box_number` — emitted when the address has a box, with the box keyword
  of the street's language (`Floréallaan 154 bus 1, 1180 Ukkel`)

The boosted pools emit only their own variant (`cross_language`,
`high_house_number`). The current set is
`eval/belgium-2026-10-15-v2.json` (seed 1, 3,742 queries).

Use `--scale N` to multiply all per-region targets uniformly (e.g.
`--scale 4.0` gives ~2800 bgids → ~11k queries). Useful for tightening
confidence intervals.

### Adversarial categories

Hand-curated entries in `eval/adversarial-belgium-v1.yaml`:

| Category | What it probes |
|---|---|
| `hamlet` | Hamlets whose name shadows famous municipalities |
| `notability` | Iconic toponyms (Grand-Place, Atomium, …) |
| `de_zone` | DE-speaking communes (Eupen, St. Vith, …) |
| `facilites` | Brussels communes with FR/NL facility names |
| `cross_language` | FR query against NL-canonical doc and vice versa |
| `special_chars` | Apostrophes, hyphens, mixed casing |
| `known_failure` | Entries the system fails on by design — tracked separately, excluded from aggregates |

Each entry is scored at a `match_level` (default `address`):

| `match_level` | Correct when |
|---|---|
| `address` | The top feature is `expected_best_id` (else, when none is pinned, `expected_building_group_id`) |
| `building` | Any unit of an expected building — landmarks that can't name a unit |
| `street` | Any building on an expected building's street — queries without a house number |
| `locality` | Any building within `within_m` (default 1500 m) of `expected_point` `[lat, lon]` — bare place names |

The report shows the levels scored next to each category, e.g.
`hamlet (locality, street)`.

## Generating an eval set

```bash
./target/release/crabfinder-eval --generate-eval-set --mvp \
  --index ./index-belgium \
  --output eval/belgium-2026-10-15-v2.json \
  --seed 1 \
  --scale 1.0 \
  --dump-date 2026-10-15
```

Reproducible — same `--seed` + same index emits the same eval set.

## Running an eval

```bash
./target/release/crabfinder-eval \
  --eval-set eval/belgium-2026-10-15-v2.json \
  --eval-set eval/adversarial-belgium-v1.yaml \
  --service http://127.0.0.1:8080 \
  --parallel 8 \
  --output reports/post-foo.json \
  --output-md reports/post-foo.md
```

Besides the JSON and Markdown, a full run writes
`reports/post-foo-detail.json.gz`: one row per query with the top-1
building, best id, match type, latency, error and `level_rank` (the
1-based rank of the first correct feature at the query's match level,
0 = missed). Compare two detail files to find per-query regressions.

`--dry-run` runs the first 50 queries and projects the full-run wall
time, useful for sanity-checking a fresh eval set before the full run.

## Diffing two reports

```bash
./target/release/crabfinder-eval \
  --baseline reports/v2-baseline.json \
  --current  reports/post-foo.json \
  --diff-output reports/diff-foo.md
```

The diff focuses on Tier A overall metrics, per-region, per-variant,
adversarial categories, the cross-language §3.2 contract and
known-failure state changes. For per-query regressions, compare the two
`-detail.json.gz` files.

## Validating an eval set after a BeSt refresh

```bash
./target/release/crabfinder-eval --validate-eval-set \
  --eval-set eval/belgium-2026-10-15-v2.json \
  --eval-set eval/adversarial-belgium-v1.yaml \
  --service http://127.0.0.1:8080 \
  --max-missing-pct 5.0
```

Looks every expected building up directly
(`GET /building/{building_group_id}/units`) rather than searching for
it, so ranking changes can't mask a stale id. An entry passes when one
of its expected buildings exists and, for `address`-level entries that
pin a `best_id`, that address is one of the building's units.
`locality`-level entries and entries with nothing expected pass. Aborts
if more than `--max-missing-pct` of entries fail — the signal that the
eval set is stale and needs regenerating. `scripts/refresh_best.sh` runs
this on every fresh index before switching `index-current`.

## Metrics

### Tier A (aggregates exclude `known_failure`)

| Metric | Definition |
|---|---|
| `recall@K` | Fraction of queries whose `expected_best_id` is among the top-K returned features |
| `building_group_recall@K` | Same, but against `expected_building_group_id` |
| `MRR` | Mean of `1 / rank` (0 if not found in returned set) |
| `strict_match_rate` | Fraction where the response's `match_type` is `strict` |

### Tier B (cross-language contract)

| Metric | Threshold |
|---|---|
| `cross_language_recall@1` random pool | ≥ 0.95 |
| `cross_language_recall@1` boosted pool | ≥ 0.95 |

### Latency

p50 / p95 / p99 / mean / max are reported in ms, measured HTTP-loopback.
Loopback adds roughly 0.5–1 ms/query which is non-trivial on sub-3 ms
strict queries but negligible on the fuzzy tail.

### `known_failure` tracking

Adversarial entries marked `expected_to_fail: true` are *not* counted
in any aggregate. They surface in a dedicated panel listing the state
of each entry (`still_failing` / `now_passing` / `failing_differently`)
so you can see at a glance whether a documented failure has changed
behavior.

## Baseline

`reports/v2-baseline.json` is the current promoted baseline: BeSt dump of
2026-10-15, schema v9, `eval/belgium-2026-10-15-v2.json` +
`eval/adversarial-belgium-v1.yaml`, OSM places, full notability
whitelist, full abbreviation dict. `reports/pre-v2-baseline.json` is the
v1 code on the same data — the comparison point for the v2 work
(`reports/v2-vs-pre-v2.md`). `reports/mvp-v1-baseline.json` is the
original MVP baseline on the 2026-05-19 dump; per-phase v2 diffs are in
`reports/archive/v2-phases/`.

To promote a new baseline:

```bash
# After validating the new report is at least as good as the baseline,
mv reports/v2-baseline.json reports/archive/$(date -I)-baseline.json
cp reports/post-newfeature.json reports/v2-baseline.json
```
