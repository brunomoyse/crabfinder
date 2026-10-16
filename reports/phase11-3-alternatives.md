# Phase 11.3: alternatives by municipality

Eval run with `--alternatives` (`/geocode?alternatives=true`) on the synthetic v3 + adversarial sets, BeSt 2026-10-15, schema v11, BeSt-only service. Asking for alternatives fetches 50 buildings but doesn't change the features: per query at rank@1, 0 fixed / 0 broken against the Phase 11.1b run.

The target was the `minimal` variant (street + number, no municipality): recall@1 0.792. With the alternatives, the expected building is the best answer of one of the first 3 municipalities for 0.886 of those queries.

The expected building is the best answer of one of the first k municipality groups.

| Variant | n | recall@1 | recall@3 |
|---|---|---|---|
| abbreviated | 289 | 1.000 | 1.000 |
| box_number | 61 | 1.000 | 1.000 |
| canonical | 500 | 1.000 | 1.000 |
| casing | 500 | 1.000 | 1.000 |
| cross_language | 306 | 1.000 | 1.000 |
| high_house_number | 101 | 1.000 | 1.000 |
| messy_punctuation | 500 | 1.000 | 1.000 |
| minimal | 500 | 0.792 | 0.886 |
| typo | 487 | 0.996 | 0.996 |
| with_postal | 500 | 1.000 | 1.000 |
| wrong_postal | 500 | 0.958 | 0.966 |
| all | 4244 | 0.970 | 0.982 |
