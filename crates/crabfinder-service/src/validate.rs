//! Address validation (v3 Phase 11.2): compare what the caller wrote with
//! the BeSt address the resolver lands on, and say what differs.
//!
//! `assess` is pure over an already-resolved `ResolvedAddress` (the same
//! top-1 decision `POST /batch/geocode` makes), so `/validate` and the
//! batch `corrections` columns agree by construction. Words are compared
//! in `crab`-analyzer form (`query_builder::analyze`): case and accents
//! never count as a correction.
//!
//! Every official value comes from the BeSt payload; nothing here invents
//! or stores an address.

use std::collections::HashSet;

use serde::Serialize;
use tantivy::Index;

use crabfinder_core::{Address, Language, MultilingualName};

use crate::abbreviation::AbbreviationMap;
use crate::query_builder::{analyze, typo_distance};
use crate::resolver::{
    AddressQuery, Confidence, ResolvedAddress, ResolverContext, SearchOptions, normalize_box,
    resolve_with,
};

/// The verdict on one input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// The address exists as written (case, accents and completions such
    /// as a missing postal code aside).
    Valid,
    /// It exists once corrected: see the corrections.
    Corrected,
    /// Two near-equal answers on different streets: the input doesn't
    /// say which (`CandidateSet::ambiguity`).
    Ambiguous,
    /// Only a street or a place matched, not an address (no house
    /// number, or one BeSt doesn't have on that street).
    Incomplete,
    /// Nothing matched.
    NotFound,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Valid => "valid",
            Status::Corrected => "corrected",
            Status::Ambiguous => "ambiguous",
            Status::Incomplete => "incomplete",
            Status::NotFound => "not_found",
        }
    }
}

/// What kind of difference a `Correction` reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CorrectionKind {
    /// The postal code given isn't the address's (`adjacent_commune`, or
    /// a code that doesn't match the street).
    PostalCodeChanged,
    /// No postal code was given: here is BeSt's. A completion.
    PostalCodeAdded,
    /// A street word misspelt (edit distance, as the typo tier).
    StreetSpelling,
    /// An abbreviation written out (`av` → `avenue`).
    AbbreviationExpanded,
    /// Words of the official street name are missing from the input.
    StreetCompleted,
    /// The street was written in another language than the one asked
    /// for (`lang`). A completion.
    StreetLanguage,
    /// An input word that matches nothing in the address.
    WordIgnored,
    /// A municipality / locality word misspelt.
    MunicipalitySpelling,
    /// The city given names none of the address's municipality, postal
    /// area or part of municipality.
    MunicipalityChanged,
    /// The number given isn't on the street: the nearest one is.
    HouseNumberApproximated,
    /// The number given isn't on the street; only the street matched.
    HouseNumberNotFound,
    /// No box was given and the building has a single unit, with a box.
    /// A completion.
    BoxAdded,
    /// The box given isn't one of the building's.
    BoxNotFound,
    /// The free text was read another way than first parsed (a 4-digit
    /// number as the house number rather than the postal code…).
    Reading,
}

impl CorrectionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PostalCodeChanged => "postal_code_changed",
            Self::PostalCodeAdded => "postal_code_added",
            Self::StreetSpelling => "street_spelling",
            Self::AbbreviationExpanded => "abbreviation_expanded",
            Self::StreetCompleted => "street_completed",
            Self::StreetLanguage => "street_language",
            Self::WordIgnored => "word_ignored",
            Self::MunicipalitySpelling => "municipality_spelling",
            Self::MunicipalityChanged => "municipality_changed",
            Self::HouseNumberApproximated => "house_number_approximated",
            Self::HouseNumberNotFound => "house_number_not_found",
            Self::BoxAdded => "box_added",
            Self::BoxNotFound => "box_not_found",
            Self::Reading => "reading",
        }
    }

    /// Completions add what the input left out without contradicting it:
    /// they don't make a valid address `corrected`.
    pub fn is_completion(self) -> bool {
        matches!(
            self,
            Self::PostalCodeAdded | Self::StreetLanguage | Self::BoxAdded
        )
    }
}

