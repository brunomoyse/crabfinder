//! Pure Tantivy query construction.
//!
//! Splits out of `main.rs` so both the HTTP `/geocode` handler and the batch
//! `resolve()` core consume the same builders. Nothing in this module touches
//! HTTP, CSV, or CLI surfaces — it only knows about Tantivy schema + queries
//! and the parsed query shape.

use std::sync::Arc;

use std::ops::Bound;
use tantivy::collector::TopDocs;

use tantivy::query::{
    BooleanQuery, BoostQuery, ConstScoreQuery, FuzzyTermQuery, Occur, PhraseQuery,
    Query as TantivyQuery, RangeQuery, TermQuery,
};
use tantivy::schema::IndexRecordOption;
use tantivy::{DocId, Index, Score, SegmentReader, Term};

use crabfinder_indexer::index::CrabSchema;
use crabfinder_parser::ParsedQuery;

use crate::addr_count::SharedAddressCountIndex;

/// Single searchable text field (schema v3+). Every multilingual variant of
/// street/muni/postal names is indexed into this one multi-valued field, so
/// query builders use a plain Term/Phrase/Boolean against it.
pub fn searchable_field(schema: &CrabSchema) -> tantivy::schema::Field {
    schema.searchable
}

/// Run the registered "crab" analyzer on a string and collect the resulting
/// tokens. Indexer and service share the same tokenizer registration, so
/// query-side and index-side token streams agree byte-for-byte.
pub fn analyze(index: &Index, text: &str) -> Vec<String> {
    let mut analyzer = index
        .tokenizers()
        .get(crabfinder_indexer::index::TOKENIZER_NAME)
        .expect("crab tokenizer registered at startup");
    let mut out = Vec::new();
    let mut stream = analyzer.token_stream(text);
    while let Some(t) = stream.next() {
        out.push(t.text.clone());
    }
    out
}

/// Weight on the address-count tiebreaker. Small enough that two docs
/// with similar BM25 + identical notability are separated by the busier-
/// street signal, but not so large that obscure exact matches get
/// crowded out by popular look-alikes.
///
/// Empirical (2026-05-22): at `0.05` the signal crushes adversarial
/// `hamlet` recall@1 from 0.467 → 0.233 because popular streets eat the
/// top-N even when the user is searching for a rare variant. `0.02`
/// keeps the `minimal` variant lift (+0.020) most of the way and limits
/// the hamlet regression to a tolerable band.
pub const ADDR_COUNT_WEIGHT: f32 = 0.005;

/// Plain BM25 ranking, no per-doc multiplier. Used when the service is
/// started with `--no-notability-boost` (A/B isolation against the same
/// physical index). Keeps the same `(Score, DocAddress)` shape as the
/// notability path so the caller is shape-agnostic.
pub fn search_score_only(
    searcher: &tantivy::Searcher,
    query: &dyn TantivyQuery,
    limit: usize,
) -> tantivy::Result<Vec<(tantivy::Score, tantivy::DocAddress)>> {
    let collector = TopDocs::with_limit(limit).order_by_score();
    searcher.search(query, &collector)
}

/// Top-N search with the schema-v4 `notability_score` FAST field folded into
/// the rank order. Each candidate's BM25 score is multiplied by its per-doc
/// notability multiplier (1.0 = no boost, > 1.0 = curated landmark), then
/// combined with a log-scaled address-count popularity tiebreaker.
pub fn search_with_notability(
    searcher: &tantivy::Searcher,
    query: &dyn TantivyQuery,
    limit: usize,
    addr_counts: SharedAddressCountIndex,
) -> tantivy::Result<Vec<(tantivy::Score, tantivy::DocAddress)>> {
    let collector = TopDocs::with_limit(limit).tweak_score(move |seg: &SegmentReader| {
        let notability_col = seg
            .fast_fields()
            .f64(crabfinder_indexer::index::FIELD_NOTABILITY_SCORE)
            .ok()
            .map(|c| c.first_or_default_col(1.0));
        let counts = addr_counts.for_segment(seg);
        move |doc: DocId, score: Score| -> Score {
            let mult = notability_col
                .as_ref()
                .map(|c| c.get_val(doc) as f32)
                .unwrap_or(1.0);
            let count = counts
                .as_ref()
                .and_then(|v: &Arc<Vec<u32>>| v.get(doc as usize).copied())
                .unwrap_or(1) as f32;
            let popularity = 1.0 + ADDR_COUNT_WEIGHT * (1.0 + count).ln();
            score * mult * popularity
        }
    });
    searcher.search(query, &collector)
}

