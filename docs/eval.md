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

Each random bgid generates 4–6 query variants:

- `canonical` — `Street HN, Postal Muni`
- `minimal` — `Street HN`
- `with_postal` — `Street HN Postal`
- `casing` — lowercased canonical
- `cross_language` — emitted when the firing policy detects a usable
  alias (e.g. FR municipality + NL street name)
- `abbreviated` — emitted when the street type matches the abbreviation
  map (`Avenue Louise` → `Av. Louise`)

The boosted pool emits the `cross_language` variant only.

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

## Generating an eval set

```bash
./target/release/crabfinder-eval --generate-eval-set --mvp \
  --index ./index-belgium \
  --output eval/belgium-2026-05-21-mvp-v1.json \
  --seed 1 \
  --scale 1.0 \
  --dump-date 2026-05-21
```

Reproducible — same `--seed` + same index emits the same eval set.

## Running an eval

```bash
./target/release/crabfinder-eval \
  --eval-set eval/belgium-2026-05-21-mvp-v1.json \
  --eval-set eval/adversarial-belgium-v1.yaml \
  --service http://127.0.0.1:8080 \
  --parallel 8 \
  --output reports/post-foo.json \
  --output-md reports/post-foo.md
```

`--dry-run` runs the first 50 queries and projects the full-run wall
time, useful for sanity-checking a fresh eval set before the full run.

## Diffing two reports

```bash
./target/release/crabfinder-eval \
  --baseline reports/mvp-v1-baseline.json \
  --current  reports/post-foo.json \
  --diff-output reports/diff-foo.md
```

The diff focuses on Tier A overall metrics, per-region, per-variant,
adversarial categories, and the cross-language §3.2 contract. Per-query
regression list is intentionally out of scope at MVP.

## Validating an eval set after a BeSt refresh

```bash
./target/release/crabfinder-eval --validate-eval-set \
  --eval-set eval/belgium-2026-05-21-mvp-v1.json \
  --service http://127.0.0.1:8080 \
  --max-missing-pct 5.0
```

Verifies that every `expected_*` id in the set still resolves in the
top-50 against the running service. Aborts if more than
`--max-missing-pct` of entries don't resolve — the signal that the
eval set is stale and needs regenerating.

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

`reports/mvp-v1-baseline.json` is the current promoted baseline. Generated
with seed 1, schema v6, full notability whitelist, full abbreviation dict.

To promote a new baseline:

```bash
# After validating the new report is at least as good as the baseline,
mv reports/mvp-v1-baseline.json reports/archive/$(date -I)-baseline.json
cp reports/post-newfeature.json reports/mvp-v1-baseline.json
```
