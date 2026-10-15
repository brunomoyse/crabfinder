//! Eval set types + loaders (file 14 §4).
//!
//! Two on-disk formats:
//!   - SyntheticEvalSet — JSON, machine-generated (file 14 §2)
//!   - AdversarialEvalSet — YAML, hand-edited (file 14 §3)
//!
//! Both fold into a unified `EvalSet` enum the runner iterates over.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

// --- Synthetic --------------------------------------------------------------

/// Variant kind generated per sample. MVP started at five (file 14 §2);
/// `Abbreviated` was added during Phase 6 §3.1 follow-up to exercise the
/// service-side street-type expansion (file 12 §3.3.a).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VariantType {
    Canonical,
    Minimal,
    WithPostal,
    CrossLanguage,
    Casing,
    Abbreviated,
}

impl VariantType {
    pub fn as_str(self) -> &'static str {
        match self {
            VariantType::Canonical => "canonical",
            VariantType::Minimal => "minimal",
            VariantType::WithPostal => "with_postal",
            VariantType::CrossLanguage => "cross_language",
            VariantType::Casing => "casing",
            VariantType::Abbreviated => "abbreviated",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyntheticCorpus {
    pub schema_version: u32,
    pub doc_count: u64,
    pub dump_date: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyntheticQuery {
    pub id: String,
    pub sample_best_id: String,
    pub sample_building_group_id: String,
    pub sample_region: String,
    pub sample_street_primary_lang: String,
    pub variant_type: VariantType,
    /// Whether this sample was drawn from the random pool or the boosted
    /// cross_language pool (file 14 §2 — "Cross-language boost").
    #[serde(default)]
    pub boosted: bool,
    pub query_string: String,
    pub expected_best_id: String,
    pub expected_building_group_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyntheticEvalSet {
    pub version: String,
    pub seed: u64,
    pub corpus: SyntheticCorpus,
    pub queries: Vec<SyntheticQuery>,
}

impl SyntheticEvalSet {
    pub fn load(path: &Path) -> Result<Self> {
        let body = fs::read_to_string(path)
            .with_context(|| format!("reading synthetic eval set {path:?}"))?;
        serde_json::from_str(&body)
            .with_context(|| format!("parsing synthetic eval set {path:?} as JSON"))
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let body = serde_json::to_string_pretty(self).context("serialising SyntheticEvalSet")?;
        fs::write(path, body).with_context(|| format!("writing {path:?}"))
    }
}

// --- Adversarial ------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdversarialCategory {
    Hamlet,
    DeZone,
    Facilites,
    CrossLanguage,
    SpecialChars,
    Notability,
    KnownFailure,
}

impl AdversarialCategory {
    pub fn as_str(self) -> &'static str {
        match self {
            AdversarialCategory::Hamlet => "hamlet",
            AdversarialCategory::DeZone => "de_zone",
            AdversarialCategory::Facilites => "facilites",
            AdversarialCategory::CrossLanguage => "cross_language",
            AdversarialCategory::SpecialChars => "special_chars",
            AdversarialCategory::Notability => "notability",
            AdversarialCategory::KnownFailure => "known_failure",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdversarialQuery {
    pub id: String,
    pub category: AdversarialCategory,
    pub subcategory: String,
    pub query: String,
    /// For most categories: exactly one expected best_id.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub expected_best_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub expected_building_group_id: Option<String>,
    /// For `notability` — any of these bgids counts as correct (file 14 §3).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub expected_building_group_id_set: Option<Vec<String>>,
    /// For `known_failure` — free text describing current behavior.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub expected_behavior: Option<String>,
    #[serde(default)]
    pub expected_to_fail: bool,
    /// Quarterly review fields (file 14 §3 known_failure discipline).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub introduced: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub reference: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub acceptable_until: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub regression_marker: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdversarialEvalSet {
    pub version: String,
    pub queries: Vec<AdversarialQuery>,
}

impl AdversarialEvalSet {
    pub fn load(path: &Path) -> Result<Self> {
        let body = fs::read_to_string(path)
            .with_context(|| format!("reading adversarial eval set {path:?}"))?;
        serde_saphyr::from_str(&body)
            .with_context(|| format!("parsing adversarial eval set {path:?} as YAML"))
    }
}

// --- Unified ----------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum EvalSet {
    Synthetic(SyntheticEvalSet),
    Adversarial(AdversarialEvalSet),
}

impl EvalSet {
    /// Dispatch by extension. JSON → synthetic, YAML/YML → adversarial.
    pub fn auto_load(path: &Path) -> Result<Self> {
        match path.extension().and_then(|s| s.to_str()) {
            Some("json") => Ok(Self::Synthetic(SyntheticEvalSet::load(path)?)),
            Some("yaml") | Some("yml") => Ok(Self::Adversarial(AdversarialEvalSet::load(path)?)),
            ext => bail!(
                "unknown eval set format for {path:?} (extension={ext:?}); \
                 expected .json or .yaml/.yml"
            ),
        }
    }
}

/// Runner-facing unified view of a single query, independent of source format.
///
/// The runner only sees this shape; the metric layer pattern-matches on
/// `source` to attribute results to the right aggregation bucket.
#[derive(Debug, Clone)]
pub struct EvalQuery {
    pub id: String,
    pub query_string: String,
    /// One or more building_group_ids that count as "correct top-1." Set has
    /// >1 entries only for notability adversarial entries.
    pub expected_bgid_set: Vec<String>,
    /// One or more best_ids that count as the literal expected match. Empty
    /// when the source doesn't pin a best_id (e.g., notability set-of-bgids
    /// adversarial entries, or known_failure entries).
    pub expected_best_id_set: Vec<String>,
    pub expected_to_fail: bool,
    pub source: EvalQuerySource,
}

#[derive(Debug, Clone)]
pub enum EvalQuerySource {
    Synthetic {
        variant_type: VariantType,
        region: String,
        street_primary_lang: String,
        boosted: bool,
    },
    Adversarial {
        category: AdversarialCategory,
        subcategory: String,
    },
}

impl EvalQuery {
    pub fn from_synthetic(q: &SyntheticQuery) -> Self {
        EvalQuery {
            id: q.id.clone(),
            query_string: q.query_string.clone(),
            expected_bgid_set: vec![q.expected_building_group_id.clone()],
            expected_best_id_set: vec![q.expected_best_id.clone()],
            expected_to_fail: false,
            source: EvalQuerySource::Synthetic {
                variant_type: q.variant_type,
                region: q.sample_region.clone(),
                street_primary_lang: q.sample_street_primary_lang.clone(),
                boosted: q.boosted,
            },
        }
    }

    pub fn from_adversarial(q: &AdversarialQuery) -> Self {
        let expected_bgid_set = if let Some(set) = &q.expected_building_group_id_set {
            set.clone()
        } else if let Some(bgid) = &q.expected_building_group_id {
            vec![bgid.clone()]
        } else {
            Vec::new()
        };
        let expected_best_id_set = q.expected_best_id.iter().cloned().collect();
        EvalQuery {
            id: q.id.clone(),
            query_string: q.query.clone(),
            expected_bgid_set,
            expected_best_id_set,
            expected_to_fail: q.expected_to_fail,
            source: EvalQuerySource::Adversarial {
                category: q.category,
                subcategory: q.subcategory.clone(),
            },
        }
    }
}

/// Flatten a list of EvalSets into a single Vec<EvalQuery> for the runner.
pub fn flatten(sets: &[EvalSet]) -> Vec<EvalQuery> {
    let mut out = Vec::new();
    for set in sets {
        match set {
            EvalSet::Synthetic(s) => out.extend(s.queries.iter().map(EvalQuery::from_synthetic)),
            EvalSet::Adversarial(a) => {
                out.extend(a.queries.iter().map(EvalQuery::from_adversarial))
            }
        }
    }
    out
}

// --- Tests ------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write(path: &Path, body: &str) {
        fs::write(path, body).unwrap();
    }

    #[test]
    fn synthetic_round_trip() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("test.json");
        let set = SyntheticEvalSet {
            version: "mvp-v1".into(),
            seed: 1,
            corpus: SyntheticCorpus {
                schema_version: 3,
                doc_count: 6_841_382,
                dump_date: "2026-05-19".into(),
            },
            queries: vec![SyntheticQuery {
                id: "mvp-v1-000001".into(),
                sample_best_id: "X".into(),
                sample_building_group_id: "Y".into(),
                sample_region: "flanders".into(),
                sample_street_primary_lang: "nl".into(),
                variant_type: VariantType::Canonical,
                boosted: false,
                query_string: "Grote Markt 1, 2000 Antwerpen".into(),
                expected_best_id: "X".into(),
                expected_building_group_id: "Y".into(),
            }],
        };
        set.save(&path).unwrap();

        let loaded = SyntheticEvalSet::load(&path).unwrap();
        assert_eq!(loaded.version, "mvp-v1");
        assert_eq!(loaded.queries.len(), 1);
        assert_eq!(loaded.queries[0].variant_type, VariantType::Canonical);
    }

    #[test]
    fn adversarial_loads_yaml_with_notability_set() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("adv.yaml");
        write(
            &path,
            r#"
version: adversarial-belgium-v1
queries:
  - id: adv-notability-001
    category: notability
    subcategory: grand-place-bruxelles
    query: "Grand-Place"
    expected_building_group_id_set:
      - "bgid-001"
      - "bgid-002"
    notes: "UNESCO"
    regression_marker: true

  - id: adv-known-001
    category: known_failure
    subcategory: rue-de-la-gare-brussels
    query: "Rue de la Gare"
    expected_behavior: "Zero Brussels matches in top-100"
    expected_to_fail: true
    introduced: "2026-05-15"
    reference: "file 13 §1"
    acceptable_until: "2026-08-15"
"#,
        );

        let set = AdversarialEvalSet::load(&path).unwrap();
        assert_eq!(set.queries.len(), 2);
        let n = &set.queries[0];
        assert_eq!(n.category, AdversarialCategory::Notability);
        assert_eq!(n.expected_building_group_id_set.as_ref().unwrap().len(), 2);
        assert!(n.regression_marker);

        let k = &set.queries[1];
        assert_eq!(k.category, AdversarialCategory::KnownFailure);
        assert!(k.expected_to_fail);
        assert_eq!(k.acceptable_until.as_deref(), Some("2026-08-15"));
    }

