# Evaluation

`crabfinder-eval` produces synthetic and adversarial eval sets, runs them
against a live `crabfinder-service`, and emits comparable reports. It can
also run the same queries through other Belgian geocoders and compare them
(`--benchmark`).

## Eval sets

Three on-disk formats, dispatched by file extension and, for YAML, a
top-level `kind`:

| Extension | Type | Source |
|---|---|---|
| `.json` | **Synthetic** — generated from the live index | `--generate-eval-set --mvp` |
| `.yaml` / `.yml` | **Adversarial** — hand-edited | `eval/adversarial-belgium-v1.yaml` |
| `.yaml` with `kind: realworld` | **Real-world** — addresses from public open-data lists | `eval/realworld-be-v1.yaml` |

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

- `wrong_postal` — `Street HN Postal` with the postal code whose centre
  is nearest to the address among the *other* codes (from the index's
  localities file): the neighbouring-commune mistake. Emitted for every
  random sample, after all other queries.

The boosted pools emit only their own variant (`cross_language`,
`high_house_number`). The current set is
`eval/belgium-2026-10-15-v3.json` (seed 1, 4,244 queries);
`eval/belgium-2026-10-15-v2.json` (3,742, no `wrong_postal`, generated
on an earlier build of the same dump) is kept for the pre-v2
comparison.

Use `--scale N` to multiply all per-region targets uniformly (e.g.
`--scale 4.0` gives ~2800 bgids → ~11k queries). Useful for tightening
confidence intervals.

### Adversarial categories

Hand-curated entries in `eval/adversarial-belgium-v1.yaml`:

| Category | What it probes |
|---|---|
| `hamlet` | Hamlets whose name shadows famous municipalities; OSM-only hamlet names need the optional `--places` aid |
| `notability` | Iconic toponyms (Grand-Place, Atomium, …) |
| `landmark` | A landmark named without a street (Manneken Pis, Gare du Midi, Gravensteen Gent), scored at `locality` level within 600 m of its OSM position; needs the optional `--pois` aid |
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

### Real-world set

`eval/realworld-be-v1.yaml` holds 1,510 addresses taken from public lists
(schools, post offices, pharmacies, shops and care facilities in the three
regions). Each entry has the address as published, split into fields and
joined into a free-text `query`. Its `expected_point` is the publisher's
own coordinate. Real users type addresses like these: uppercase, typos,
French names for Flemish towns, `Chaussée De Haecht`.

An entry is scored at `locality` level: a hit is any result within
`within_m` (100 m by default) of the published point. Published points are
often an entrance or a parcel centroid rather than the BeSt address point.
An entry that pins a hand-checked `expected_best_id` is scored at
`address` level instead. Results are reported by dataset
(`by_realworld_source`), and the distance metrics apply.

200 Brussels entries pin a BeSt address. The id comes from the Brussels
regional geocoder (Paradigm, which publishes BeSt Brussels), so crabfinder
is never graded against its own answers. An entry is pinned only when that
geocoder's top-1 has the entry's exact house number and lies within 100 m
of the published point. `scripts/pin_realworld_ids.py` adds the pins from
a benchmark run's response cache and detail file, without sending any new
requests.

`scripts/build_realworld_eval.py` rebuilds the set. It downloads each
source once into `eval/realworld/sources/` (gitignored), dedupes, takes a
seeded sample stratified by postal area, and writes the YAML plus
[`eval/realworld/SOURCES.md`](../eval/realworld/SOURCES.md), which lists
each dataset's publisher, licence and attribution.

## Generating an eval set

```bash
./target/release/crabfinder-eval --generate-eval-set --mvp \
  --index ./index-belgium \
  --output eval/belgium-2026-10-15-v3.json \
  --seed 1 \
  --scale 1.0 \
  --dump-date 2026-10-15
```

Reproducible — same `--seed` + same index emits the same eval set.

## Running an eval

```bash
./target/release/crabfinder-eval \
  --eval-set eval/belgium-2026-10-15-v3.json \
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

## Benchmarking other geocoders

```bash
./target/release/crabfinder-eval --benchmark \
  --eval-set eval/realworld-be-v1.yaml \
  --geocoder crabfinder=http://127.0.0.1:8080 \
  --geocoder crabfinder-osm:crabfinder=http://127.0.0.1:8081 \
  --geocoder brussels --geocoder vlaanderen --geocoder wallonie \
  --geocoder bepelias=http://127.0.0.1:4001/REST/bepelias/v1 \
  --geocoder phacochr \
  --output reports/benchmark-2026-10-16.json \
  --output-md reports/benchmark-2026-10-16.md