/// Optional per-request filters applied as `Occur::Must` term queries
/// alongside the textual match. When `None`, the filter is skipped — the
/// query behaves like the pre-filter path.
#[derive(Default, Clone)]
pub struct FilterParams {
    /// Indexed ISO form (`BE-BRU` / `BE-VLG` / `BE-WAL`).
    pub region: Option<String>,
    /// Belgian 4-digit postal code. Overrides any postal code the parser
    /// pulled out of the free-text `q`.
    pub postal_code: Option<String>,
    /// Bounding box around a named locality (v2 Phase 6).
    pub area: Option<Area>,
    /// Caller's rectangle (`/geocode?bbox=`, v2 Phase 10).
    pub bbox: Option<BBox>,
}

/// A lat/lon rectangle, filtered on the FAST `lat` / `lon` fields.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BBox {
    pub min_lat: f64,
    pub max_lat: f64,
    pub min_lon: f64,
    pub max_lon: f64,
}

impl BBox {
    /// Parse `min_lon,min_lat,max_lon,max_lat` (the GeoJSON / OSM order).
    /// `None` unless all four are finite, in range and min < max.
    pub fn parse(raw: &str) -> Option<Self> {
        let v: Vec<f64> = raw
            .split(',')
            .map(|p| p.trim().parse::<f64>())
            .collect::<Result<_, _>>()
            .ok()?;
        let [min_lon, min_lat, max_lon, max_lat] = v[..] else {
            return None;
        };
        let lat_ok = |x: f64| (-90.0..=90.0).contains(&x);
        let lon_ok = |x: f64| (-180.0..=180.0).contains(&x);
        (lat_ok(min_lat)
            && lat_ok(max_lat)
            && lon_ok(min_lon)
            && lon_ok(max_lon)
            && min_lat < max_lat
            && min_lon < max_lon)
            .then_some(Self {
                min_lat,
                max_lat,
                min_lon,
                max_lon,
            })
    }

    pub fn contains(&self, lat: f64, lon: f64) -> bool {
        (self.min_lat..=self.max_lat).contains(&lat) && (self.min_lon..=self.max_lon).contains(&lon)
    }
}

/// A circle around a point, filtered as its bounding box on the FAST
/// `lat` / `lon` fields.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Area {
    pub lat: f64,
    pub lon: f64,
    pub radius_m: f64,
}

impl Area {
    /// `(min_lat, max_lat, min_lon, max_lon)`.
    pub fn bounds(&self) -> (f64, f64, f64, f64) {
        const M_PER_DEG: f64 = 111_320.0;
        let dlat = self.radius_m / M_PER_DEG;
        let dlon = self.radius_m / (M_PER_DEG * self.lat.to_radians().cos().max(0.1));
        (
            self.lat - dlat,
            self.lat + dlat,
            self.lon - dlon,
            self.lon + dlon,
        )
    }
}

