# 5. Understanding results

Read this before acting on a geocoded CSV or making an automated decision off a `/geocode` response. This section explains what `confidence`, `fallback_reason`, `house_number_delta`, `position_source`, `ambiguous` and `score` mean, and how to use them for sorting or filtering. The vocabulary is the same in batch (CSV / GeoJSON columns) and online (`/geocode` feature fields).

## The golden rule

**Every output row has a coordinate *and* a confidence level**, except `not_found` which has neither. Quality varies. You decide what's acceptable for your case.

Typical acceptance thresholds:

| Use | Acceptable | Inspect | Reject |
|---|---|---|---|
| Map with per-building markers | `exact` | `fuzzy_street`, `nearest_*` | `median_street`, `locality`, `not_found` |
| Aggregation by neighbourhood / municipality | `exact`, `nearest_*`, `median_street`, `fuzzy_street` | `locality` | `not_found` |
| Spatial join with polygons (density, sector) | everything except `not_found` | `locality` | `not_found` |
| Billing address | `exact` only | everything else | everything else |

Add two checks to any of these: rows with `ambiguous=true` deserve a look whatever their confidence, and `position_source` tells you whether an `exact` row's point is the building itself (see [below](#where-the-coordinates-come-from-position_source)).

## The `confidence` taxonomy

Eight values, ordered from strongest to weakest.

| `confidence` | Spatial precision | In short |
|---|---|---|
| `exact` | building | The requested number exists on that street. |
| `nearest_same_side` | building (a neighbour) | Number absent; nearest number on the same side of the street. |
| `nearest_any_side` | building (a neighbour) | Number absent; that side has no number close enough, nearest on the other side. |
| `median_street` | street | No number, or a number too far from any existing one. |
| `fuzzy_street` | building or street | Street name misspelled or only partly matched. |
| `locality` | village / municipality | The query only names a place. |
| `adjacent_commune` | — | Reserved, never emitted yet. |
| `not_found` | none | Nothing plausible. |

### `exact` — perfect match

The exact house number was found in BeSt, at the given street, at the given postal code.

```
input :  Avenue Louise 32 1050
found :  Avenue Louise 32, 1050 Ixelles   lat=50.83451 lng=4.35670
         confidence = exact
         fallback_reason = (empty)
         house_number_delta = 0
```

**Action**: use as-is.

### `nearest_same_side` / `nearest_any_side` — a neighbouring number

The street was found but the requested number doesn't exist on it (at that postal code). crabfinder returns the **closest existing number on the same side of the street** (same parity: odd with odd, even with even) — like a postman would. If that side has no number close enough, it takes the closest one on the other side (`nearest_any_side`). On a tie, the lower number wins. Letter suffixes are ignored for the distance (`34B` counts as 34).

```
input :  Avenue Louise 33 1050
found :  Avenue Louise 59, 1050 Bruxelles   lat=50.83310 lng=4.35728
         confidence = nearest_same_side
         fallback_reason = house_number_out_of_range
         house_number_delta = 26          (59 − 33)
```

`house_number_delta` is the matched number minus the requested one: the larger it is, the further the point may be from the real address. A neighbour is only accepted within `max(50, requested / 2)` numbers; beyond that (e.g. `Avenue Louise 9999`) the result falls back to `median_street`.

**Action**: building-level coordinates of a real neighbouring address. Fine for maps when `|house_number_delta|` is small; inspect large deltas.

### `median_street` — street centroid

No usable number: missing in the input, or too far from every existing number. crabfinder returns a **representative point for the street**: the building whose number sits at the median of the existing numbers for that street and postal code.

```
input :  Grand-Place 1000     (no house_number)
found :  Grand-Place 2, 1000 Bruxelles   lat=50.84623 lng=4.35301
         confidence = median_street
         fallback_reason = house_number_missing
         house_number_delta = (empty)
```

**Spatial precision**: **street-level**, not building-level. Drift can be a few metres on a short street, or several hundred metres on a long one. For neighbourhood-level aggregation, usually acceptable. For precise marker placement on a map, it's misleading — at minimum, render with a different icon.

Two possible `fallback_reason`s:

- `house_number_missing`: the input had no house number.
- `house_number_out_of_range`: a number was supplied but no existing number on that street is close enough.

**Action**: usable for spatial aggregates, avoid for building-level analysis. Batch users who'd rather get no point at all can turn this fallback off (`--no-mid-street` / `mid_street_fallback=false`); the row is then `not_found`.

### `fuzzy_street` — approximately matched street

The street name didn't match as typed. Two mechanisms produce it:

- **Typo correction** (`match_type=typo`): one or two letters wrong in a street word ("Avenue Loiuse", "Rue Beliard"). Usually right.
- **Partial match** (`match_type=fuzzy`): not every word matched; crabfinder kept the street sharing the most words with your query. Riskier.

Either way, the result may be the requested building on that street (if the number exists: `house_number_delta=0`, building-level) or the street's representative point (`house_number_delta` empty, street-level).

```
input :  Avenue Loiuse 32 1050     (typo on "Louise")
found :  Avenue Louise 32, 1050 Ixelles   lat=50.83451 lng=4.35670
         confidence = fuzzy_street
         fallback_reason = street_spelling_fuzzy
         match_type = typo
```

**Action**: inspect, especially `match_type=fuzzy`. Compare `matched_street` with your input; a low `street_match_score` (below ~10) suggests a false positive.

### `locality` — a place, not an address

The query only named a place — a municipality, a part of municipality, a postal area, or an OpenStreetMap village or hamlet ("Spontin", "Maredsous", "Han-sur-Lesse 5580"). There is no street to place the point on.

- Batch: the row gets the **place's centre**, the label `5530 Spontin`, the postal code; `matched_street`, `building_group_id` and `best_id` stay empty.
- `/geocode`: the features are the addresses closest to the place (`match_type=spatial`), and the top-level `locality` object gives the place itself (kind, names, postal codes, centre).

```
input :  Spontin
found :  5530 Spontin   lat=50.32262 lng=5.00695
         confidence = locality
         fallback_reason = street_missing
```

**Action**: village-level precision (hundreds of metres to a few km). Good for "which municipality", not for a delivery.

### `not_found` — no coordinates

Nothing plausible was found. The row exits with empty `lat/lng/matched_*`, `confidence = not_found`, `fallback_reason = no_match`.

```
input :  Some Nonexistent Street 99 1000
found :  (nothing)
         confidence = not_found
         fallback_reason = no_match
```