/// One difference between the input and the official address.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Correction {
    /// `street`, `city`, `postal_code`, `house_number`, `box` or `query`.
    pub field: &'static str,
    pub kind: CorrectionKind,
    /// What the input said (analyzed form for words); absent when the
    /// input said nothing.
    pub input: Option<String>,
    /// What BeSt says; absent when nothing replaces the input.
    pub official: Option<String>,
}

impl Correction {
    fn new(
        field: &'static str,
        kind: CorrectionKind,
        input: Option<impl Into<String>>,
        official: Option<impl Into<String>>,
    ) -> Self {
        Self {
            field,
            kind,
            input: input.map(Into::into),
            official: official.map(Into::into),
        }
    }
}

/// `assess`'s answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Validation {
    pub status: Status,
    pub corrections: Vec<Correction>,
}

impl Validation {
    /// The batch `corrections` column: `kind:field:input->official`
    /// joined by `; ` (empty sides left empty).
    pub fn corrections_column(&self) -> String {
        self.corrections
            .iter()
            .map(|c| {
                format!(
                    "{}:{}:{}->{}",
                    c.kind.as_str(),
                    c.field,
                    c.input.as_deref().unwrap_or(""),
                    c.official.as_deref().unwrap_or("")
                )
            })
            .collect::<Vec<_>>()
            .join("; ")
    }
}

/// `resolve_with`, then `assess` the answer into `ResolvedAddress::validation`.
pub fn resolve_and_assess(
    query: &AddressQuery,
    opts: &SearchOptions,
    ctx: &ResolverContext<'_>,
    lang: Option<Language>,
) -> ResolvedAddress {
    let mut r = resolve_with(query, opts, ctx);
    r.validation = Some(assess(query, &r, ctx.index, &ctx.abbrev, lang));
    r
}

/// Compare `query` (as given, alternatives included) with the address
/// `r` resolved it to. `lang` is the language the caller wants names in
/// (`street_language`); `None` = each address's own.
pub fn assess(
    query: &AddressQuery,
    r: &ResolvedAddress,
    index: &Index,
    abbrev: &AbbreviationMap,
    lang: Option<Language>,
) -> Validation {
    let Some(a) = r
        .address
        .as_ref()
        .filter(|_| r.confidence != Confidence::NotFound)
    else {
        return Validation {
            status: Status::NotFound,
            corrections: Vec::new(),
        };
    };
    let q = match r.reading {
        0 => query,
        n => query.alternatives.get(n - 1).unwrap_or(query),
    };
    let mut out = Vec::new();
    if !std::ptr::eq(q, query) {
        out.push(Correction::new(
            "query",
            CorrectionKind::Reading,
            Some(describe(query)),
            Some(describe(q)),
        ));
    }
    // A locality or landmark answer is the BeSt address nearest to the
    // place the words name: nothing to compare them with.
    if matches!(r.confidence, Confidence::Locality | Confidence::Landmark) {
        return Validation {
            status: if r.ambiguous {
                Status::Ambiguous
            } else {
                Status::Incomplete
            },
            corrections: out,
        };
    }
    let address_level = r.house_number_delta.is_some();
    Words::new(a, index, abbrev).compare(q, lang, &mut out);
    match &q.postal_code {
        Some(pc) if *pc != a.postal_code => out.push(Correction::new(
            "postal_code",
            CorrectionKind::PostalCodeChanged,
            Some(pc.as_str()),
            Some(a.postal_code.as_str()),
        )),
        Some(_) => {}
        None => out.push(Correction::new(
            "postal_code",
            CorrectionKind::PostalCodeAdded,
            None::<String>,
            Some(a.postal_code.as_str()),
        )),
    }
    if let Some(hn) = &q.house_number {
        match r.house_number_delta {
            Some(_) if !hn.eq_ignore_ascii_case(&a.house_number) => out.push(Correction::new(
                "house_number",
                CorrectionKind::HouseNumberApproximated,
                Some(hn.as_str()),
                Some(a.house_number.as_str()),
            )),
            Some(_) => {}
            None => out.push(Correction::new(
                "house_number",
                CorrectionKind::HouseNumberNotFound,
                Some(hn.as_str()),
                None::<String>,
            )),
        }
    }
    if address_level {
        let official_box = a.box_number.as_deref().filter(|b| !b.is_empty());
        match (&q.box_number, official_box) {
            (Some(b), o) if o.map(normalize_box) != Some(normalize_box(b)) => out.push(
                Correction::new("box", CorrectionKind::BoxNotFound, Some(b.as_str()), o),
            ),
            (None, Some(o)) if r.hit.as_ref().is_some_and(|h| h.unit_count == 1) => out.push(
                Correction::new("box", CorrectionKind::BoxAdded, None::<String>, Some(o)),
            ),
            _ => {}
        }
    }
    let status = if r.ambiguous {
        Status::Ambiguous
    } else if !address_level {
        Status::Incomplete
    } else if r.confidence == Confidence::Exact && out.iter().all(|c| c.kind.is_completion()) {
        Status::Valid
    } else {
        Status::Corrected
    };
    Validation {
        status,
        corrections: out,
    }
}