/// Must clauses keeping `lat` / `lon` inside `(min_lat, max_lat, min_lon,
/// max_lon)`.
fn bounds_clauses(
    schema: &CrabSchema,
    (lat0, lat1, lon0, lon1): (f64, f64, f64, f64),
) -> [Box<dyn TantivyQuery>; 2] {
    let range = |field, lo, hi| -> Box<dyn TantivyQuery> {
        Box::new(RangeQuery::new(
            Bound::Included(Term::from_field_f64(field, lo)),
            Bound::Included(Term::from_field_f64(field, hi)),
        ))
    };
    [range(schema.lat, lat0, lat1), range(schema.lon, lon0, lon1)]
}

/// Every document inside `area` (the locality fallback when no text
/// matches), with any other filters.
pub fn build_area_query(
    schema: &CrabSchema,
    filters: &FilterParams,
) -> Option<Box<dyn TantivyQuery>> {
    filters.area?;
    let mut clauses = Vec::new();
    push_filters(&mut clauses, &ParsedQuery::default(), schema, filters);
    Some(Box::new(BooleanQuery::new(clauses)))
}

/// Build the strict query: SHOULD(phrase, boost=3.0) + SHOULD(AND-tokens,
/// boost=1.0) against the single `searchable` field, plus MUST filters on
/// house number and postal code when present.
///
/// `slots` is one inner vec per query token; each inner vec is the set of
/// forms acceptable for that slot (the original token followed by any
/// abbreviation-expansion canonicals — see `AbbreviationMap::expand`). A slot
/// with exactly one form behaves identically to the pre-expansion path.
///
/// Strict structure: AND across slots, OR within each slot. The phrase boost
/// uses the FIRST form of each slot (the originally-typed user token);
/// abbreviation queries don't get the phrase bonus, but the AND-of-(OR-of-forms)
/// branch still resolves them.
///
/// Returns None when there is nothing to search for (no street tokens and no
/// filters).
pub fn build_strict_query(
    field: tantivy::schema::Field,
    slots: &[Vec<String>],
    parsed: &ParsedQuery,
    schema: &CrabSchema,
    filters: &FilterParams,
) -> Option<Box<dyn TantivyQuery>> {
    let mut clauses: Vec<(Occur, Box<dyn TantivyQuery>)> = Vec::new();

    if !slots.is_empty() {
        let mut text_branches: Vec<(Occur, Box<dyn TantivyQuery>)> = Vec::new();

        // Phrase query — only if we have ≥2 slots. Tantivy refuses single-
        // term phrase queries; a single-slot query is handled by the AND
        // branch instead. The phrase uses the user-typed form of each token
        // (slot[0]); abbreviation expansions don't participate in phrase
        // ordering.
        if slots.len() >= 2 {
            let phrase_terms: Vec<Term> = slots
                .iter()
                .map(|s| Term::from_field_text(field, &s[0]))
                .collect();
            let phrase = PhraseQuery::new(phrase_terms);
            let boosted: Box<dyn TantivyQuery> = Box::new(BoostQuery::new(Box::new(phrase), 3.0));
            text_branches.push((Occur::Should, boosted));
        }
        // Adjacent pairs (v2 Phase 6). The full phrase can never match
        // when the query also names the municipality — street and
        // municipality are separate values of `searchable`, and Tantivy
        // leaves a position gap between values — so without these "Grand
        // Place Bruxelles" ranks "Place du Grand Sablon" level with
        // "Grand-Place". Each adjacent pair found in order adds a bonus.
        if slots.len() >= 3 {
            for pair in slots.windows(2) {
                let terms = vec![
                    Term::from_field_text(field, &pair[0][0]),
                    Term::from_field_text(field, &pair[1][0]),
                ];
                let q: Box<dyn TantivyQuery> =
                    Box::new(BoostQuery::new(Box::new(PhraseQuery::new(terms)), 1.5));
                text_branches.push((Occur::Should, q));
            }
        }

        // AND-of-(OR-of-forms). Each slot is a sub-BooleanQuery OR-ing the
        // slot's forms; the outer BooleanQuery AND-s the slots together.
        let and_clauses: Vec<(Occur, Box<dyn TantivyQuery>)> = slots
            .iter()
            .map(|forms| {
                let slot_q: Box<dyn TantivyQuery> = if forms.len() == 1 {
                    let term = Term::from_field_text(field, &forms[0]);
                    Box::new(TermQuery::new(term, IndexRecordOption::WithFreqs))
                } else {
                    let or_clauses: Vec<(Occur, Box<dyn TantivyQuery>)> = forms
                        .iter()
                        .map(|f| {
                            let term = Term::from_field_text(field, f);
                            let tq: Box<dyn TantivyQuery> =
                                Box::new(TermQuery::new(term, IndexRecordOption::WithFreqs));
                            (Occur::Should, tq)
                        })
                        .collect();
                    Box::new(BooleanQuery::new(or_clauses))
                };
                (Occur::Must, slot_q)
            })
            .collect();
        let and_q: Box<dyn TantivyQuery> = Box::new(BooleanQuery::new(and_clauses));
        // No boost on AND-tokens — let phrase out-rank it naturally. The
        // AND branch is the Must; phrases only add score on top of it (a
        // lone matching pair must never satisfy the query).
        text_branches.push((Occur::Must, and_q));
        text_branches.extend(
            exact_name_clauses(schema, slots)
                .into_iter()
                .map(|q| (Occur::Should, q)),
        );

        let textual: Box<dyn TantivyQuery> = Box::new(BooleanQuery::new(text_branches));
        clauses.push((Occur::Must, textual));
    }

    push_filters(&mut clauses, parsed, schema, filters);

    if clauses.is_empty() {
        return None;
    }
    Some(Box::new(BooleanQuery::new(clauses)))
}