    #[test]
    fn auto_load_dispatches_on_extension() {
        let tmp = TempDir::new().unwrap();
        let json_path = tmp.path().join("a.json");
        let yaml_path = tmp.path().join("a.yaml");
        write(
            &json_path,
            r#"{"version":"mvp-v1","seed":1,"corpus":{"schema_version":3,"doc_count":1,"dump_date":"2026-05-19"},"queries":[]}"#,
        );
        write(
            &yaml_path,
            r#"version: adversarial-v1
queries: []
"#,
        );

        match EvalSet::auto_load(&json_path).unwrap() {
            EvalSet::Synthetic(_) => {}
            _ => panic!("expected synthetic"),
        }
        match EvalSet::auto_load(&yaml_path).unwrap() {
            EvalSet::Adversarial(_) => {}
            _ => panic!("expected adversarial"),
        }
    }

    #[test]
    fn auto_load_rejects_unknown_extension() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("a.toml");
        write(&path, "");
        assert!(EvalSet::auto_load(&path).is_err());
    }

    #[test]
    fn flatten_concatenates_both_sources() {
        let synth = SyntheticEvalSet {
            version: "mvp-v1".into(),
            seed: 1,
            corpus: SyntheticCorpus {
                schema_version: 3,
                doc_count: 1,
                dump_date: "2026-05-19".into(),
            },
            queries: (0..3)
                .map(|i| SyntheticQuery {
                    id: format!("s-{i}"),
                    sample_best_id: "X".into(),
                    sample_building_group_id: "Y".into(),
                    sample_region: "flanders".into(),
                    sample_street_primary_lang: "nl".into(),
                    variant_type: VariantType::Canonical,
                    boosted: false,
                    query_string: "x".into(),
                    expected_best_id: "X".into(),
                    expected_building_group_id: "Y".into(),
                })
                .collect(),
        };
        let adv = AdversarialEvalSet {
            version: "adv-v1".into(),
            queries: (0..2)
                .map(|i| AdversarialQuery {
                    id: format!("a-{i}"),
                    category: AdversarialCategory::Hamlet,
                    subcategory: "x".into(),
                    query: "q".into(),
                    expected_best_id: None,
                    expected_building_group_id: Some("Y".into()),
                    expected_building_group_id_set: None,
                    expected_behavior: None,
                    expected_to_fail: false,
                    introduced: None,
                    reference: None,
                    acceptable_until: None,
                    notes: None,
                    regression_marker: false,
                })
                .collect(),
        };
        let flat = flatten(&[EvalSet::Synthetic(synth), EvalSet::Adversarial(adv)]);
        assert_eq!(flat.len(), 5);
    }
}