/// A reading on one line: street words, number, postal code.
fn describe(q: &AddressQuery) -> String {
    let mut s = q.street_tokens.join(" ");
    if let Some(hn) = &q.house_number {
        s.push(' ');
        s.push_str(hn);
    }
    if let Some(pc) = &q.postal_code {
        s.push_str(", ");
        s.push_str(pc);
    }
    s.trim().to_string()
}

/// The address's names in analyzer form.
struct Words<'a> {
    address: &'a Address,
    abbrev: &'a AbbreviationMap,
    /// Street name per language.
    street: Vec<(Language, Vec<String>)>,
    /// Municipality, postal area and part-of-municipality names, every
    /// language.
    place: Vec<String>,
}

impl<'a> Words<'a> {
    fn new(address: &'a Address, index: &Index, abbrev: &'a AbbreviationMap) -> Self {
        let street = address
            .street_name
            .iter()
            .map(|(l, s)| (l, analyze(index, s)))
            .collect();
        let mut seen = HashSet::new();
        let place = [
            Some(&address.municipality_name),
            Some(&address.postal_name),
            address.part_of_municipality_name.as_ref(),
        ]
        .into_iter()
        .flatten()
        .flat_map(MultilingualName::iter)
        .flat_map(|(_, s)| analyze(index, s))
        .filter(|t| seen.insert(t.clone()))
        .collect();
        Self {
            address,
            abbrev,
            street,
            place,
        }
    }