/// Score added when the street is named exactly a leading run of the query
/// words (schema v10). Constant, so rare and common names gain alike; the
/// size of one more matching word of `searchable`.
pub const EXACT_NAME_BONUS: Score = 10.0;

/// Leading runs of slots tried as whole street names, longest first, and
/// the cap on the form combinations they expand to.
const MAX_EXACT_KEYS: usize = 16;

/// Should clauses rewarding a street named exactly `slots[..k]` for some
/// `k` (any abbreviation form per slot): "Markt" → *Markt* over *Grote
/// Markt*, "Kerkstraat Gent" → *Kerkstraat* over *Oude Kerkstraat*.
fn exact_name_clauses(schema: &CrabSchema, slots: &[Vec<String>]) -> Vec<Box<dyn TantivyQuery>> {
    let mut keys: Vec<String> = Vec::new();
    for k in (1..=slots.len()).rev() {
        let mut combos = vec![String::new()];
        for forms in &slots[..k] {
            combos = combos
                .iter()
                .flat_map(|c| {
                    forms.iter().map(move |f| {
                        if c.is_empty() {
                            f.clone()
                        } else {
                            format!("{c} {f}")
                        }
                    })
                })
                .take(MAX_EXACT_KEYS)
                .collect();
        }
        keys.extend(combos);
        if keys.len() >= MAX_EXACT_KEYS {
            break;
        }
    }
    keys.truncate(MAX_EXACT_KEYS);
    keys.sort();
    keys.dedup();
    keys.into_iter()
        .map(|k| {
            let tq = TermQuery::new(
                Term::from_field_text(schema.street_exact, &k),
                IndexRecordOption::Basic,
            );
            Box::new(ConstScoreQuery::new(Box::new(tq), EXACT_NAME_BONUS)) as Box<dyn TantivyQuery>
        })
        .collect()
}

