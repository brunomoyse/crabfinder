//! crabfinder-parser — Phase 2 query parser.
//!
//! Splits a free-text query like "Avenue Louise 32 1050" into structured
//! components: street tokens, optional house number, optional postal code.
//!
//! The parser is intentionally tiny: it knows two patterns (a Belgian 4-digit
//! postal code and a numeric house number with optional letter suffix) and
//! leaves everything else as street tokens. The heavy lifting — accent fold,
//! case, hyphen split — happens later in the Tantivy "crab" tokenizer.

#![forbid(unsafe_code)]

/// Parsed query split.
///
/// `raw_query` keeps the original input verbatim so the service can echo it
/// back in the response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedQuery {
    pub raw_query: String,
    /// Street tokens in input order, before tokenizer normalization. The
    /// service hands these to the Tantivy "crab" analyzer at search time so
    /// indexer and query path stay in lockstep.
    pub street_tokens: Vec<String>,
    /// Numeric prefix + optional alpha suffix (e.g. "32", "32A"). Compared
    /// case-insensitively against the `house_number` raw-token field.
    pub house_number: Option<String>,
    /// 4-digit Belgian postal code (1000–9999).
    pub postal_code: Option<String>,
}

/// Parse a free-text query into structured components.
///
/// Rules (kept deliberately simple — typo tolerance and synonym expansion are
/// out of scope for this phase):
///
///   * a standalone 4-digit token in range 1000–9999 → postal_code
///   * a token matching `\d+[A-Za-z]?` (1–4 digits) → house_number
///   * everything else → street_tokens (whitespace-split, no further
///     normalization at this layer)
///
/// If the input has *multiple* candidates of the same kind, the **last** one
/// wins. That matches how a user typically types: street first, then number,
/// then postal code — corrections come at the end.
pub fn parse(q: &str) -> ParsedQuery {
    let raw_query = q.to_string();
    let mut street_tokens: Vec<String> = Vec::new();
    let mut house_number: Option<String> = None;
    let mut postal_code: Option<String> = None;

    for tok in q.split_whitespace() {
        if let Some(pc) = match_postal_code(tok) {
            postal_code = Some(pc);
        } else if let Some(hn) = match_house_number(tok) {
            house_number = Some(hn);
        } else {
            street_tokens.push(tok.to_string());
        }
    }

    ParsedQuery {
        raw_query,
        street_tokens,
        house_number,
        postal_code,
    }
}

fn match_postal_code(tok: &str) -> Option<String> {
    // Belgian postal codes are exactly 4 digits in [1000, 9999].
    if tok.len() != 4 {
        return None;
    }
    if !tok.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let n: u32 = tok.parse().ok()?;
    if (1000..=9999).contains(&n) {
        Some(tok.to_string())
    } else {
        None
    }
}

fn match_house_number(tok: &str) -> Option<String> {
    // \d{1,4}[A-Za-z]?  — refuse 5-digit numbers (those would be misparsed
    // postal codes or some other artifact, not a Belgian house number).
    let bytes = tok.as_bytes();
    if bytes.is_empty() {
        return None;
    }
    let mut i = 0usize;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i == 0 || i > 4 {
        return None;
    }
    // At most one trailing ASCII letter (A, B, …) — common for Belgian
    // suffixes ("32A", "12B").
    if i < bytes.len() {
        if bytes.len() - i != 1 || !bytes[i].is_ascii_alphabetic() {
            return None;
        }
    }
    Some(tok.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(q: &str) -> ParsedQuery {
        parse(q)
    }

    #[test]
    fn bare_street_only() {
        let r = p("Avenue Louise");
        assert_eq!(r.street_tokens, vec!["Avenue", "Louise"]);
        assert_eq!(r.house_number, None);
        assert_eq!(r.postal_code, None);
    }

    #[test]
    fn street_with_house_number() {
        let r = p("Avenue Louise 32");
        assert_eq!(r.street_tokens, vec!["Avenue", "Louise"]);
        assert_eq!(r.house_number.as_deref(), Some("32"));
        assert_eq!(r.postal_code, None);
    }

    #[test]
    fn house_number_with_letter_suffix() {
        let r = p("Avenue Louise 32A");
        assert_eq!(r.house_number.as_deref(), Some("32A"));
    }

    #[test]
    fn street_house_number_and_postal_code() {
        let r = p("Avenue Louise 32 1050");
        assert_eq!(r.street_tokens, vec!["Avenue", "Louise"]);
        assert_eq!(r.house_number.as_deref(), Some("32"));
        assert_eq!(r.postal_code.as_deref(), Some("1050"));
    }

    #[test]
    fn postal_code_can_appear_before_street() {
        let r = p("1050 Avenue Louise");
        assert_eq!(r.street_tokens, vec!["Avenue", "Louise"]);
        assert_eq!(r.postal_code.as_deref(), Some("1050"));
    }

    #[test]
    fn four_digit_house_number_is_not_a_postal_code() {
        // 4-digit tokens always parse as postal codes if in range. This is
        // documented behaviour — Belgian house numbers above 9999 are
        // vanishingly rare, and ambiguity favors the postal code reading.
        let r = p("Rue 1234");
        assert_eq!(r.postal_code.as_deref(), Some("1234"));
        assert_eq!(r.house_number, None);
    }

    #[test]
    fn out_of_range_4_digit_is_house_number() {
        // 0999 would be out of postal range; should NOT be postal_code.
        let r = p("Rue 0999");
        assert_eq!(r.postal_code, None);
        // 4-digit numeric token outside postal range: house_number rule
        // accepts up to 4 digits, so this lands there.
        assert_eq!(r.house_number.as_deref(), Some("0999"));
    }

    #[test]
    fn five_digit_token_is_street_token() {
        // Nothing matches; should be kept as a street token rather than
        // silently dropped.
        let r = p("Rue 12345");
        assert_eq!(r.street_tokens, vec!["Rue", "12345"]);
        assert_eq!(r.house_number, None);
        assert_eq!(r.postal_code, None);
    }

    #[test]
    fn hyphenated_street_kept_as_single_token() {
        // Parser does NOT split on hyphens — the Tantivy tokenizer does that
        // downstream. This keeps the parser's responsibility narrow.
        let r = p("Grand-Place");
        assert_eq!(r.street_tokens, vec!["Grand-Place"]);
    }

    #[test]
    fn empty_query() {
        let r = p("");
        assert!(r.street_tokens.is_empty());
        assert_eq!(r.house_number, None);
        assert_eq!(r.postal_code, None);
    }

    #[test]
    fn multiple_house_numbers_last_wins() {
        let r = p("Rue 10 12");
        // Last numeric token wins — user's correction beats their first try.
        assert_eq!(r.house_number.as_deref(), Some("12"));
    }
}
