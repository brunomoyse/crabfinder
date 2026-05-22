//! Query-time street-type abbreviation expansion.
//!
//! Loads `data/street-type-abbreviations-v1.yaml`, builds a lookup that maps
//! tokenizer-form tokens (after the same `crab` analyzer the index uses) to
//! a set of canonical forms. The query handler calls [`AbbreviationMap::expand`]
//! on each query token; the result is the set of forms that should be OR'd
//! into that token's slot in the strict AND-of-tokens query.
//!
//! Bidirectional analysis: BeSt is ~99.5% canonical, so we only need
//! query-time expansion (user-abbrev → BeSt-full-form). No index-time
//! expansion is required.
//!
//! Discrimination by `kind`:
//!   - `standalone` — match only when the WHOLE token equals the abbrev
//!     (e.g. "av" → ["av", "avenue"]).
//!   - `suffix` — match only when the token is strictly longer than the
//!     abbrev AND ends with it (e.g. "stationsstr" → ["stationsstr",
//!     "stationsstraat"]).
//!   - `both` — apply both rules.
//!
//! Abbreviations and canonical forms are passed through the `crab` analyzer
//! at load time so the lookup is byte-equal to query-side tokens (lowercase,
//! ASCII-folded, no punctuation).

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;
use tantivy::Index;
use tantivy::tokenizer::TextAnalyzer;
use tracing::{info, warn};

use crabfinder_indexer::index::TOKENIZER_NAME;

#[derive(Debug, Deserialize)]
struct YamlEntry {
    canonical: String,
    abbreviations: Vec<String>,
    #[serde(default)]
    #[allow(dead_code)] // informational; the engine applies all entries regardless
    language: String,
    kind: String,
    #[serde(default)]
    also_match: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    rationale: String,
}

#[derive(Debug, Deserialize)]
struct YamlFile {
    version: String,
    entries: Vec<YamlEntry>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Standalone,
    Suffix,
    Both,
}

pub struct AbbreviationMap {
    /// Whole-token exact match. Key is the analyzed abbreviation form
    /// (e.g. "av"); value is the set of analyzed canonical forms (e.g.
    /// ["avenue"]).
    standalone: HashMap<String, Vec<String>>,
    /// Suffix rules. Each entry: (analyzed abbreviation suffix, analyzed
    /// canonical replacement(s)). Applied when a token is strictly longer
    /// than the abbreviation AND ends with it.
    suffix_rules: Vec<(String, Vec<String>)>,
}

impl AbbreviationMap {
    /// Empty map — `expand(t) == [t]` always. Used when no
    /// `--abbreviation-dict` flag is provided.
    pub fn empty() -> Self {
        Self {
            standalone: HashMap::new(),
            suffix_rules: Vec::new(),
        }
    }