/// Must clauses shared by every tier: house number (when the reading
/// carries one), postal code (explicit filter first) and region.
fn push_filters(
    clauses: &mut Vec<(Occur, Box<dyn TantivyQuery>)>,
    parsed: &ParsedQuery,
    schema: &CrabSchema,
    filters: &FilterParams,
) {
    if let Some(hn) = &parsed.house_number {
        clauses.push((Occur::Must, house_number_clause(schema, hn)));
    }
    let postal = filters
        .postal_code
        .as_deref()
        .or(parsed.postal_code.as_deref());
    if let Some(pc) = postal {
        let term = Term::from_field_text(schema.postal_code, pc);
        let tq: Box<dyn TantivyQuery> = Box::new(TermQuery::new(term, IndexRecordOption::Basic));
        clauses.push((Occur::Must, tq));
    }
    if let Some(region) = filters.region.as_deref() {
        let term = Term::from_field_text(schema.region, region);
        let tq: Box<dyn TantivyQuery> = Box::new(TermQuery::new(term, IndexRecordOption::Basic));
        clauses.push((Occur::Must, tq));
    }
    if let Some(area) = &filters.area {
        for q in bounds_clauses(schema, area.bounds()) {
            clauses.push((Occur::Must, q));
        }
    }
    if let Some(b) = &filters.bbox {
        for q in bounds_clauses(schema, (b.min_lat, b.max_lat, b.min_lon, b.max_lon)) {
            clauses.push((Occur::Must, q));
        }
    }
}

/// Edit distance allowed for a typed word of `chars` characters (Phase 3):
/// none under 5, one for 5–8, two from 9.
pub fn typo_distance(chars: usize) -> u8 {
    match chars {
        0..=4 => 0,
        5..=8 => 1,
        _ => 2,
    }
}

/// Whether no form of a slot exists in the searchable vocabulary — the
/// only slots the typo tier makes fuzzy, so a correctly spelled word is
/// never "corrected" into a neighbouring street name.
pub fn slot_is_unknown(
    searcher: &tantivy::Searcher,
    field: tantivy::schema::Field,
    forms: &[String],
) -> bool {
    forms.iter().all(|f| {
        searcher
            .doc_freq(&Term::from_field_text(field, f))
            .map_or(true, |n| n == 0)
    })
}

/// Typo tier (Phase 3): the strict AND-of-slots with every unknown slot
/// replaced by a Damerau–Levenshtein `FuzzyTermQuery` on the typed form.
/// For two-edit words a one-edit clause is added with boost 2.0, so the
/// closer correction ranks first (Tantivy scores fuzzy hits as constants).
///
/// `None` when no slot is unknown (the strict tiers already cover it) or
/// an unknown slot is too short to correct.
pub fn build_typo_query(
    field: tantivy::schema::Field,
    slots: &[Vec<String>],
    unknown: &[bool],
    parsed: &ParsedQuery,
    schema: &CrabSchema,
    filters: &FilterParams,
) -> Option<Box<dyn TantivyQuery>> {
    if !unknown.iter().any(|u| *u) {
        return None;
    }
    let mut clauses: Vec<(Occur, Box<dyn TantivyQuery>)> = Vec::with_capacity(slots.len() + 3);
    for (forms, is_unknown) in slots.iter().zip(unknown) {
        let slot_q: Box<dyn TantivyQuery> = if *is_unknown {
            let typed = &forms[0];
            let d = typo_distance(typed.chars().count());
            if d == 0 {
                return None;
            }
            let term = Term::from_field_text(field, typed);
            let one: Box<dyn TantivyQuery> = Box::new(BoostQuery::new(
                Box::new(FuzzyTermQuery::new(term.clone(), 1, true)),
                2.0,
            ));
            if d == 1 {
                one
            } else {
                let two: Box<dyn TantivyQuery> = Box::new(FuzzyTermQuery::new(term, 2, true));
                Box::new(BooleanQuery::new(vec![
                    (Occur::Should, one),
                    (Occur::Should, two),
                ]))
            }
        } else {
            let forms_q: Vec<(Occur, Box<dyn TantivyQuery>)> = forms
                .iter()
                .map(|f| {
                    let tq: Box<dyn TantivyQuery> = Box::new(TermQuery::new(
                        Term::from_field_text(field, f),
                        IndexRecordOption::WithFreqs,
                    ));
                    (Occur::Should, tq)
                })
                .collect();
            Box::new(BooleanQuery::new(forms_q))
        };
        clauses.push((Occur::Must, slot_q));
    }
    push_filters(&mut clauses, parsed, schema, filters);
    Some(Box::new(BooleanQuery::new(clauses)))
}

