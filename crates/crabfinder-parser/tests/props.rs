//! Property and snapshot tests for `parse`.

use crabfinder_parser::{MAX_ALTERNATIVES, ParsedQuery, parse};
use proptest::prelude::*;

/// Every output piece must come from the input (case-insensitive), so the
/// parser never invents text.
fn check_provenance(q: &str, r: &ParsedQuery) {
    let lower = q.to_lowercase();
    // `32 b` (or `32 - b`) merges tokens across dropped punctuation, so
    // compare alphanumerics only.
    let alnum = |s: &str| -> String { s.chars().filter(|c| c.is_alphanumeric()).collect() };
    let squashed = alnum(&lower);
    for t in &r.street_tokens {
        assert!(
            squashed.contains(&alnum(&t.to_lowercase())),
            "{t:?} not in {q:?}"
        );
    }
    for v in [&r.house_number, &r.box_number, &r.postal_code]
        .into_iter()
        .flatten()
    {
        let digits: String = v.chars().take_while(char::is_ascii_digit).collect();
        assert!(lower.contains(&digits.to_lowercase()), "{v:?} not in {q:?}");
    }
}

fn token() -> impl Strategy<Value = String> {
    prop_oneof![
        "[A-Za-zéèàçß]{1,10}",
        "[0-9]{1,5}",
        "[0-9]{1,4}[a-zA-Z]",
        "[0-9]{1,3}/[0-9A-Z]{1,2}",
        "[0-9]{1,3}-[0-9]{1,3}",
        "B-?[0-9]{4}",
        Just("bte".to_string()),
        Just("bus".to_string()),
        Just("b".to_string()),
        Just("/".to_string()),
        Just(",".to_string()),
        "[[:punct:]]{1,2}",
    ]
}

proptest! {
    #[test]
    fn never_panics_on_arbitrary_text(q in "\\PC{0,60}") {
        let r = parse(&q);
        prop_assert!(r.alternatives.len() <= MAX_ALTERNATIVES);
    }

    #[test]
    fn address_like_inputs_are_well_formed(toks in prop::collection::vec(token(), 0..10)) {
        let q = toks.join(" ");
        let r = parse(&q);
        check_provenance(&q, &r);
        prop_assert!(r.alternatives.len() <= MAX_ALTERNATIVES);
        if let Some(pc) = &r.postal_code {
            prop_assert_eq!(pc.len(), 4);
        }
        if let Some(hn) = &r.house_number {
            prop_assert_eq!(hn, &hn.to_uppercase());
        }
        for alt in &r.alternatives {
            check_provenance(&q, alt);
            prop_assert!(alt.alternatives.is_empty());
            prop_assert_ne!(alt, &r);
            prop_assert_eq!(&alt.raw_query, &q);
        }
    }
}

#[test]
fn alternatives_snapshot() {
    let queries = [
        "Chaussée de Waterloo 1357 Uccle",
        "Rue du 11 Novembre",
        "Avenue Louise 32 Ixelles",
        "Rue de la Loi 16 bte 3, B-1040 Etterbeek",
        "Meir 12/3 2000 Antwerpen",
    ];
    let out: Vec<ParsedQuery> = queries.iter().map(|q| parse(q)).collect();
    insta::assert_debug_snapshot!(out);
}
