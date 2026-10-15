//! crabfinder-parser — free-text Belgian address splitter.
//!
//! Splits a query like "Rue de la Loi 16 bte 3, B-1040 Etterbeek" into
//! street tokens, house number, box number and postal code. Normalisation
//! of the street words (accent fold, case, hyphen split) is *not* done
//! here — that is the Tantivy "crab" analyzer's job at search time.
//!
//! v2 (Phase 2) rules, applied in order:
//!
//!  1. **Pre-tokenisation.** Split on whitespace, `,` and `;`. Leading and
//!     trailing punctuation is trimmed from each token (`"32."` → `32`),
//!     internal `/` and `-` are kept.
//!  2. **Token rewrites.**
//!     * `B-1050`, `B1050`, `BE-1050` → postal code (explicit, never a
//!       house number).
//!     * `1050-Ixelles` → postal code + street word.
//!     * `12/3` → house number 12, box 3; `12 / 3` likewise.
//!     * `12-14` → house number 12 (first of a range).
//!     * `bte`, `boîte`, `boite`, `bt`, `bus`, `box`, `b` followed by a
//!       box-shaped token → box number. A lone letter right after a bare
//!       number with nothing box-shaped behind it is a suffix (`32 b` →
//!       `32B`).
//!  3. **Number classification.**
//!     * Several 4-digit postal-range tokens: the last is the postal code,
//!       the others are house-number candidates (`Chaussée de Waterloo
//!       1357 1180 Uccle`).
//!     * One 4-digit token (and no explicit postal code): it is the postal
//!       code; when no other house number exists an alternative reading
//!       treats it as the house number.
//!     * Of several house-number candidates the **last** one wins; earlier
//!       ones go back to the street words (`Rue du 11 Novembre 5`).
//!     * A house number sitting between two street words of the same
//!       comma segment gets an alternative reading where it is a street
//!       word (`Rue du 11 Novembre`).
//!  4. House numbers are uppercased (`32a` → `32A`).
//!
//! `alternatives` holds at most [`MAX_ALTERNATIVES`] other readings, best
//! first. They never have alternatives of their own.

#![forbid(unsafe_code)]

/// Upper bound on [`ParsedQuery::alternatives`].
pub const MAX_ALTERNATIVES: usize = 3;

/// Parsed query split.
///
/// `raw_query` keeps the original input verbatim so the service can echo it
/// back in the response.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ParsedQuery {
    pub raw_query: String,
    /// Street tokens in input order, before tokenizer normalization. The
    /// service hands these to the Tantivy "crab" analyzer at search time so
    /// indexer and query path stay in lockstep.
    pub street_tokens: Vec<String>,
    /// Numeric prefix + optional alpha suffix, uppercased (`"32"`, `"32A"`).
    pub house_number: Option<String>,
    /// Box / unit (`bte 3`, `bus 2`, `12/3`), as typed.
    pub box_number: Option<String>,
    /// 4-digit Belgian postal code (1000–9999).
    pub postal_code: Option<String>,
    /// Other plausible readings, best first (≤ [`MAX_ALTERNATIVES`]).
    pub alternatives: Vec<ParsedQuery>,
}

impl ParsedQuery {
    /// Make `alternatives[i]` the primary reading; the old primary takes
    /// its slot. Used when outside information (an explicit postal-code
    /// parameter) makes an alternative the better first guess.
    ///
    /// # Panics
    /// When `i` is out of bounds.
    pub fn promote(&mut self, i: usize) {
        let mut alts = std::mem::take(&mut self.alternatives);
        std::mem::swap(self, &mut alts[i]);
        self.alternatives = alts;
    }
}

/// Lexeme after token rewrites, before number classification.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Lex {
    Word(String),
    /// House-number shaped (`\d{1,4}[A-Za-z]?`), not a postal candidate.
    Num(String),
    /// 4 digits in the postal range: postal code or house number.
    Four(String),
    /// Explicitly marked postal code (`B-1050`, `1050-Ixelles`).
    Postal(String),
    Box(String),
}

/// A pre-token plus whether a `,`/`;` separated it from the previous one.
struct Tok {
    text: String,
    segment: usize,
}

/// Parse a free-text query into structured components.
pub fn parse(q: &str) -> ParsedQuery {
    let toks = pre_tokenize(q);
    let lexes = lex(&toks);
    let mut primary = classify(&lexes, Mode::Primary);
    primary.raw_query = q.to_string();

    let mut alternatives: Vec<ParsedQuery> = Vec::new();
    for mode in [Mode::FourAsHouseNumber, Mode::NumberInStreet] {
        if let Some(mut alt) = classify_alt(&lexes, mode) {
            alt.raw_query = q.to_string();
            if alt != primary && !alternatives.contains(&alt) {
                alternatives.push(alt);
            }
        }
    }
    alternatives.truncate(MAX_ALTERNATIVES);
    primary.alternatives = alternatives;
    primary
}