/// House-number filter that ignores the letter suffix's case: schema v8
/// indexes `house_number` uppercased (`32a` → `32A`).
pub(crate) fn house_number_clause(schema: &CrabSchema, hn: &str) -> Box<dyn TantivyQuery> {
    let term = Term::from_field_text(schema.house_number, &hn.to_uppercase());
    Box::new(TermQuery::new(term, IndexRecordOption::Basic))
}

#[cfg(test)]
mod strict_tests {
    use super::*;
    use crabfinder_indexer::index::register_tokenizers;
    use tantivy::Index;

    fn empty_index_with_tokenizer() -> Index {
        let schema = CrabSchema::build();
        let index = Index::create_in_ram(schema.schema.clone());
        register_tokenizers(&index);
        index
    }

    #[test]
    fn analyze_lowercases_and_folds_accents() {
        let index = empty_index_with_tokenizer();
        // "É" → "e" via AsciiFolding, "Étterbeek" → ["etterbeek"]
        assert_eq!(analyze(&index, "Étterbeek"), vec!["etterbeek"]);
    }

    #[test]
    fn analyze_splits_on_hyphen() {
        let index = empty_index_with_tokenizer();
        // Hyphen is a hard separator (see indexer's TextAnalyzer config).
        assert_eq!(analyze(&index, "Grote-Ringlaan"), vec!["grote", "ringlaan"]);
    }

    #[test]
    fn analyze_drops_punctuation() {
        let index = empty_index_with_tokenizer();
        // Apostrophes, commas, periods all break tokens.
        assert_eq!(
            analyze(&index, "St. Jean, d'Acre"),
            vec!["st", "jean", "d", "acre"]
        );
    }

    #[test]
    fn empty_slots_no_filters_returns_none() {
        let schema = CrabSchema::build();
        let parsed = ParsedQuery {
            raw_query: String::new(),
            street_tokens: Vec::new(),
            house_number: None,
            postal_code: None,
            ..Default::default()
        };
        let q = build_strict_query(
            searchable_field(&schema),
            &[],
            &parsed,
            &schema,
            &FilterParams::default(),
        );
        assert!(q.is_none(), "no tokens + no filters → no query");
    }

    #[test]
    fn empty_slots_with_postal_filter_builds_filter_only_query() {
        // Edge case: caller passes a postal_code filter and no street tokens.
        // build_strict_query must NOT return None in that case — the filter
        // alone is a legitimate query (e.g. "every doc with postal=1050").
        let schema = CrabSchema::build();
        let parsed = ParsedQuery {
            raw_query: String::new(),
            street_tokens: Vec::new(),
            house_number: None,
            postal_code: None,
            ..Default::default()
        };
        let filters = FilterParams {
            postal_code: Some("1050".into()),
            ..Default::default()
        };
        let q = build_strict_query(searchable_field(&schema), &[], &parsed, &schema, &filters);
        assert!(q.is_some(), "filter alone must produce a query");
    }

    #[test]
    fn single_token_does_not_use_phrase_query() {
        // PhraseQuery refuses single-term inputs (Tantivy invariant). With
        // exactly one slot the builder must skip the phrase branch and rely
        // on the AND-of-OR branch alone — building the phrase would panic
        // at search time.
        let schema = CrabSchema::build();
        let parsed = ParsedQuery {
            raw_query: "louise".into(),
            street_tokens: vec!["louise".into()],
            house_number: None,
            postal_code: None,
            ..Default::default()
        };
        let q = build_strict_query(
            searchable_field(&schema),
            &[vec!["louise".into()]],
            &parsed,
            &schema,
            &FilterParams::default(),
        );
        assert!(q.is_some());
    }