    pub fn load(path: &Path, index: &Index) -> Result<Self> {
        let body = fs::read_to_string(path)
            .with_context(|| format!("reading abbreviation dictionary {path:?}"))?;
        let parsed: YamlFile = serde_yaml::from_str(&body)
            .with_context(|| format!("parsing {path:?} as YAML"))?;
        if parsed.version != "v1" {
            warn!(version = %parsed.version, "abbreviation dict version != v1");
        }

        let analyzer = index
            .tokenizers()
            .get(TOKENIZER_NAME)
            .expect("crab tokenizer must be registered before loading abbreviations");

        let mut standalone: HashMap<String, Vec<String>> = HashMap::new();
        let mut suffix_rules: Vec<(String, Vec<String>)> = Vec::new();

        for e in &parsed.entries {
            let kind = match e.kind.as_str() {
                "standalone" => Kind::Standalone,
                "suffix" => Kind::Suffix,
                "both" => Kind::Both,
                other => {
                    warn!(canonical = %e.canonical, kind = %other, "unknown kind; entry skipped");
                    continue;
                }
            };
            // Canonical targets: pass the canonical + also_match through the
            // analyzer so the lookup output is byte-equal to query-side tokens.
            let mut canonical_forms: Vec<String> = Vec::new();
            for tok in analyze(&analyzer, &e.canonical) {
                canonical_forms.push(tok);
            }
            if let Some(am) = &e.also_match {
                for tok in analyze(&analyzer, am) {
                    canonical_forms.push(tok);
                }
            }
            canonical_forms.sort();
            canonical_forms.dedup();
            if canonical_forms.is_empty() {
                warn!(canonical = %e.canonical, "canonical did not yield any tokens; entry skipped");
                continue;
            }

            for abbr in &e.abbreviations {
                let abbr_tokens = analyze(&analyzer, abbr);
                if abbr_tokens.len() != 1 {
                    warn!(
                        abbrev = %abbr,
                        n_tokens = abbr_tokens.len(),
                        "abbreviation did not analyze to exactly one token; skipped"
                    );
                    continue;
                }
                let abbr_tok = abbr_tokens.into_iter().next().unwrap();
                match kind {
                    Kind::Standalone => {
                        standalone
                            .entry(abbr_tok)
                            .or_default()
                            .extend(canonical_forms.iter().cloned());
                    }
                    Kind::Suffix => {
                        suffix_rules.push((abbr_tok, canonical_forms.clone()));
                    }
                    Kind::Both => {
                        standalone
                            .entry(abbr_tok.clone())
                            .or_default()
                            .extend(canonical_forms.iter().cloned());
                        suffix_rules.push((abbr_tok, canonical_forms.clone()));
                    }
                }
            }
        }

        // Dedup standalone targets in-place.
        for v in standalone.values_mut() {
            v.sort();
            v.dedup();
        }

        info!(
            ?path,
            n_entries = parsed.entries.len(),
            n_standalone = standalone.len(),
            n_suffix = suffix_rules.len(),
            "abbreviation dictionary loaded"
        );

        Ok(Self { standalone, suffix_rules })
    }

    /// Expand a single analyzer-form token to the set of forms that should
    /// be OR'd into that token's query slot. The returned vector always
    /// starts with the original token. Identity expansion (`[token]`) when
    /// no rule matches.
    pub fn expand(&self, token: &str) -> Vec<String> {
        let mut out: Vec<String> = vec![token.to_string()];
        if let Some(cans) = self.standalone.get(token) {
            for c in cans {
                if !out.iter().any(|x| x == c) {
                    out.push(c.clone());
                }
            }
        }
        for (abbr, canonicals) in &self.suffix_rules {
            if token.len() > abbr.len() && token.ends_with(abbr.as_str()) {
                let prefix = &token[..token.len() - abbr.len()];
                for c in canonicals {
                    let expanded = format!("{prefix}{c}");
                    if !out.iter().any(|x| *x == expanded) {
                        out.push(expanded);
                    }
                }
            }
        }
        out
    }
}

fn analyze(analyzer: &TextAnalyzer, text: &str) -> Vec<String> {
    let mut a = analyzer.clone();
    let mut tokens = Vec::new();
    let mut stream = a.token_stream(text);
    while let Some(t) = stream.next() {
        tokens.push(t.text.clone());
    }
    tokens
}

#[cfg(test)]
mod tests {
    use super::*;
    use crabfinder_indexer::index::{CrabSchema, register_tokenizers};
    use std::io::Write;
    use tantivy::Index;

    fn test_index() -> Index {
        let schema = CrabSchema::build();
        let index = Index::create_in_ram(schema.schema.clone());
        register_tokenizers(&index);
        index
    }

    fn write_tmp_yaml(body: &str) -> tempfile::NamedTempFile {
        let mut f = tempfile::Builder::new()
            .suffix(".yaml")
            .tempfile()
            .unwrap();
        f.write_all(body.as_bytes()).unwrap();
        f
    }

    fn load_test_map() -> AbbreviationMap {
        // A small but representative subset of the production dictionary.
        let yaml = r#"
version: v1
entries:
  - canonical: avenue
    abbreviations: [av, avn]
    language: fr
    kind: standalone
  - canonical: boulevard
    abbreviations: [bd, bld, bdv, boul]
    language: fr
    kind: standalone
  - canonical: chaussée
    abbreviations: [ch, chsee, chee]
    language: fr
    kind: standalone
  - canonical: place
    abbreviations: [pl]
    language: fr
    kind: standalone
  - canonical: straat
    abbreviations: [str]
    language: nl
    kind: suffix
  - canonical: plein
    abbreviations: [pl]
    language: nl
    kind: suffix
  - canonical: straße
    abbreviations: [str]
    language: de
    kind: both
    also_match: strasse
  - canonical: platz
    abbreviations: [pl]
    language: de
    kind: both
"#;
        let f = write_tmp_yaml(yaml);
        AbbreviationMap::load(f.path(), &test_index()).unwrap()
    }