fn pre_tokenize(q: &str) -> Vec<Tok> {
    let mut out = Vec::new();
    for (segment, part) in q.split([',', ';']).enumerate() {
        for raw in part.split_whitespace() {
            // A lone "/" is kept so `12 / 3` can become a box.
            if raw == "/" {
                out.push(Tok {
                    text: "/".into(),
                    segment,
                });
                continue;
            }
            let t = raw.trim_matches(|c: char| !c.is_alphanumeric());
            if !t.is_empty() {
                out.push(Tok {
                    text: t.to_string(),
                    segment,
                });
            }
        }
    }
    out
}

fn is_postal(s: &str) -> bool {
    s.len() == 4
        && s.bytes().all(|b| b.is_ascii_digit())
        && s.parse::<u32>().is_ok_and(|n| (1000..=9999).contains(&n))
}

/// `\d{1,4}[A-Za-z]?`
fn is_house_number(s: &str) -> bool {
    let digits = s.bytes().take_while(u8::is_ascii_digit).count();
    let rest = &s[digits..];
    (1..=4).contains(&digits)
        && (rest.is_empty() || (rest.len() == 1 && rest.as_bytes()[0].is_ascii_alphabetic()))
}

/// Box values: `3`, `A`, `0.1`, `B12`, `RCH` — short, alphanumeric (+ `.`).
fn is_box_value(s: &str) -> bool {
    let n = s.chars().count();
    (1..=8).contains(&n)
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '.')
        && (s.chars().any(|c| c.is_ascii_digit()) || n <= 3)
}

fn is_box_marker(s: &str) -> bool {
    matches!(
        s.to_lowercase().as_str(),
        "bte" | "boîte" | "boite" | "bt" | "bus" | "box" | "b"
    )
}

/// `B-1050`, `B1050`, `BE-1050`, `BE1050` → `1050`.
fn strip_country_prefix(s: &str) -> Option<&str> {
    let upper = s.to_ascii_uppercase();
    for prefix in ["BE-", "BE", "B-", "B"] {
        if upper.starts_with(prefix) {
            let rest = &s[prefix.len()..];
            if is_postal(rest) {
                return Some(rest);
            }
        }
    }
    None
}

fn number_lex(s: &str) -> Lex {
    if is_postal(s) {
        Lex::Four(s.to_string())
    } else {
        Lex::Num(s.to_string())
    }
}

fn lex(toks: &[Tok]) -> Vec<(Lex, usize)> {
    let mut out: Vec<(Lex, usize)> = Vec::with_capacity(toks.len());
    let mut i = 0;
    while i < toks.len() {
        let t = &toks[i].text;
        let seg = toks[i].segment;
        let next = toks.get(i + 1).filter(|n| n.segment == seg);

        if let Some(pc) = strip_country_prefix(t) {
            out.push((Lex::Postal(pc.to_string()), seg));
        } else if let Some((left, right)) = t.split_once('/')
            && is_house_number(left)
            && is_box_value(right)
        {
            out.push((Lex::Num(left.to_string()), seg));
            out.push((Lex::Box(right.to_string()), seg));
        } else if t == "/" {
            // `12 / 3`: box when it follows a number and precedes a value.
            match (out.last(), next) {
                (Some((Lex::Num(_) | Lex::Four(_), _)), Some(n)) if is_box_value(&n.text) => {
                    out.push((Lex::Box(n.text.clone()), seg));
                    i += 1;
                }
                _ => {}
            }
        } else if let Some((left, right)) = t.split_once('-')
            && is_postal(left)
            && !right.is_empty()
            && right.chars().next().is_some_and(char::is_alphabetic)
        {
            out.push((Lex::Postal(left.to_string()), seg));
            out.push((Lex::Word(right.to_string()), seg));
        } else if let Some((left, right)) = t.split_once('-')
            && is_house_number(left)
            && !right.is_empty()
            && right.bytes().all(|b| b.is_ascii_alphanumeric())
            && right.as_bytes()[0].is_ascii_digit()
        {
            // Range `12-14`: keep the first number.
            out.push((Lex::Num(left.to_string()), seg));
        } else if is_box_marker(t)
            && let Some(n) = next
            && is_box_value(&n.text)
            && !is_postal(&n.text)
            && !matches!(out.last(), None | Some((Lex::Word(_), _)))
        {
            out.push((Lex::Box(n.text.clone()), seg));
            i += 1;
        } else if t.len() == 1
            && t.as_bytes()[0].is_ascii_alphabetic()
            && let Some((last, s)) = out.last_mut()
            && *s == seg
            && let Lex::Num(n) | Lex::Four(n) = last
            && n.bytes().all(|b| b.is_ascii_digit())
            && i > 0
            && toks[i - 1].text == *n
        {
            // `32 b` → `32B` (a suffixed number is never a postal code).
            let merged = format!("{n}{t}");
            *last = Lex::Num(merged);
        } else if is_house_number(t) {
            out.push((number_lex(t), seg));
        } else {
            out.push((Lex::Word(t.clone()), seg));
        }
        i += 1;
    }
    out
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Primary,
    /// A lone 4-digit token is the house number, not the postal code.
    FourAsHouseNumber,
    /// The chosen house number is a street word instead.
    NumberInStreet,
}