    #[test]
    fn postal_filter_overrides_parsed_postal_code() {
        // Two paths produce a postal-code clause: parser-extracted (from the
        // free-text `q`) and per-request filter. The filter wins. We can't
        // easily inspect a BooleanQuery's contents, but we CAN check both
        // paths build a non-None query without panicking, and that producing
        // *both* doesn't double-clause.
        let schema = CrabSchema::build();
        let parsed = ParsedQuery {
            raw_query: "louise 1000".into(),
            street_tokens: vec!["louise".into()],
            house_number: None,
            postal_code: Some("1000".into()),
            ..Default::default()
        };
        let filters = FilterParams {
            postal_code: Some("1050".into()),
            ..Default::default()
        };
        let q = build_strict_query(
            searchable_field(&schema),
            &[vec!["louise".into()]],
            &parsed,
            &schema,
            &filters,
        );
        assert!(q.is_some());
    }
}

/// Build the fuzzy fallback: OR-of-forms across all slots against the single
/// `searchable` field, still gated by house-number / postal-code / region
/// filters when present. All forms from all slots are flattened into a single
/// OR (deduped).
pub fn build_fuzzy_query(
    field: tantivy::schema::Field,
    slots: &[Vec<String>],
    parsed: &ParsedQuery,
    schema: &CrabSchema,
    filters: &FilterParams,
) -> Option<Box<dyn TantivyQuery>> {
    let mut clauses: Vec<(Occur, Box<dyn TantivyQuery>)> = Vec::new();

    if !slots.is_empty() {
        let mut or_branches: Vec<(Occur, Box<dyn TantivyQuery>)> = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for slot in slots {
            for f in slot {
                if seen.insert(f.clone()) {
                    let term = Term::from_field_text(field, f);
                    let tq: Box<dyn TantivyQuery> =
                        Box::new(TermQuery::new(term, IndexRecordOption::WithFreqs));
                    or_branches.push((Occur::Should, tq));
                }
            }
        }
        let textual: Box<dyn TantivyQuery> = Box::new(BooleanQuery::new(or_branches));
        clauses.push((Occur::Must, textual));
    }

    push_filters(&mut clauses, parsed, schema, filters);

    if clauses.is_empty() {
        return None;
    }
    Some(Box::new(BooleanQuery::new(clauses)))
}

#[cfg(test)]
mod exact_name_tests {
    use super::*;

    fn slots(words: &[&[&str]]) -> Vec<Vec<String>> {
        words
            .iter()
            .map(|forms| forms.iter().map(|f| f.to_string()).collect())
            .collect()
    }

    fn keys(slots: &[Vec<String>]) -> Vec<String> {
        let schema = CrabSchema::build();
        exact_name_clauses(&schema, slots)
            .iter()
            .map(|q| {
                let mut terms = Vec::new();
                q.query_terms(&mut |t, _| terms.push(t.value().as_str().unwrap().to_string()));
                terms.join("|")
            })
            .collect()
    }

    #[test]
    fn every_leading_run_and_abbreviation_form_is_a_key() {
        let k = keys(&slots(&[&["av", "avenue"], &["louise"]]));
        assert_eq!(k, ["av", "av louise", "avenue", "avenue louise"]);
        assert!(keys(&[]).is_empty());
    }

    #[test]
    fn keys_are_capped() {
        let many: Vec<&[&str]> = vec![&["a", "b", "c"]; 6];
        assert_eq!(keys(&slots(&many)).len(), MAX_EXACT_KEYS);
    }
}

#[cfg(test)]
mod fuzzy_tests {
    use super::*;