    /// The street name the input words match best (ties: the address's
    /// own language), with how many words matched.
    fn best_street(&self, words: &[String]) -> Option<(usize, &(Language, Vec<String>))> {
        let own = self.address.default_language();
        self.street
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let hits = words
                    .iter()
                    .filter(|w| self.abbrev.expand(w).iter().any(|f| s.1.contains(f)))
                    .count();
                ((hits, s.0 == own, usize::MAX - i), s)
            })
            .max_by_key(|(k, _)| *k)
            .map(|((hits, ..), s)| (hits, s))
    }

    fn compare(&self, q: &AddressQuery, lang: Option<Language>, out: &mut Vec<Correction>) {
        let split = q.street_tokens.len() - q.city_tokens.min(q.street_tokens.len());
        let (street_words, city_words) = q.street_tokens.split_at(split);
        if let Some((hits, (street_lang, official))) = self.best_street(street_words) {
            let mut covered = vec![false; official.len()];
            let mut cover = |w: &str| {
                let at = (0..official.len())
                    .find(|&i| !covered[i] && official[i] == w)
                    .or_else(|| official.iter().position(|o| o == w));
                if let Some(i) = at {
                    covered[i] = true;
                }
            };
            for w in street_words {
                if official.contains(w) {
                    cover(w);
                } else if self.place.contains(w) {
                    // A locality word in the street part of the input.
                } else if let Some(e) = self
                    .abbrev
                    .expand(w)
                    .into_iter()
                    .skip(1)
                    .find(|e| official.contains(e))
                {
                    out.push(Correction::new(
                        "street",
                        CorrectionKind::AbbreviationExpanded,
                        Some(w.as_str()),
                        Some(e.as_str()),
                    ));
                    cover(&e);
                } else if self.street.iter().any(|(_, s)| s.contains(w)) {
                    // A word of the name in another language.
                } else if let Some(o) = closest(w, official) {
                    out.push(Correction::new(
                        "street",
                        CorrectionKind::StreetSpelling,
                        Some(w.as_str()),
                        Some(o),
                    ));
                    cover(o);
                } else if let Some(p) = closest(w, &self.place) {
                    out.push(Correction::new(
                        "city",
                        CorrectionKind::MunicipalitySpelling,
                        Some(w.as_str()),
                        Some(p),
                    ));
                } else {
                    out.push(Correction::new(
                        "query",
                        CorrectionKind::WordIgnored,
                        Some(w.as_str()),
                        None::<String>,
                    ));
                }
            }
            let name = |l: Language| self.address.street_name.get(l).map(str::to_string);
            if !street_words.is_empty() && covered.iter().any(|c| !c) {
                out.push(Correction::new(
                    "street",
                    CorrectionKind::StreetCompleted,
                    Some(street_words.join(" ")),
                    name(*street_lang),
                ));
            }
            if let Some(l) = lang
                && l != *street_lang
                && hits > 0
                && let Some(wanted) = name(l)
            {
                out.push(Correction::new(
                    "street",
                    CorrectionKind::StreetLanguage,
                    name(*street_lang),
                    Some(wanted),
                ));
            }
        }
        self.compare_city(city_words, lang, out);
    }

    /// The structured `city` words against the address's place names.
    fn compare_city(&self, words: &[String], lang: Option<Language>, out: &mut Vec<Correction>) {
        if words.is_empty() || words.iter().any(|w| self.place.contains(w)) {
            return;
        }
        let fixes: Vec<(&String, &str)> = words
            .iter()
            .filter_map(|w| closest(w, &self.place).map(|p| (w, p)))
            .collect();
        if fixes.len() == words.len() {
            for (w, p) in fixes {
                out.push(Correction::new(
                    "city",
                    CorrectionKind::MunicipalitySpelling,
                    Some(w.as_str()),
                    Some(p),
                ));
            }
            return;
        }
        let a = self.address;
        let lang = lang.unwrap_or_else(|| a.default_language());
        let pick = |n: &MultilingualName| n.get(lang).or_else(|| n.primary()).map(str::to_string);
        out.push(Correction::new(
            "city",
            CorrectionKind::MunicipalityChanged,
            Some(words.join(" ")),
            pick(&a.postal_name).or_else(|| pick(&a.municipality_name)),
        ));
    }
}

/// The candidate within the typo tier's edit distance of `word`
/// (`typo_distance`), closest first; `None` for words too short to
/// correct.
fn closest<'c>(word: &str, candidates: &'c [String]) -> Option<&'c str> {
    let max = typo_distance(word.chars().count()) as usize;
    if max == 0 {
        return None;
    }
    candidates
        .iter()
        .map(|c| (osa_distance(word, c), c))
        .filter(|(d, _)| *d <= max)
        .min_by_key(|(d, _)| *d)
        .map(|(_, c)| c.as_str())
}

