//! Intra-batch dédup + parallel resolution.
//!
//! Per `docs/design-batch-geocoding.md` §3.5: if the input CSV has 50k rows
//! but only 12k distinct `(street_lower, house_number, postal_code)` triplets,
//! we resolve 12k times and broadcast — never breaks the 1-in-1-out contract.
//!
//! The actual `resolve()` call goes through a `Resolver` trait so tests can
//! inject a stub without spinning up a real Tantivy index. Production wires
//! the trait to `crate::resolver::resolve()` via a closure adapter (see
//! `ClosureResolver`).

use std::collections::HashMap;

use rayon::prelude::*;

use crate::query_builder::analyze;
use crate::resolver::{AddressQuery, Confidence, ResolvedAddress};

use super::io::{InputRow, OutputRow};

/// Indirection layer between the dédup pipeline and the actual resolver.
/// Production passes a closure that captures `ResolverContext`; tests pass
/// a stub.
///
/// Implementations must be `Sync` because rayon farms out resolution across
/// threads. The resolver's underlying types (Tantivy `Searcher`, `Arc`-d
/// dictionaries) are already `Sync`, so this is mechanical.
pub trait Resolver: Sync {
    fn resolve(&self, q: &AddressQuery) -> ResolvedAddress;
}

/// Helper that adapts an arbitrary `Fn(&AddressQuery) -> ResolvedAddress`
/// into a `Resolver`. Lets the façades (HTTP, CLI) build a closure that
/// captures their `ResolverContext` borrow without inventing a struct.
#[allow(dead_code)] // consumed by Lots E/F when the façades land
pub struct ClosureResolver<F: Fn(&AddressQuery) -> ResolvedAddress + Sync>(pub F);

impl<F: Fn(&AddressQuery) -> ResolvedAddress + Sync> Resolver for ClosureResolver<F> {
    fn resolve(&self, q: &AddressQuery) -> ResolvedAddress {
        (self.0)(q)
    }
}

/// Cache key for dédup. We normalise `street` to lowercase so that
/// "Avenue Louise" and "avenue louise" resolve once (the resolver itself
/// lowercases internally via the tokenizer, so this is purely a dédup
/// optimisation, not a correctness boundary).
type CacheKey = (String, Option<String>, Option<String>, Option<String>);

fn cache_key(row: &InputRow) -> CacheKey {
    (
        row.street.to_lowercase(),
        row.house_number.clone(),
        row.postal_code.clone(),
        row.box_number.clone(),
    )
}

/// The `AddressQuery` for one CSV row. The house-number column goes
/// through the free-text parser so `12/3`, `12 bte 3` or `12a` are split
/// into number + box and normalised like `/geocode` input; anything the
/// parser can't read as a bare number is passed through as-is.
pub fn row_query(index: &tantivy::Index, row: &InputRow) -> AddressQuery {
    let (house_number, parsed_box) = match row.house_number.as_deref() {
        Some(raw) => {
            let p = crabfinder_parser::parse(raw);
            if p.street_tokens.is_empty() && p.postal_code.is_none() && p.house_number.is_some() {
                (p.house_number, p.box_number)
            } else {
                (Some(raw.trim().to_string()), None)
            }
        }
        None => (None, None),
    };
    AddressQuery {
        street_tokens: analyze(index, &row.street),
        house_number,
        box_number: row.box_number.clone().or(parsed_box),
        postal_code: row.postal_code.clone(),
        ..Default::default()
    }
}