    #[test]
    fn fuzzy_query_empty_slots_no_filters_returns_none() {
        let schema = CrabSchema::build();
        let parsed = ParsedQuery {
            raw_query: String::new(),
            street_tokens: Vec::new(),
            house_number: None,
            postal_code: None,
            ..Default::default()
        };
        let q = build_fuzzy_query(
            searchable_field(&schema),
            &[],
            &parsed,
            &schema,
            &FilterParams::default(),
        );
        assert!(q.is_none());
    }

    #[test]
    fn fuzzy_query_dedupes_repeated_forms_across_slots() {
        // Two slots both contain "louise" → the OR must keep one branch,
        // not two. The dedup happens via `seen: HashSet<String>` inside
        // build_fuzzy_query; this test guards against accidentally
        // dropping it (which would double-count the term frequency at
        // ranking time).
        let schema = CrabSchema::build();
        let parsed = ParsedQuery {
            raw_query: "louise louise".into(),
            street_tokens: vec!["louise".into(), "louise".into()],
            house_number: None,
            postal_code: None,
            ..Default::default()
        };
        let q = build_fuzzy_query(
            searchable_field(&schema),
            &[vec!["louise".into()], vec!["louise".into()]],
            &parsed,
            &schema,
            &FilterParams::default(),
        );
        assert!(q.is_some());
    }

    #[test]
    fn fuzzy_query_with_only_house_number_filter_is_built() {
        // Caller passed no street tokens but a house_number — the fuzzy
        // builder must still produce a filter-only query (mirrors strict's
        // behaviour and prevents asymmetric None returns).
        let schema = CrabSchema::build();
        let parsed = ParsedQuery {
            raw_query: "32".into(),
            street_tokens: Vec::new(),
            house_number: Some("32".into()),
            postal_code: None,
            ..Default::default()
        };
        let q = build_fuzzy_query(
            searchable_field(&schema),
            &[],
            &parsed,
            &schema,
            &FilterParams::default(),
        );
        assert!(q.is_some());
    }
}

#[cfg(test)]
mod typo_tests {
    use super::*;
    use crabfinder_test_support::{TestIndex, fixtures};

    fn slots(words: &[&str]) -> Vec<Vec<String>> {
        words.iter().map(|w| vec![w.to_string()]).collect()
    }

    #[test]
    fn distance_table() {
        assert_eq!(
            (0..=10).map(typo_distance).collect::<Vec<_>>(),
            [0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2]
        );
    }

    #[test]
    fn unknown_slots_are_those_missing_from_the_vocabulary() {
        let ti = TestIndex::build(&fixtures::louise_street());
        let searcher = ti.reader.searcher();
        let f = ti.schema.searchable;
        assert!(!slot_is_unknown(&searcher, f, &["louise".into()]));
        assert!(slot_is_unknown(&searcher, f, &["loiuse".into()]));
        // One known form is enough (abbreviation expansions).
        assert!(!slot_is_unknown(
            &searcher,
            f,
            &["av".into(), "avenue".into()]
        ));
    }

    #[test]
    fn typo_query_needs_a_correctable_unknown_slot() {
        let schema = CrabSchema::build();
        let f = schema.searchable;
        let p = ParsedQuery::default();
        let filters = FilterParams::default();
        let s = slots(&["avenue", "loiuse"]);
        assert!(build_typo_query(f, &s, &[false, false], &p, &schema, &filters).is_none());
        assert!(build_typo_query(f, &s, &[false, true], &p, &schema, &filters).is_some());
        // "pare" (4 chars) can't be corrected → no typo tier at all.
        let s = slots(&["rue", "pare"]);
        assert!(build_typo_query(f, &s, &[false, true], &p, &schema, &filters).is_none());
        // Two-edit words get both distances.
        let s = slots(&["kloosterstrat"]);
        let q = build_typo_query(f, &s, &[true], &p, &schema, &filters).unwrap();
        let dbg = format!("{q:?}");
        assert_eq!(dbg.matches("FuzzyTermQuery").count(), 2, "{dbg}");
    }
}