/// Optimal-string-alignment distance (Levenshtein plus adjacent
/// transpositions), the measure behind Tantivy's fuzzy terms.
fn osa_distance(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut d = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    d[0] = (0..=b.len()).collect();
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            d[i][j] = (d[i - 1][j] + 1)
                .min(d[i][j - 1] + 1)
                .min(d[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                d[i][j] = d[i][j].min(d[i - 2][j - 2] + 1);
            }
        }
    }
    d[a.len()][b.len()]
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::sync::Arc;

    use super::*;
    use crate::addr_count::AddressCountIndex;
    use crate::dedup::MatchType;
    use crate::hydrate::HydratedHit;
    use crate::landmarks::LandmarkIndex;
    use crate::localities::LocalityIndex;
    use crate::mid_street::MidStreetIndex;
    use crabfinder_core::Region;
    use crabfinder_test_support::{AddressBuilder, TestIndex};

    /// Avenue Louise 10/20/32 (32: two boxes) in 1050 Ixelles, Rue de la
    /// Loi 16 (one unit, box 3) in 1000, Rue de Fer 25 in 5000 Namur.
    fn addresses() -> Vec<Address> {
        let louise = |oid: &str, hn: &str, lat: f64| {
            AddressBuilder::new(oid, "3919", hn)
                .street(Some("Avenue Louise"), Some("Louizalaan"))
                .municipality("21009", Some("Ixelles"), Some("Elsene"), None)
                .postal("1050")
                .postal_name(Some("Ixelles"), Some("Elsene"))
                .at(lat, 4.3567)
        };
        vec![
            louise("l10", "10", 50.8301).build(),
            louise("l20", "20", 50.8302).build(),
            louise("l32a", "32", 50.8303).box_no("1").build(),
            louise("l32b", "32", 50.8303).box_no("2").build(),
            AddressBuilder::new("w16", "100", "16")
                .street(Some("Rue de la Loi"), Some("Wetstraat"))
                .box_no("3")
                .at(50.8453, 4.3658)
                .build(),
            AddressBuilder::new("f25", "6001", "25")
                .region(Region::Wallonia)
                .street(Some("Rue de Fer"), None)
                .municipality("92094", Some("Namur"), None, None)
                .postal("5000")
                .part_of_municipality("p1", Some("Namur"), None)
                .at(50.4660, 4.8660)
                .build(),
        ]
    }

    fn abbreviations(index: &Index) -> AbbreviationMap {
        let mut f = tempfile::Builder::new().suffix(".yaml").tempfile().unwrap();
        f.write_all(
            b"version: v1\nentries:\n  - canonical: avenue\n    abbreviations: [av]\n    language: fr\n    kind: standalone\n",
        )
        .unwrap();
        AbbreviationMap::load(f.path(), index).unwrap()
    }

    fn ctx(ti: &TestIndex) -> ResolverContext<'_> {
        ResolverContext {
            index: &ti.index,
            reader: &ti.reader,
            schema: &ti.schema,
            abbrev: Arc::new(abbreviations(&ti.index)),
            addr_counts: Arc::new(AddressCountIndex::build(&ti.reader, &ti.schema).unwrap()),
            notability_enabled: false,
            mid_street: Arc::new(MidStreetIndex::empty()),
            localities: Arc::new(LocalityIndex::empty()),
            landmarks: Arc::new(LandmarkIndex::empty()),
        }
    }

    /// Structured input: street and city words, number, box, postal code.
    fn input(
        ti: &TestIndex,
        street: &str,
        city: Option<&str>,
        hn: Option<&str>,
        bx: Option<&str>,
        pc: Option<&str>,
    ) -> AddressQuery {
        let mut street_tokens = analyze(&ti.index, street);
        let city_tokens = city.map_or(Vec::new(), |c| analyze(&ti.index, c));
        let n = city_tokens.len();
        street_tokens.extend(city_tokens);
        AddressQuery {
            street_tokens,
            house_number: hn.map(Into::into),
            box_number: bx.map(Into::into),
            postal_code: pc.map(Into::into),
            city_tokens: n,
            ..Default::default()
        }
    }

    fn run(ti: &TestIndex, q: &AddressQuery, lang: Option<Language>) -> Validation {
        let r = resolve_and_assess(q, &SearchOptions::default(), &ctx(ti), lang);
        r.validation.unwrap()
    }

    fn kinds(v: &Validation) -> Vec<(&'static str, &'static str, Option<&str>, Option<&str>)> {
        v.corrections
            .iter()
            .map(|c| {
                (
                    c.kind.as_str(),
                    c.field,
                    c.input.as_deref(),
                    c.official.as_deref(),
                )
            })
            .collect()
    }

    #[test]
    fn an_address_as_written_is_valid_whatever_the_case_and_accents() {
        let ti = TestIndex::build(&addresses());
        for street in ["Avenue Louise", "AVENUE LOUISE", "Avénue Louise"] {
            let v = run(
                &ti,
                &input(&ti, street, Some("Ixelles"), Some("32"), None, Some("1050")),
                None,
            );
            assert_eq!((v.status, kinds(&v)), (Status::Valid, vec![]), "{street}");
        }
        // Dutch name and city: still valid, both are official.
        let v = run(
            &ti,
            &input(
                &ti,
                "Louizalaan",
                Some("Elsene"),
                Some("32"),
                None,
                Some("1050"),
            ),
            None,
        );
        assert_eq!((v.status, kinds(&v)), (Status::Valid, vec![]));
    }

    #[test]
    fn completions_keep_an_address_valid() {
        let ti = TestIndex::build(&addresses());
        // No postal code, no box on a one-unit building, another language.
        let v = run(
            &ti,
            &input(&ti, "Wetstraat", None, Some("16"), None, None),
            Some(Language::Fr),
        );
        assert_eq!(v.status, Status::Valid);
        assert_eq!(
            kinds(&v),
            [
                (
                    "street_language",
                    "street",
                    Some("Wetstraat"),
                    Some("Rue de la Loi")
                ),
                ("postal_code_added", "postal_code", None, Some("1000")),
                ("box_added", "box", None, Some("3")),
            ]
        );
        assert!(kinds(&v).iter().all(|k| {
            CorrectionKind::is_completion(
                v.corrections
                    .iter()
                    .find(|c| c.kind.as_str() == k.0)
                    .unwrap()
                    .kind,
            )
        }));
    }

    #[test]
    fn street_words_are_spelt_expanded_and_completed() {
        let ti = TestIndex::build(&addresses());
        let pc = Some("1050");
        let v = run(
            &ti,
            &input(&ti, "Av Louise", None, Some("32"), None, pc),
            None,
        );
        assert_eq!(v.status, Status::Corrected);
        assert_eq!(
            kinds(&v),
            [(
                "abbreviation_expanded",
                "street",
                Some("av"),
                Some("avenue")
            )]
        );
        let v = run(
            &ti,
            &input(&ti, "Avenue Louisse", None, Some("32"), None, pc),
            None,
        );
        assert_eq!(
            (v.status, kinds(&v)),
            (
                Status::Corrected,
                vec![("street_spelling", "street", Some("louisse"), Some("louise"))]
            )
        );
        let v = run(&ti, &input(&ti, "Louise", None, Some("32"), None, pc), None);
        assert_eq!(
            kinds(&v),
            [(
                "street_completed",
                "street",
                Some("louise"),
                Some("Avenue Louise")
            )]
        );
        // A free-text locality word is fine, an unknown one is reported.
        let v = run(
            &ti,
            &input(
                &ti,
                "Avenue Louise Ixelles Belgique",
                None,
                Some("32"),
                None,
                pc,
            ),
            None,
        );
        assert_eq!(
            kinds(&v),
            [("word_ignored", "query", Some("belgique"), None)]
        );
        let v = run(
            &ti,
            &input(&ti, "Avenue Louise Ixeles", None, Some("32"), None, pc),
            None,
        );
        assert_eq!(
            kinds(&v),
            [(
                "municipality_spelling",
                "city",
                Some("ixeles"),
                Some("ixelles")
            )]
        );
    }

    #[test]
    fn the_city_field_is_checked_against_the_place_names() {
        let ti = TestIndex::build(&addresses());
        let q = |city| {
            input(
                &ti,
                "Rue de Fer",
                Some(city),
                Some("25"),
                None,
                Some("5000"),
            )
        };
        assert_eq!(run(&ti, &q("Namur"), None).status, Status::Valid);
        let v = run(&ti, &q("Namuur"), None);
        assert_eq!(
            kinds(&v),
            [(
                "municipality_spelling",
                "city",
                Some("namuur"),
                Some("namur")
            )]
        );
        let v = run(&ti, &q("Charleroi"), None);
        assert_eq!(
            kinds(&v),
            [(
                "municipality_changed",
                "city",
                Some("charleroi"),
                Some("Namur")
            )]
        );
        assert_eq!(v.status, Status::Corrected);
    }

    #[test]
    fn numbers_and_boxes() {
        let ti = TestIndex::build(&addresses());
        let pc = Some("1050");
        // 30 isn't on the street: the nearest number is.
        let v = run(
            &ti,
            &input(&ti, "Avenue Louise", None, Some("30"), None, pc),
            None,
        );
        assert_eq!(v.status, Status::Corrected);
        assert_eq!(kinds(&v)[0].0, "house_number_approximated");
        assert_eq!(kinds(&v)[0].2, Some("30"));
        // Box 2 exists, box 7 doesn't; no box on a two-unit building is fine.
        let v = run(
            &ti,
            &input(&ti, "Avenue Louise", None, Some("32"), Some("02"), pc),
            None,
        );
        assert_eq!((v.status, kinds(&v)), (Status::Valid, vec![]));
        let v = run(
            &ti,
            &input(&ti, "Avenue Louise", None, Some("32"), Some("7"), pc),
            None,
        );
        assert_eq!(kinds(&v)[0].0, "box_not_found");
        assert_eq!(kinds(&v)[0].2, Some("7"));
        // Without a number only the street matched.
        let v = run(
            &ti,
            &input(&ti, "Avenue Louise", None, None, None, pc),
            None,
        );
        assert_eq!((v.status, kinds(&v)), (Status::Incomplete, vec![]));
    }

    #[test]
    fn postal_code_changed_and_not_found() {
        let ti = TestIndex::build(&addresses());
        let v = run(&ti, &input(&ti, "xyzzy", None, Some("1"), None, None), None);
        assert_eq!((v.status, kinds(&v)), (Status::NotFound, vec![]));
        // A hand-made decision: the address found outside the code given.
        let a = addresses().remove(0);
        let mut r = ResolvedAddress::not_found();
        r.address = Some(a);
        r.confidence = Confidence::AdjacentCommune;
        r.house_number_delta = Some(0);
        let q = input(&ti, "Avenue Louise", None, Some("10"), None, Some("1000"));
        let v = assess(&q, &r, &ti.index, &abbreviations(&ti.index), None);
        assert_eq!(
            (v.status, kinds(&v)),
            (
                Status::Corrected,
                vec![(
                    "postal_code_changed",
                    "postal_code",
                    Some("1000"),
                    Some("1050")
                )]
            )
        );
        // The same street-level: the number wasn't found.
        r.house_number_delta = None;
        let v = assess(&q, &r, &ti.index, &abbreviations(&ti.index), None);
        assert_eq!(v.status, Status::Incomplete);
        assert_eq!(
            kinds(&v)[1],
            ("house_number_not_found", "house_number", Some("10"), None)
        );
    }

    #[test]
    fn readings_ambiguity_and_places() {
        let ti = TestIndex::build(&addresses());
        let abbrev = abbreviations(&ti.index);
        let a = addresses().remove(0);
        let mut r = ResolvedAddress::not_found();
        r.address = Some(a);
        r.confidence = Confidence::Exact;
        r.house_number_delta = Some(0);
        r.hit = Some(HydratedHit {
            lat: 0.0,
            lon: 0.0,
            score: 1.0,
            match_type: MatchType::Strict,
            unit_count: 1,
            building_group_id: String::new(),
            payload: serde_json::Value::Null,
            distance_m: None,
        });
        // The second reading won: "1050" was the postal code after all.
        let mut q = input(&ti, "Avenue Louise", None, Some("1050"), None, None);
        q.alternatives = vec![input(
            &ti,
            "Avenue Louise",
            None,
            Some("10"),
            None,
            Some("1050"),
        )];
        r.reading = 1;
        let v = assess(&q, &r, &ti.index, &abbrev, None);
        assert_eq!(v.status, Status::Corrected);
        assert_eq!(
            kinds(&v),
            [(
                "reading",
                "query",
                Some("avenue louise 1050"),
                Some("avenue louise 10, 1050")
            )]
        );
        r.ambiguous = true;
        assert_eq!(
            assess(&q, &r, &ti.index, &abbrev, None).status,
            Status::Ambiguous
        );
        // A place answer: nothing to compare.
        r.reading = 0;
        r.ambiguous = false;
        r.confidence = Confidence::Locality;
        let v = assess(
            &input(&ti, "Spontin", None, None, None, None),
            &r,
            &ti.index,
            &abbrev,
            None,
        );
        assert_eq!((v.status, kinds(&v)), (Status::Incomplete, vec![]));
        r.ambiguous = true;
        let v = assess(
            &input(&ti, "Spontin", None, None, None, None),
            &r,
            &ti.index,
            &abbrev,
            None,
        );
        assert_eq!(v.status, Status::Ambiguous);
    }

    #[test]
    fn wire_strings_and_column() {
        for (s, w) in [
            (Status::Valid, "valid"),
            (Status::Corrected, "corrected"),
            (Status::Ambiguous, "ambiguous"),
            (Status::Incomplete, "incomplete"),
            (Status::NotFound, "not_found"),
        ] {
            assert_eq!(s.as_str(), w);
            assert_eq!(serde_json::to_value(s).unwrap(), w);
        }
        use CorrectionKind::*;
        for k in [
            PostalCodeChanged,
            PostalCodeAdded,
            StreetSpelling,
            AbbreviationExpanded,
            StreetCompleted,
            StreetLanguage,
            WordIgnored,
            MunicipalitySpelling,
            MunicipalityChanged,
            HouseNumberApproximated,
            HouseNumberNotFound,
            BoxAdded,
            BoxNotFound,
            Reading,
        ] {
            assert_eq!(serde_json::to_value(k).unwrap(), k.as_str());
        }
        let v = Validation {
            status: Status::Corrected,
            corrections: vec![
                Correction::new("street", AbbreviationExpanded, Some("av"), Some("avenue")),
                Correction::new("postal_code", PostalCodeAdded, None::<String>, Some("1050")),
            ],
        };
        assert_eq!(
            v.corrections_column(),
            "abbreviation_expanded:street:av->avenue; postal_code_added:postal_code:->1050"
        );
    }

    #[test]
    fn edit_distance() {
        assert_eq!(osa_distance("louise", "louise"), 0);
        assert_eq!(osa_distance("louisse", "louise"), 1);
        assert_eq!(osa_distance("lousie", "louise"), 1, "transposition");
        assert_eq!(osa_distance("", "abc"), 3);
        let c = ["louise".to_string(), "louvain".to_string()];
        assert_eq!(closest("louisse", &c), Some("louise"));
        assert_eq!(closest("loi", &c), None, "too short to correct");
        assert_eq!(closest("xxxxxxx", &c), None);
    }
}