```

`--geocoder label[:kind][=target]` adds a geocoder; the kind defaults to
the label. The target is a base URL, or the command line for `phacochr`.

| Kind | What | Coverage | BeSt ids |
|---|---|---|---|
| `crabfinder` | this service, `/geocode` | Belgium | yes |
| `bepelias` | SmalsResearch BePelias, self-hosted from its Docker compose | Belgium | yes |
| `phacochr` | the PhacochR R package via `scripts/bench/phacochr.R` (a batch, not a service) | Belgium | no |
| `brussels` | Brussels regional geocoder (Paradigm / UrbIS, BeSt model) | Brussels | yes |
| `vlaanderen` | Digitaal Vlaanderen Geolocation API | Flanders + Brussels | no |
| `wallonie` | SPW `geocodeWS` (ICAR) | Wallonia | no |

The NGI national geocoder has no public endpoint yet: `search.geo.be`
does not resolve (checked 2026-10-16), and `--geocoder ngi` says so.

Each geocoder is only asked the queries it covers. A regional service gets
its own region, and the region of a synthetic or adversarial query is read
off its expected BeSt ids. Compare geocoders within a region row.

Every geocoder is scored on distance from its top-1 point to the query's
reference point:

- the share of queries answered within 25, 100 and 250 m;
- the median and p90 error;
- the found rate (the share answered with a point at all).

Recall is reported only for geocoders that return BeSt ids. Without
`--with-points`, only real-world entries and locality-level entries have a
reference point. `--with-points` looks up the BeSt point of every
address-level query's expected address on `--service` (by default the
crabfinder at `:8080`), which also scores the synthetic set by distance.

The report contains:

- per-geocoder metrics, overall and by slice: set, real-world dataset,
  region;
- head-to-head counts against the first `crabfinder` geocoder, with a hit
  being a top-1 within 100 m;
- the queries crabfinder misses that another geocoder gets right. This
  list feeds the ranking backlog.

`-detail.json.gz` holds one row per (geocoder, query).

Courtesy and reproducibility:

- Public services are limited to 2 requests/s by default (`--rate label=N`
  overrides it).
- Transport errors and 5xx responses are retried with backoff.
- Responses are cached in `eval/bench-cache/<label>/` (gitignored), so a
  rerun asks nobody anything. `--no-bench-cache` disables this.
- crabfinder itself is never cached.
- PhacochR's output is cached by a hash of its input.

Nothing a competitor returns is ever used by crabfinder; it is only
compared.

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
  --eval-set eval/belgium-2026-10-15-v3.json \
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

### Distance (every geocoder)

| Metric | Definition |
|---|---|
| `found_rate` | Share of queries with a reference point answered with a point |
| `within_25m` / `within_100m` / `within_250m` | Share of those queries whose top-1 lies that close (no answer = miss) |
| `median_error_m` / `p90_error_m` | Over the answered ones |

The reference point is a real-world entry's published point, a locality
entry's place, or (with `--with-points`) the BeSt point of the expected
address. Reports break it down in `distance_by`: `synthetic`,
`adversarial`, `realworld`, `realworld:<dataset>`, `region:<region>`.

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

`reports/v2-baseline.json` is the current promoted baseline, in the
default BeSt-only configuration: BeSt dump of 2026-10-15, schema v11,
`eval/belgium-2026-10-15-v3.json` + `eval/adversarial-belgium-v1.yaml`,
full notability whitelist, full abbreviation dict, no OpenStreetMap aid.
`reports/v2-baseline-osm.json` is the same with both optional OSM aids
(index built with `--places`, service with `--pois`);
`reports/v2-osm-aids.md` diffs the two (hamlets 0.833 → 1.000,
landmarks 0.269 → 1.000). Evaluate a change in the configuration it
affects — BeSt-only by default, against the OSM baseline when touching
the locality or landmark steps. `reports/pre-v2-baseline.json` is the
v1 code on the same data and the v2 set — the comparison point for the
v2 work (`reports/v2-vs-pre-v2.md`, on the shared v2 set). `reports/mvp-v1-baseline.json` is the
original MVP baseline on the 2026-05-19 dump; per-phase v2 diffs are in
`reports/archive/v2-phases/`.

To promote a new baseline:

```bash
# After validating the new report is at least as good as the baseline,
mv reports/v2-baseline.json reports/archive/$(date -I)-baseline.json
cp reports/post-newfeature.json reports/v2-baseline.json
```
