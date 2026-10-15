//! Pure Tantivy query construction.
//!
//! Splits out of `main.rs` so both the HTTP `/geocode` handler and the batch
//! `resolve()` core consume the same builders. Nothing in this module touches
//! HTTP, CSV, or CLI surfaces — it only knows about Tantivy schema + queries
//! and the parsed query shape.

use std::sync::Arc;

use tantivy::collector::TopDocs;
use tantivy::query::{
    BooleanQuery, BoostQuery, Occur, PhraseQuery, Query as TantivyQuery, TermQuery,
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
        // No boost on AND-tokens — let phrase out-rank it naturally.
        text_branches.push((Occur::Should, and_q));

        let textual: Box<dyn TantivyQuery> = Box::new(BooleanQuery::new(text_branches));
        clauses.push((Occur::Must, textual));
    }

    if let Some(hn) = &parsed.house_number {
        let term = Term::from_field_text(schema.house_number, hn);
        let tq: Box<dyn TantivyQuery> = Box::new(TermQuery::new(term, IndexRecordOption::Basic));
        clauses.push((Occur::Must, tq));
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

    if clauses.is_empty() {
        return None;
    }
    Some(Box::new(BooleanQuery::new(clauses)))
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
        };
        let filters = FilterParams {
            region: None,
            postal_code: Some("1050".into()),
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
        };
        let filters = FilterParams {
            region: None,
            postal_code: Some("1050".into()),
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

    if let Some(hn) = &parsed.house_number {
        let term = Term::from_field_text(schema.house_number, hn);
        let tq: Box<dyn TantivyQuery> = Box::new(TermQuery::new(term, IndexRecordOption::Basic));
        clauses.push((Occur::Must, tq));
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

    if clauses.is_empty() {
        return None;
    }
    Some(Box::new(BooleanQuery::new(clauses)))
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