**Action**: investigate row-by-row. Possible causes:
- A typo too heavy to correct (more than 2 letters, or in a short word).
- Wrong postal code (the street doesn't exist at that PC).
- Recent address not yet in the BeSt dump (refresh needed).
- Address outside Belgium.
- Retired address (only current BeSt addresses are indexed).

### `adjacent_commune` — reserved

Defined in the output schema (street found in a neighbouring municipality, wrong input PC) but **not emitted yet**. Your scripts can ignore it for now.

## The `fallback_reason` taxonomy

Finer-grained than `confidence`. When `confidence != exact`, it tells you **why**:

| `fallback_reason` | Meaning | With |
|---|---|---|
| `house_number_missing` | The input had no house number. | `median_street`, `fuzzy_street` |
| `house_number_out_of_range` | A number was supplied but doesn't exist for that street in BeSt. | `nearest_*`, `median_street` |
| `street_spelling_fuzzy` | The street matched only after typo correction or partially. | `fuzzy_street` |
| `street_missing` | The query names a place but no street. | `locality` |
| `postal_code_mismatch` | Reserved (future adjacent-commune fallback). | — |
| `no_match` | Nothing found at all. | `not_found` |

Combined case: a row with `confidence=fuzzy_street, fallback_reason=house_number_missing` means "I approximated the street *and* you didn't provide a number". That's double degradation — worth inspecting more than single-degradation cases.

## The `house_number_delta` field

Matched number minus requested number, filled whenever the requested number was honoured:

- `0`: the requested number was found (`exact`, and `fuzzy_street` on the right building).
- non-zero: a neighbour was returned (`nearest_*`); `26` = 26 numbers further up the street, `-2` = 2 numbers down.
- empty: street-level and locality results, `not_found`.

## Where the coordinates come from: `position_source`

BeSt publishes a position for almost every address, but not all: in the 2026-10-15 dump, **140,263 Walloon addresses** had no position yet (published at Lambert 72 `0 0`, which would land in France). crabfinder gives them a position from their neighbours instead, and says so:

| `position_source` | How the point was obtained | Precision | 2026-10-15 dump |
|---|---|---|---|
| `measured` | Published by BeSt. | building | 6,746,374 |
| `same_number` | Another unit with the same number on the same street and postal code has a position: shared. | building | 44,909 |
| `interpolated` | Between the nearest numbered neighbours on the same street and postal code, on the same side when possible. | approximately the building (tens of metres) | 87,293 |
| `street` | The street has positioned addresses but none to interpolate from: the street's representative point. | street | 0 |
| `locality` | Nothing on the street has a position: the centre of the address's part of municipality (else municipality). | village | 8,061 |

Real examples: `Rue Longue 205, 1320 Beauvechain` is `interpolated`, `Stoquoit 9Bis, 7863 Lessines` is `same_number`, `Chemin du Bois Robert 1B, 4860 Pepinster` is `locality`.

Where to find it:

- Batch: the `position_source` column, always filled.
- `/geocode`, `/building`: `properties.position_source`, present only when the position was imputed (absent = `measured`).

Note that `confidence` is about the **text match**, `position_source` about the **point**: an `exact` row can still carry a `locality` position — the right address, placed at its village centre. For building-level uses, accept `measured`, `same_number` and `interpolated`, and treat `street` / `locality` like `median_street` / `locality` results. `/reverse` never returns addresses with a `street` or `locality` position.

## Ambiguity: `ambiguous` and `score_gap`

`ambiguous=true` means the runner-up scored at least 90% of the chosen answer **and** lies on a different street or municipality. Typical case: a common street name without postal code ("Kerkstraat 1", "Rue de la Station 1") — crabfinder picked one of many equally plausible candidates.

- Batch: the `ambiguous` column (`true` / `false`).
- `/geocode`: top-level `ambiguous`, plus `score_gap` = `1 − score₂/score₁` when there are at least two results (`0` = a tie).

**Action**: treat an ambiguous row as unconfirmed whatever its confidence. Adding the postal code almost always resolves it.

## The `street_match_score`

This is the **raw BM25 score** from the text search against the `searchable` field (all multilingual street name variants + municipality + postal names). Not a normalized score. Not a percentage. A continuous quantity that depends on:

- token frequency in the query,
- token frequency in the corpus (rare tokens weigh more),
- any notability boost (Grand-Place, Atomium, etc.).

On the Tier A eval (on `exact` hits):

| Quartile | BM25 value |
|---|---|
| q25 | ~25.8 |
| q50 (median) | ~32.8 |
| q75 | ~101.1 |
| q95 | ~144.4 |

**Takeaway**: a score below q25 on an `exact` hit is *suspect* (weak text match, possibly a very short street name). A score above q75 is solid `exact`. To compare two results, the score is usable; for absolute decisions, **use `confidence`, not the score**.

For `fuzzy_street`, scores are typically lower. For `not_found`, the field is empty.

## The `building_group_id` and `best_id` identifiers

Two downstream-usable references:

- `building_group_id`: crabfinder's building identifier, `{street id}|{house number}` (e.g. `https://databrussels.be/id/streetname|3919|32`). Stable across BeSt dumps as long as the street id and number are. All boxes of a building share it — useful for grouping on your side, or for `/building/{id}/units`.
- `best_id`: official BeSt identifier in the form `namespace/object_identifier/version`. Lets you trace back to the source BeSt row (audit, joins against other BeSt data). When you give a box number that BeSt knows, it is that unit's id.

## Mini-recipe for sorting

You can sort your geocoded CSV by confidence + score to process certain cases first and inspect doubtful ones later, for example with `awk`:

```bash
# List rows by quality descending (exact first, not_found last)
awk -F, 'NR==1 {print; next} {
  rank = ($10=="exact") ? 1 :
         ($10 ~ /^nearest_/) ? 2 :
         ($10=="fuzzy_street") ? 3 :
         ($10=="median_street") ? 4 :
         ($10=="locality") ? 5 : 9
  print rank","$0
}' geocoded.csv | sort -t, -k1n | cut -d, -f2-
```

(Splitting on `,` is only safe while no field contains a comma; the `label` column does, so prefer the SQL route or a real CSV tool such as `xsv`/`qsv` for anything serious.)

Or in SQL if you load the CSV into a table:

```sql
SELECT id, street, postal_code, lat, lng, confidence, fallback_reason, position_source, ambiguous
FROM geocoded_addresses
ORDER BY CASE confidence
           WHEN 'exact'             THEN 1
           WHEN 'nearest_same_side' THEN 2
           WHEN 'nearest_any_side'  THEN 2
           WHEN 'fuzzy_street'      THEN 3
           WHEN 'median_street'     THEN 4
           WHEN 'locality'          THEN 5
           WHEN 'not_found'         THEN 9
           ELSE 6
         END,
         ambiguous,
         street_match_score DESC NULLS LAST;
```

## What's next

- Unexpected behaviour, performance, startup error → [`06-faq.md`](06-faq.md).
- Back to batch mode → [`03-batch.md`](03-batch.md).
- Back to the online API → [`04-online-search.md`](04-online-search.md).