/// Index of the lexeme that becomes the house number, plus which `Four`
/// (if any) is the postal code.
struct Roles {
    postal_idx: Option<usize>,
    hn_idx: Option<usize>,
}

fn roles(lexes: &[(Lex, usize)], four_as_hn: bool) -> Roles {
    let explicit_postal = lexes.iter().any(|(l, _)| matches!(l, Lex::Postal(_)));
    let fours: Vec<usize> = lexes
        .iter()
        .enumerate()
        .filter(|(_, (l, _))| matches!(l, Lex::Four(_)))
        .map(|(i, _)| i)
        .collect();
    let postal_idx = if explicit_postal || four_as_hn {
        None
    } else {
        fours.last().copied()
    };
    let hn_idx = lexes
        .iter()
        .enumerate()
        .rev()
        .find(|(i, (l, _))| {
            matches!(l, Lex::Num(_)) || (matches!(l, Lex::Four(_)) && Some(*i) != postal_idx)
        })
        .map(|(i, _)| i);
    Roles { postal_idx, hn_idx }
}

fn build(lexes: &[(Lex, usize)], roles: &Roles, hn_in_street: bool) -> ParsedQuery {
    let mut out = ParsedQuery::default();
    for (i, (l, _)) in lexes.iter().enumerate() {
        match l {
            Lex::Word(w) => out.street_tokens.push(w.clone()),
            Lex::Postal(p) => out.postal_code = Some(p.clone()),
            Lex::Box(b) => out.box_number = Some(b.clone()),
            Lex::Num(n) | Lex::Four(n) => {
                if Some(i) == roles.postal_idx {
                    out.postal_code = Some(n.clone());
                } else if Some(i) == roles.hn_idx && !hn_in_street {
                    out.house_number = Some(n.to_uppercase());
                } else {
                    out.street_tokens.push(n.clone());
                }
            }
        }
    }
    out
}

fn classify(lexes: &[(Lex, usize)], mode: Mode) -> ParsedQuery {
    let r = roles(lexes, mode == Mode::FourAsHouseNumber);
    build(lexes, &r, mode == Mode::NumberInStreet)
}