/// Given a closure mapping `InputRow` to its analyzed `AddressQuery`,
/// resolve every unique triplet in `rows` once (in parallel via rayon)
/// and propagate the result to all input rows that share the triplet.
///
/// Returns the output rows in the same order as `rows`, one output per
/// input. Order preservation is the 1-in-1-out contract from
/// `design-batch-geocoding.md` §3.1.
///
/// The two closure params let tests stub the resolver without a real
/// Tantivy index, and let production avoid coupling the tokenizer step
/// (which needs `&Index`) into this module.
pub fn dedup_then_resolve<R, MakeQuery>(
    rows: Vec<InputRow>,
    resolver: &R,
    make_query: MakeQuery,
) -> Vec<OutputRow>
where
    R: Resolver,
    MakeQuery: Fn(&InputRow) -> AddressQuery + Sync,
{
    if rows.is_empty() {
        return Vec::new();
    }

    // 1) Identify unique triplets, in deterministic order. We use a Vec
    //    (not a HashSet) so the par_iter below sees stable iteration order
    //    — useful when debugging.
    let mut unique_keys: Vec<CacheKey> = Vec::new();
    let mut seen: HashMap<CacheKey, ()> = HashMap::new();
    for row in &rows {
        let k = cache_key(row);
        if seen.insert(k.clone(), ()).is_none() {
            unique_keys.push(k);
        }
    }

    // 2) Build a mapping from CacheKey to the first InputRow that bears
    //    it. Resolution happens against that representative, then the
    //    decision is broadcast.
    let mut key_to_rep: HashMap<CacheKey, &InputRow> = HashMap::with_capacity(unique_keys.len());
    for row in &rows {
        key_to_rep.entry(cache_key(row)).or_insert(row);
    }

    // 3) Resolve uniques in parallel. The result is a `Vec<(key, decision)>`
    //    that we collect into a HashMap for O(1) lookup during broadcast.
    let resolved_pairs: Vec<(CacheKey, ResolvedAddress)> = unique_keys
        .par_iter()
        .map(|key| {
            let rep = key_to_rep
                .get(key)
                .expect("every unique key has a representative");
            let q = make_query(rep);
            let r = resolver.resolve(&q);
            (key.clone(), r)
        })
        .collect();
    let cache: HashMap<CacheKey, ResolvedAddress> = resolved_pairs.into_iter().collect();

    // 4) Broadcast. One OutputRow per InputRow, preserving order.
    rows.into_iter()
        .map(|row| {
            let key = cache_key(&row);
            // SAFETY-ish: we just populated `cache` from `unique_keys`,
            // which was derived from `rows` — every row's key is present.
            let resolved = cache
                .get(&key)
                .expect("dédup cache must contain every row's key")
                .clone();
            output_row_from(row, resolved)
        })
        .collect()
}