    #[test]
    fn empty_map_is_identity() {
        let m = AbbreviationMap::empty();
        assert_eq!(m.expand("anything"), vec!["anything"]);
    }

    #[test]
    fn av_expands_to_avenue() {
        let m = load_test_map();
        let exp = m.expand("av");
        assert!(exp.iter().any(|x| x == "av"));
        assert!(exp.iter().any(|x| x == "avenue"));
    }

    #[test]
    fn avn_expands_to_avenue() {
        let m = load_test_map();
        let exp = m.expand("avn");
        assert!(exp.contains(&"avenue".to_string()));
    }

    #[test]
    fn bd_bld_boul_expand_to_boulevard() {
        let m = load_test_map();
        for abbr in ["bd", "bld", "boul"] {
            let exp = m.expand(abbr);
            assert!(
                exp.iter().any(|x| x == "boulevard"),
                "{abbr} did not expand to boulevard; got {exp:?}"
            );
        }
    }

    #[test]
    fn chaussee_variants_expand() {
        let m = load_test_map();
        // The crab analyzer ASCII-folds "chsée" → "chsee".
        for abbr in ["ch", "chsee", "chee"] {
            let exp = m.expand(abbr);
            // chaussée → analyzed → "chaussee"
            assert!(
                exp.iter().any(|x| x == "chaussee"),
                "{abbr} did not expand to chaussee; got {exp:?}"
            );
        }
    }

    #[test]
    fn pl_expands_to_place_and_platz_not_plein() {
        let m = load_test_map();
        // "pl" as a standalone token must hit FR Place (standalone) and DE
        // Platz (both → standalone branch). NL plein is `suffix` only and
        // must NOT fire on the standalone "pl" token.
        let exp = m.expand("pl");
        assert!(exp.contains(&"place".to_string()), "missing place; got {exp:?}");
        assert!(exp.contains(&"platz".to_string()), "missing platz; got {exp:?}");
        assert!(!exp.contains(&"plein".to_string()), "plein leaked into standalone; got {exp:?}");
    }

    #[test]
    fn stationsstr_expands_to_stationsstraat_and_stationsstrasse() {
        let m = load_test_map();
        let exp = m.expand("stationsstr");
        assert!(
            exp.contains(&"stationsstraat".to_string()),
            "missing stationsstraat in {exp:?}"
        );
        // DE Straße (kind=both) also matches via suffix; analyzer folds ß→ss.
        // The canonical "straße" itself folds to "strasse", and also_match
        // adds "strasse" (dedup makes them equal). So we expect "stationsstrasse".
        assert!(
            exp.contains(&"stationsstrasse".to_string()),
            "missing stationsstrasse in {exp:?}"
        );
    }

    #[test]
    fn rathauspl_expands_to_rathausplatz_via_de_suffix() {
        let m = load_test_map();
        let exp = m.expand("rathauspl");
        assert!(
            exp.contains(&"rathausplatz".to_string()),
            "missing rathausplatz; got {exp:?}"
        );
    }

    #[test]
    fn str_standalone_expands_for_de() {
        // German "Lütticher Str." → token "str" → standalone match on DE Straße (kind=both).
        let m = load_test_map();
        let exp = m.expand("str");
        // After analyzer fold: ["str", "strasse"]
        assert!(exp.contains(&"strasse".to_string()), "missing strasse; got {exp:?}");
    }

    #[test]
    fn non_abbreviation_token_returns_identity() {
        let m = load_test_map();
        assert_eq!(m.expand("louise"), vec!["louise"]);
        assert_eq!(m.expand("eupen"), vec!["eupen"]);
        assert_eq!(m.expand("antwerpen"), vec!["antwerpen"]);
    }
}
