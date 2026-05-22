# 5. Understanding results

Read this before acting on a geocoded CSV or making an automated decision off a `/geocode` response. This section explains what `confidence`, `fallback_reason`, and `score` mean, and how to use them for sorting or filtering.

## The golden rule

**Every output row has a coordinate *and* a confidence level**, except `not_found` which has neither. Quality varies. You decide what's acceptable for your case.

Typical acceptance thresholds:

| Use | Acceptable | Inspect | Reject |
|---|---|---|---|
| Map with per-building markers | `exact` | `fuzzy_street`, `nearest_*` | `median_street`, `not_found` |
| Aggregation by neighbourhood / municipality | `exact`, `median_street`, `fuzzy_street` | `nearest_*`, `adjacent_commune` | `not_found` |
| Spatial join with polygons (density, sector) | everything except `not_found` | — | `not_found` |
| Billing address | `exact` only | everything else | everything else |

## The `confidence` taxonomy

Seven values, ordered from strongest to weakest.

### `exact` — perfect match

The exact house number was found in BeSt, at the given street, at the given postal code.

```
input :  Avenue Louise 32 1050
found :  Avenue Louise 32 1050 Ixelles, lat=50.832 lng=4.367
         confidence = exact
         fallback_reason = (empty)
```

**Action**: use as-is.

### `median_street` — street centroid

The requested number doesn't exist in BeSt (missing in input, or present in input but absent from the official dataset). crabfinder returns a **representative point for the street**: the coordinates of the building whose number sits at the median of existing numbers for the `(street, PC)` pair.

```
input :  Grand-Place 1000     (no house_number)
found :  Grand-Place 1000, lat=50.847 lng=4.353 (median of numbers 1 to 19)
         confidence = median_street
         fallback_reason = house_number_missing
         house_number_delta = (empty)
```

**Spatial precision**: **street-level**, not building-level. Drift can be a few metres on a short street, or several hundred metres on a long one. For IRIS / neighbourhood-level aggregation, usually acceptable. For precise marker placement on a map, it's misleading — at minimum, render with a different icon.

**Action**: usable for spatial aggregates, avoid for building-level analysis.

Two possible `fallback_reason`s in this case:

- `house_number_missing`: the input `house_number` was empty.
- `house_number_out_of_range`: a number was supplied but doesn't exist in BeSt for that street.

### `fuzzy_street` — approximately matched street

The strict pass failed (misspelled street, unrecognized abbreviation…). The fuzzy OR-of-tokens fallback found a street sharing enough tokens with your query. The result may be:

- The exact building at that fuzzy street (if `house_number` happened to match) → building-level coordinates.
- The centroid of the fuzzy street (if `house_number` didn't match either) → street-level coordinates, cumulative `fallback_reason = house_number_missing`.

```
input :  Avenu Louise 32 1050     (typo on "Avenue")
found :  Avenue Louise 32 1050, lat=50.832 lng=4.367
         confidence = fuzzy_street
         fallback_reason = street_spelling_fuzzy
         street_match_score = low (typically < 10)
```

**Action**: inspect. The BM25 score (`street_match_score`) is a decent text-match indicator; low values (< 10 on the Tier A eval) suggest a false positive.

### `not_found` — no coordinates

Neither the strict nor the fuzzy pass found a plausible candidate. The row exits with empty `lat/lng/matched_*`, `confidence = not_found`, `fallback_reason = no_match`.

```
input :  Nonexistent Street 99 1000
found :  (nothing)
         confidence = not_found
         fallback_reason = no_match
```

**Action**: investigate row-by-row. Possible causes:
- Severe typo the fuzzy fallback can't bridge.
- Wrong postal code (the street doesn't exist at that PC).
- Recent address not yet in the BeSt dump (refresh needed).
- Address outside Belgium.
- Historically removed address (the BeSt dump filters non-active statuses).

On the Tier A eval (~2600 queries), the `not_found` rate is about 0.1%. If yours is substantially higher, there's probably a systematic input issue (encoding, mass-missing PCs, etc.) worth investigating.

### Confidences reserved for future versions

These three values are defined in the output schema but **not emitted in v1**. They'll become active once the corresponding feature is implemented:

- `nearest_same_side` — closest same-parity number (= same side of the street). Inspired by PhacochR §2.2.
- `nearest_any_side` — closest number, any side.
- `adjacent_commune` — street found in a neighbouring municipality (wrong input PC).

Your scripts can ignore them for now; they will never come out of a crabfinder v1.

## The `fallback_reason` taxonomy

Finer-grained than `confidence`. When `confidence != exact`, it tells you **why**:

| `fallback_reason` | Meaning |
|---|---|
| `house_number_missing` | The input `house_number` was empty. |
| `house_number_out_of_range` | A number was supplied but doesn't exist for that street in BeSt. |
| `street_spelling_fuzzy` | Strict pass didn't find the street, fuzzy did. |
| `postal_code_mismatch` | Reserved (future adjacent-commune fallback). |
| `no_match` | Nothing found at all. Always paired with `confidence=not_found`. |

Combined case: a row with `confidence=fuzzy_street, fallback_reason=house_number_missing` means "I approximated the street *and* you didn't provide a number". That's double degradation — worth inspecting more than single-degradation cases.

## The `street_match_score`

This is the **raw BM25 score** from the Tantivy pass against the `searchable` field (which contains all multilingual street name variants + municipality + PC). Not a normalized score. Not a percentage. A continuous quantity that depends on:

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

For `fuzzy_street`, scores are typically lower (5-15). For `median_street` and `not_found`, the field is empty.

## The `house_number_delta` field

Filled in v1 only for confidences that warrant it (eventually: `nearest_*`). In current v1, **always empty or `0`**. Reserved.

## The `building_group_id` and `best_id` identifiers

Two downstream-usable references:

- `building_group_id`: crabfinder's synthetic identifier. Stable across BeSt dumps as long as the `(street_id, house_number)` pair is stable. Useful for grouping on your side (all boxes in the same building share the same `building_group_id`).
- `best_id`: official BeSt identifier in the form `namespace/object_identifier/version`. Lets you trace back to the source BeSt row (useful for audit or joining against other BeSt data).

## Mini-recipe for sorting

You can sort your geocoded CSV by confidence + score to process certain cases first and inspect doubtful ones later, for example with `awk`:

```bash
# List rows by quality descending (exact first, not_found last)
awk -F, 'NR==1 {print; next} {
  rank = ($10=="exact") ? 1 :
         ($10=="fuzzy_street") ? 2 :
         ($10=="median_street") ? 3 : 4
  print rank","$0
}' geocoded.csv | sort -t, -k1n | cut -d, -f2-
```

Or in SQL if you load the CSV into a table:

```sql
SELECT id, street, postal_code, lat, lng, confidence, fallback_reason
FROM geocoded_addresses
ORDER BY CASE confidence
           WHEN 'exact'          THEN 1
           WHEN 'fuzzy_street'   THEN 2
           WHEN 'median_street'  THEN 3
           WHEN 'not_found'      THEN 9
           ELSE 5
         END,
         street_match_score DESC NULLS LAST;
```

## What's next

- Unexpected behaviour, performance, startup error → [`06-faq.md`](06-faq.md).
- Back to batch mode → [`03-batch.md`](03-batch.md).
- Back to the online API → [`04-online-search.md`](04-online-search.md).