fn classify_alt(lexes: &[(Lex, usize)], mode: Mode) -> Option<ParsedQuery> {
    let primary = roles(lexes, false);
    match mode {
        Mode::Primary => None,
        Mode::FourAsHouseNumber => {
            // Only when the postal code came from a lone 4-digit token and
            // nothing else could be the house number.
            let pi = primary.postal_idx?;
            let fours = lexes
                .iter()
                .filter(|(l, _)| matches!(l, Lex::Four(_)))
                .count();
            let has_words = lexes.iter().any(|(l, _)| matches!(l, Lex::Word(_)));
            (fours == 1 && primary.hn_idx.is_none() && has_words && pi > 0)
                .then(|| classify(lexes, mode))
        }
        Mode::NumberInStreet => {
            let hi = primary.hn_idx?;
            let seg = lexes[hi].1;
            let word_at = |j: usize| matches!(lexes.get(j), Some((Lex::Word(_), s)) if *s == seg);
            (hi > 0 && word_at(hi - 1) && word_at(hi + 1)).then(|| classify(lexes, mode))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// (street, house_number, box, postal) of the primary reading.
    fn shape(q: &str) -> (String, Option<String>, Option<String>, Option<String>) {
        let r = parse(q);
        (
            r.street_tokens.join(" "),
            r.house_number,
            r.box_number,
            r.postal_code,
        )
    }

    /// input, street, house number, box, postal.
    type Case<'a> = (
        &'a str,
        &'a str,
        Option<&'a str>,
        Option<&'a str>,
        Option<&'a str>,
    );

    fn s(v: &str) -> Option<String> {
        Some(v.to_string())
    }

    #[test]
    fn table() {
        #[rustfmt::skip]
        let cases: &[Case] = &[
            // input, street, hn, box, postal
            ("Avenue Louise", "Avenue Louise", None, None, None),
            ("Avenue Louise 32", "Avenue Louise", Some("32"), None, None),
            ("Avenue Louise 32A", "Avenue Louise", Some("32A"), None, None),
            ("Avenue Louise 32a", "Avenue Louise", Some("32A"), None, None),
            ("Avenue Louise 32 b", "Avenue Louise", Some("32B"), None, None),
            ("Avenue Louise 32 1050", "Avenue Louise", Some("32"), None, Some("1050")),
            ("1050 Avenue Louise", "Avenue Louise", None, None, Some("1050")),
            ("Avenue Louise 32, 1050 Ixelles", "Avenue Louise Ixelles", Some("32"), None, Some("1050")),
            ("Avenue Louise 32. 1050", "Avenue Louise", Some("32"), None, Some("1050")),
            // Postal prefixes
            ("Rue de la Loi 16, B-1040", "Rue de la Loi", Some("16"), None, Some("1040")),
            ("Rue de la Loi 16 B1040", "Rue de la Loi", Some("16"), None, Some("1040")),
            ("Rue de la Loi 16 BE-1040 Etterbeek", "Rue de la Loi Etterbeek", Some("16"), None, Some("1040")),
            ("Avenue Louise 32 1050-Ixelles", "Avenue Louise Ixelles", Some("32"), None, Some("1050")),
            // Box numbers
            ("Rue de la Loi 16 bte 3, B-1040", "Rue de la Loi", Some("16"), Some("3"), Some("1040")),
            ("Rue de la Loi 16 bte. 3", "Rue de la Loi", Some("16"), Some("3"), None),
            ("Rue de la Loi 16 boîte 3", "Rue de la Loi", Some("16"), Some("3"), None),
            ("Rue de la Loi 16 boite A", "Rue de la Loi", Some("16"), Some("A"), None),
            ("Meir 12 bus 2 2000 Antwerpen", "Meir Antwerpen", Some("12"), Some("2"), Some("2000")),
            ("Meir 12 box 0.1", "Meir", Some("12"), Some("0.1"), None),
            ("Meir 12 bt 4", "Meir", Some("12"), Some("4"), None),
            ("Meir 12 b 4", "Meir", Some("12"), Some("4"), None),
            ("Meir 12/3", "Meir", Some("12"), Some("3"), None),
            ("Meir 12 / 3 2000", "Meir", Some("12"), Some("3"), Some("2000")),
            ("Meir 12A/B2", "Meir", Some("12A"), Some("B2"), None),
            // A box marker word that is part of the street name stays.
            ("Rue du Bus 5", "Rue du Bus", Some("5"), None, None),
            ("Box 5", "Box", Some("5"), None, None),
            // Ranges
            ("Meir 12-14", "Meir", Some("12"), None, None),
            ("Meir 12-14 2000", "Meir", Some("12"), None, Some("2000")),
            // A letter after a range or a box is not a suffix.
            ("Meir 12-14 b", "Meir b", Some("12"), None, None),
            // 4-digit house numbers
            ("Chaussée de Waterloo 1357 1180 Uccle", "Chaussée de Waterloo Uccle", Some("1357"), None, Some("1180")),
            ("Chaussée de Waterloo 1357, B-1180 Uccle", "Chaussée de Waterloo Uccle", Some("1357"), None, Some("1180")),
            ("Rue 1234", "Rue", None, None, Some("1234")),
            ("Rue 0999", "Rue", Some("0999"), None, None),
            // Numbers inside street names
            ("Rue du 11 Novembre 5", "Rue du 11 Novembre", Some("5"), None, None),
            ("Rue du 11 Novembre 5, 1040", "Rue du 11 Novembre", Some("5"), None, Some("1040")),
            ("Rue 10 12", "Rue 10", Some("12"), None, None),
            // Odd inputs
            ("", "", None, None, None),
            ("   ,;  ", "", None, None, None),
            ("1050", "", None, None, Some("1050")),
            ("32", "", Some("32"), None, None),
            ("Rue 12345", "Rue 12345", None, None, None),
            ("Grand-Place", "Grand-Place", None, None, None),
            ("Grand-Place 1, 1000 Bruxelles", "Grand-Place Bruxelles", Some("1"), None, Some("1000")),
            ("Wéris (Durbuy)", "Wéris Durbuy", None, None, None),
            ("\"Meir\" 12", "Meir", Some("12"), None, None),
            ("Straße 5 4700 Eupen", "Straße Eupen", Some("5"), None, Some("4700")),
            ("/ 3", "", Some("3"), None, None),
            ("Meir / x", "Meir x", None, None, None),
            ("Waterloo 1357 b 1180", "Waterloo", Some("1357B"), None, Some("1180")),
        ];
        for (q, street, hn, bx, pc) in cases {
            assert_eq!(
                shape(q),
                (
                    street.to_string(),
                    hn.map(String::from),
                    bx.map(String::from),
                    pc.map(String::from)
                ),
                "input: {q:?}"
            );
        }
    }

    #[test]
    fn raw_query_is_kept() {
        assert_eq!(parse(" Meir 1 ").raw_query, " Meir 1 ");
    }

    #[test]
    fn lone_four_digit_gets_house_number_alternative() {
        let r = parse("Chaussée de Waterloo 1357 Uccle");
        assert_eq!(r.postal_code, s("1357"));
        assert_eq!(r.house_number, None);
        assert_eq!(r.alternatives.len(), 1);
        let alt = &r.alternatives[0];
        assert_eq!(alt.house_number, s("1357"));
        assert_eq!(alt.postal_code, None);
        assert_eq!(alt.street_tokens, ["Chaussée", "de", "Waterloo", "Uccle"]);
        assert!(alt.alternatives.is_empty());
    }

    #[test]
    fn no_four_digit_alternative_when_a_house_number_exists() {
        assert!(parse("Avenue Louise 32 1050").alternatives.is_empty());
        // A leading postal code is not a house number.
        assert!(parse("1050 Avenue Louise").alternatives.is_empty());
        // Nor is a query with no street words.
        assert!(parse("1050").alternatives.is_empty());
    }

    #[test]
    fn explicit_postal_turns_four_digit_into_house_number() {
        let r = parse("Chaussée de Waterloo 1357 B-1180");
        assert_eq!(r.house_number, s("1357"));
        assert_eq!(r.postal_code, s("1180"));
    }

    #[test]
    fn number_between_words_gets_street_alternative() {
        let r = parse("Rue du 11 Novembre");
        assert_eq!(r.house_number, s("11"));
        assert_eq!(r.street_tokens, ["Rue", "du", "Novembre"]);
        assert_eq!(r.alternatives.len(), 1);
        let alt = &r.alternatives[0];
        assert_eq!(alt.house_number, None);
        assert_eq!(alt.street_tokens, ["Rue", "du", "11", "Novembre"]);
    }

    #[test]
    fn comma_blocks_the_street_alternative() {
        assert!(parse("Avenue Louise 32, Ixelles").alternatives.is_empty());
        assert_eq!(parse("Avenue Louise 32 Ixelles").alternatives.len(), 1);
    }

    #[test]
    fn both_alternatives_can_apply() {
        // "5" between words → street alternative; lone 4-digit → hn
        // alternative is blocked because "5" is already a house number.
        let r = parse("Rue du 5 Mai 1234 Somewhere");
        assert_eq!(r.house_number, s("5"));
        assert_eq!(r.postal_code, s("1234"));
        assert_eq!(r.alternatives.len(), 1);
        assert!(r.alternatives.len() <= MAX_ALTERNATIVES);
    }

    #[test]
    fn promote_swaps_primary_and_alternative() {
        let mut r = parse("Chaussée de Waterloo 1357 Uccle");
        let alt = r.alternatives[0].clone();
        let mut old = r.clone();
        old.alternatives.clear();
        r.promote(0);
        assert_eq!(r.house_number, alt.house_number);
        assert_eq!(r.alternatives, vec![old]);
    }

    #[test]
    fn helpers() {
        assert!(is_postal("1000") && is_postal("9999"));
        assert!(!is_postal("0999") && !is_postal("123") && !is_postal("12a4"));
        assert!(is_house_number("1") && is_house_number("1234") && is_house_number("12b"));
        assert!(!is_house_number("12345") && !is_house_number("12ab") && !is_house_number("a1"));
        assert!(is_box_value("3") && is_box_value("0.1") && is_box_value("RCH"));
        assert!(!is_box_value("Ixelles") && !is_box_value("") && !is_box_value("a-1"));
        assert_eq!(strip_country_prefix("b-1050"), Some("1050"));
        assert_eq!(strip_country_prefix("Be1050"), Some("1050"));
        assert_eq!(strip_country_prefix("B-0999"), None);
        assert_eq!(strip_country_prefix("Bxl"), None);
        assert!(classify_alt(&[], Mode::Primary).is_none());
    }
}
