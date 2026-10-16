# Phase 11.5: autocomplete

`GET /autocomplete` on an in-memory name index built at startup
(`suggest.rs`), measured with `crabfinder-eval --autocomplete-mode` on the
synthetic v3 set, BeSt 2026-10-15, schema v12, BeSt-only service. No index
field and no schema change: the v5 edge-ngram field stays removed.

**The test.** Each of the 500 `canonical` queries becomes "street
municipality" (number and postal code dropped). It is sent as its first
2, 4, 6 and 8 characters, then whole: 2,500 requests, `limit=10`. A
request scores when the expected street is among the street suggestions.
Every text is sent twice: bare, then with `focus_point` at the
municipality, as a client passing its map centre would. Summary JSON:
`reports/phase11-5-autocomplete.json`.

## Recall

| typed | bare recall@1 | bare recall@10 | focus recall@1 | focus recall@10 |
|---|---|---|---|---|
| 2 | 0.000 | 0.026 | 0.014 | 0.306 |
| 4 | 0.114 | 0.422 | 0.392 | 0.760 |
| 6 | 0.294 | 0.610 | 0.618 | 0.882 |
| 8 | 0.428 | 0.742 | 0.772 | 0.946 |
| full | 0.998 | 1.000 | 1.000 | 1.000 |

By region (bare, all lengths, recall@10): Brussels 0.746, Flanders 0.614,
Wallonia 0.345.

**Targets.** The plan's targets were recall@10 ≥ 0.85 at 8 characters and
≥ 0.50 at 4, which the v5 attempt missed. They are met with `focus_point`
(0.946 and 0.760). They are not met bare (0.742 and 0.422).

**Why bare stays lower.** 91 of the 129 bare misses at 8 characters
(70%) are still inside a generic street-type word: "Rue des " (10
misses), "Rue de l" (8), "Chemin d" (6), "Rue du P", "Place de", "Avenue
d", "Chaussée", "Rue Jean". Thousands of Belgian streets share these
prefixes, and with no position the biggest ones come first. That is
ambiguity, not a matching bug. The same text with the municipality as
focus finds the street, which is why the docs tell clients to pass
`focus_point`. Wallonia is lowest because more of its names start with
"Rue de", "Chemin de" or "Route de".

## Against the v5 edge-ngram attempt

The archived v5 run (`reports/archive/phase7a-removed/post-7a.md`) scored
the expected *building* from street-name prefixes, a stricter test, on
another set. The two runs are not strictly comparable.

| | v5 (edge-ngram field) | 11.5 bare | 11.5 with focus |
|---|---|---|---|
| recall@1 at 8 chars | 0.332 | 0.428 | 0.772 |
| recall@5 / @10 at 8 chars | 0.518 (@5) | 0.742 (@10) | 0.946 (@10) |
| p99 latency | 71.3 ms | 1.6 ms | 1.6 ms |
| `/geocode` p99 | 9 → 63 ms | unchanged | unchanged |
| index | new field, reindex | none | none |

## Latency

Client-side, `--parallel 1`: p50 0.4 ms, p95 1.1 ms, p99 1.6 ms overall,
bare and focused alike. The slowest are 2-character prefixes (p99
2.2 ms). Whole names take 0.4 ms. With `--parallel 8` on a quiet machine,
overall p99 was 2.6 ms bare and 3.4 ms focused. Under a load average of 10
on 8 cores, it reached 4.4 to 7.8 ms, which is queueing, not work. The
budget was p99 < 5 ms.

Short prefixes are fast because the 63 prefixes shared by more than 2,000
keys ("r", "rue ", "st", …) answer from precomputed best lists. Each has
the top 64 overall and the top 32 per 0.2° × 0.3° cell; the 3 × 3 cells
around the focus point are read. Before those lists, the first run's p99
was 15.4 ms.

## Cost

- **Startup build:** 149,054 entries (145,989 street and postal-code
  pairs, plus BeSt localities), 166,123 fst keys, 63 common prefixes,
  0.77 s.
- **Memory:** physical footprint 1,178 MB against 1,100 MB with
  `--no-autocomplete`, so about +78 MB. The budget was 200 MB.
- **`/geocode` unchanged:** the full eval run on a service with
  autocomplete and on one with `--no-autocomplete` gave recall@1 0.9654
  on both, with 0 of 4,368 per-query rank differences. p95 was 7.7 to
  9.3 ms on both, within noise. `/geocode` never reads the name index.
- The batch CLI and eval batch mode don't build it.

## What it returns

Streets (one per street and postal code), BeSt localities and, after a
house number, the street's buildings: every label, id and position is
BeSt's. OSM places and landmarks are suggested only with `--places` /
`--pois`, both off by default. They lead to the nearest BeSt address and
are flagged `osm_hint: true`. The runs above use neither.