/// Map a `(InputRow, ResolvedAddress)` pair into the wire-shape `OutputRow`.
/// Lives here (not in `io.rs`) because it bridges the resolver types.
pub fn output_row_from(input: InputRow, resolved: ResolvedAddress) -> OutputRow {
    let confidence = resolved.confidence.as_str().to_string();
    let fallback_reason = resolved.fallback_reason.map(|r| r.as_str().to_string());
    let (lat, lng) = match resolved.hit.as_ref() {
        Some(h) => (Some(h.lat), Some(h.lon)),
        None => (None, None),
    };
    let building_group_id = resolved.hit.as_ref().map(|h| h.building_group_id.clone());

    let (matched_street, matched_house_number, matched_postal_code, best_id) =
        match resolved.address.as_ref() {
            Some(a) => (
                Some(
                    a.street_name
                        .fr
                        .clone()
                        .or(a.street_name.nl.clone())
                        .unwrap_or_default(),
                ),
                Some(a.house_number.clone()),
                Some(a.postal_code.clone()),
                Some(a.id.to_string()),
            ),
            None => (None, None, None, None),
        };

    // `street_match_score` is 0.0 for NotFound by construction in resolver;
    // emit empty rather than "0" in that case so the CSV stays unambiguous.
    let street_match_score = match resolved.confidence {
        Confidence::NotFound => None,
        _ => Some(resolved.street_match_score),
    };

    OutputRow {
        id: input.id,
        street: input.street,
        house_number: input.house_number,
        postal_code: input.postal_code,
        matched_street,
        matched_house_number,
        matched_postal_code,
        lat,
        lng,
        confidence,
        fallback_reason,
        house_number_delta: resolved.house_number_delta,
        street_match_score,
        building_group_id,
        best_id,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolver::{FallbackReason, ResolvedAddress};
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Test stub: records each call. `resolve_fn` produces the verdict from
    /// the query alone, so two identical AddressQuery values get the same
    /// verdict deterministically.
    struct CountingResolver {
        calls: AtomicUsize,
        resolve_fn: Box<dyn Fn(&AddressQuery) -> ResolvedAddress + Sync>,
    }

    impl CountingResolver {
        fn new<F: Fn(&AddressQuery) -> ResolvedAddress + Sync + 'static>(f: F) -> Self {
            Self {
                calls: AtomicUsize::new(0),
                resolve_fn: Box::new(f),
            }
        }
        fn call_count(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    impl Resolver for CountingResolver {
        fn resolve(&self, q: &AddressQuery) -> ResolvedAddress {
            self.calls.fetch_add(1, Ordering::SeqCst);
            (self.resolve_fn)(q)
        }
    }

    fn make_input(id: &str, street: &str, hn: Option<&str>, pc: Option<&str>) -> InputRow {
        InputRow {
            id: id.to_string(),
            street: street.to_string(),
            house_number: hn.map(str::to_string),
            postal_code: pc.map(str::to_string),
            box_number: None,
        }
    }

    fn id_make_query(row: &InputRow) -> AddressQuery {
        AddressQuery {
            // Mirror the production tokenization by just splitting on
            // whitespace + lowercasing — good enough for the stub-resolver
            // tests.
            street_tokens: row
                .street
                .split_whitespace()
                .map(|s| s.to_lowercase())
                .collect(),
            house_number: row.house_number.clone(),
            postal_code: row.postal_code.clone(),
            ..Default::default()
        }
    }

    #[test]
    fn preserves_input_order_and_count_with_dedup() {
        // 5 rows, 2 pairs of duplicates → 3 unique triplets.
        let rows = vec![
            make_input("1", "Avenue Louise", Some("32"), Some("1050")),
            make_input("2", "Rue Belliard", Some("71"), Some("1040")),
            make_input("3", "AVENUE LOUISE", Some("32"), Some("1050")), // dup of #1
            make_input("4", "Grand-Place", None, Some("1000")),
            make_input("5", "Rue Belliard", Some("71"), Some("1040")), // dup of #2
        ];
        let resolver = CountingResolver::new(|_q| ResolvedAddress {
            address: None,
            hit: None,
            confidence: Confidence::NotFound,
            house_number_delta: None,
            street_match_score: 0.0,
            fallback_reason: Some(FallbackReason::NoMatch),
            // Inject the input street into a field so we can verify which
            // unique key each row mapped to — except we can't extend the
            // resolver shape here, so we lean on the order-preservation check
            // below.
        });
        // Tag each unique resolution so we can assert ordering downstream.
        // We reuse the closure-via-stub mechanism: side-effect-free Fn,
        // the CountingResolver counts calls.
        let _ = q_to_id; // silence warning if unused
        let out = dedup_then_resolve(rows, &resolver, id_make_query);
        assert_eq!(out.len(), 5, "1-in-1-out broken");
        assert_eq!(out[0].id, "1");
        assert_eq!(out[1].id, "2");
        assert_eq!(out[2].id, "3");
        assert_eq!(out[3].id, "4");
        assert_eq!(out[4].id, "5");
        assert_eq!(
            resolver.call_count(),
            3,
            "should resolve 3 unique triplets, not 5"
        );
    }

    /// Helper used to assert which triplet a row resolved to, if ever
    /// needed. Currently unused but kept for future tests.
    fn q_to_id(q: &AddressQuery) -> String {
        format!(
            "{}|{:?}|{:?}",
            q.street_tokens.join(" "),
            q.house_number,
            q.postal_code
        )
    }

    #[test]
    fn empty_input_returns_empty_output() {
        let resolver = CountingResolver::new(|_| ResolvedAddress {
            address: None,
            hit: None,
            confidence: Confidence::NotFound,
            house_number_delta: None,
            street_match_score: 0.0,
            fallback_reason: Some(FallbackReason::NoMatch),
        });
        let out = dedup_then_resolve(vec![], &resolver, id_make_query);
        assert!(out.is_empty());
        assert_eq!(resolver.call_count(), 0);
    }

    #[test]
    fn all_unique_rows_resolve_once_each() {
        let rows = vec![
            make_input("1", "Avenue Louise", Some("32"), Some("1050")),
            make_input("2", "Rue Belliard", Some("71"), Some("1040")),
            make_input("3", "Grand-Place", None, Some("1000")),
        ];
        let resolver = CountingResolver::new(|_| ResolvedAddress {
            address: None,
            hit: None,
            confidence: Confidence::NotFound,
            house_number_delta: None,
            street_match_score: 0.0,
            fallback_reason: Some(FallbackReason::NoMatch),
        });
        let out = dedup_then_resolve(rows, &resolver, id_make_query);
        assert_eq!(out.len(), 3);
        assert_eq!(resolver.call_count(), 3);
    }

    #[test]
    fn output_row_carries_confidence_and_fallback_reason() {
        use crate::dedup::MatchType;
        use crate::hydrate::HydratedHit;

        let row = make_input("42", "Av Louise", Some("99999"), Some("1050"));
        let resolved = ResolvedAddress {
            address: None,
            hit: Some(HydratedHit {
                lat: 50.83,
                lon: 4.36,
                score: 12.5,
                match_type: MatchType::Strict,
                unit_count: 1,
                building_group_id: "ns|S|99999".into(),
                payload: serde_json::json!({}),
                distance_m: None,
            }),
            confidence: Confidence::MedianStreet,
            house_number_delta: None,
            street_match_score: 12.5,
            fallback_reason: Some(FallbackReason::HouseNumberOutOfRange),
        };
        let out = output_row_from(row, resolved);
        assert_eq!(out.id, "42");
        assert_eq!(out.confidence, "median_street");
        assert_eq!(
            out.fallback_reason.as_deref(),
            Some("house_number_out_of_range")
        );
        assert_eq!(out.lat, Some(50.83));
        assert_eq!(out.street_match_score, Some(12.5));
        assert_eq!(out.house_number_delta, None);
    }

    #[test]
    fn not_found_row_emits_empty_coords() {
        let row = make_input("9", "Unknown Street", Some("1"), Some("1000"));
        let resolved = ResolvedAddress {
            address: None,
            hit: None,
            confidence: Confidence::NotFound,
            house_number_delta: None,
            street_match_score: 0.0,
            fallback_reason: Some(FallbackReason::NoMatch),
        };
        let out = output_row_from(row, resolved);
        assert_eq!(out.confidence, "not_found");
        assert_eq!(out.lat, None);
        assert_eq!(out.lng, None);
        assert_eq!(out.street_match_score, None);
        assert_eq!(out.best_id, None);
    }

    #[test]
    fn box_number_is_part_of_the_cache_key() {
        let r = CountingResolver::new(|_| ResolvedAddress::not_found());
        let mut a = make_input("1", "Meir", Some("12"), Some("2000"));
        let mut b = a.clone();
        b.id = "2".into();
        a.box_number = Some("1".into());
        b.box_number = Some("2".into());
        let c = InputRow {
            id: "3".into(),
            ..a.clone()
        };
        let out = dedup_then_resolve(vec![a, b, c], &r, id_make_query);
        assert_eq!(out.len(), 3);
        assert_eq!(r.calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn row_query_parses_the_house_number_column() {
        let ti = crabfinder_test_support::TestIndex::build(&[]);
        let row = |hn: Option<&str>, bx: Option<&str>| InputRow {
            box_number: bx.map(str::to_string),
            ..make_input("1", "Avenue Louise", hn, Some("1050"))
        };
        let q = row_query(&ti.index, &row(Some("12/3"), None));
        assert_eq!(q.street_tokens, ["avenue", "louise"]);
        assert_eq!(q.house_number.as_deref(), Some("12"));
        assert_eq!(q.box_number.as_deref(), Some("3"));
        assert_eq!(q.postal_code.as_deref(), Some("1050"));

        let q = row_query(&ti.index, &row(Some("12a bte 4"), None));
        assert_eq!(q.house_number.as_deref(), Some("12A"));
        assert_eq!(q.box_number.as_deref(), Some("4"));

        // The explicit box column wins over one parsed from the number.
        let q = row_query(&ti.index, &row(Some("12/3"), Some("7")));
        assert_eq!(q.box_number.as_deref(), Some("7"));

        // Something the parser can't read as a number passes through.
        let q = row_query(&ti.index, &row(Some(" 12345 "), None));
        assert_eq!(q.house_number.as_deref(), Some("12345"));
        let q = row_query(&ti.index, &row(Some("1050"), None));
        assert_eq!(q.house_number.as_deref(), Some("1050"));

        let q = row_query(&ti.index, &row(None, Some("2")));
        assert_eq!(q.house_number, None);
        assert_eq!(q.box_number.as_deref(), Some("2"));
    }
}
