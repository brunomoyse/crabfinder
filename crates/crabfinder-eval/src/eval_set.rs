//! Eval set types + loaders (file 14 §4).
//!
//! Three on-disk formats:
//!   - SyntheticEvalSet — JSON, machine-generated (file 14 §2)
//!   - AdversarialEvalSet — YAML, hand-edited (file 14 §3)
//!   - RealWorldEvalSet — YAML with `kind: realworld`: addresses from
//!     public open-data lists with their published coordinates (v3
//!     Phase 11.1, `scripts/build_realworld_eval.py`)
//!
//! All fold into a unified `EvalSet` enum the runner iterates over.

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
    /// One-character edit in the longest street word (v2).
    Typo,
    /// Canonical query with the box number spelled out (`bte` / `bus`).
    BoxNumber,
    /// House number ≥ 1000 next to a postal code: both are 4 digits.
    HighHouseNumber,
    /// Commas between every part and a `B-` postal prefix.
    MessyPunctuation,
    /// Street + number + the postal code of the nearest *other* postal
    /// area: the neighbouring-commune mistake (v2 Phase 10).
    WrongPostal,
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
            VariantType::Typo => "typo",
            VariantType::BoxNumber => "box_number",
            VariantType::HighHouseNumber => "high_house_number",
            VariantType::MessyPunctuation => "messy_punctuation",
            VariantType::WrongPostal => "wrong_postal",
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
    /// A landmark named without a street ("Atomium", "Gare du Midi"),
    /// scored at `match_level: locality` (v2 Phase 10).
    Landmark,
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
            AdversarialCategory::Landmark => "landmark",
            AdversarialCategory::KnownFailure => "known_failure",
        }
    }
}

/// What counts as a correct answer for an adversarial entry (v2 Phase 7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchLevel {
    /// The exact address (`expected_best_id`), falling back to the
    /// building when no address is pinned.
    #[default]
    Address,
    /// Any unit of an expected building: queries that can't name a unit
    /// (landmarks, notability).
    Building,
    /// Any building of an expected building's street: queries without a
    /// house number ("Wezembeeklaan Kraainem", hamlets), where which
    /// building comes first is not the query's to decide.
    Street,
    /// Any building within `within_m` of `expected_point`: queries that
    /// only name a place (hamlets), where the place, not one of its
    /// streets, is the answer (v2 Phase 6).
    Locality,
}

impl MatchLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            MatchLevel::Address => "address",
            MatchLevel::Building => "building",
            MatchLevel::Street => "street",
            MatchLevel::Locality => "locality",
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
    #[serde(default)]
    pub match_level: MatchLevel,
    /// `[lat, lon]` of the place, for `match_level: locality`.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub expected_point: Option<[f64; 2]>,
    /// Radius around `expected_point`, metres (default
    /// `DEFAULT_WITHIN_M`).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub within_m: Option<f64>,
}

/// Default radius of a `locality` match.
pub const DEFAULT_WITHIN_M: f64 = 1_500.0;

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

// --- Real-world -------------------------------------------------------------

/// One address from a public open-data list (a school, a pharmacy…): the
/// fields as published, the joined free-text query, and the published
/// coordinates as ground truth (v3 Phase 11.1).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RealWorldQuery {
    pub id: String,
    /// Dataset key (`brussels-schools`), described in `eval/realworld/SOURCES.md`.
    pub source: String,
    /// `brussels` / `flanders` / `wallonia`.
    pub region: String,
    pub query: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub street: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub house_number: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub postal_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub city: Option<String>,
    /// `[lat, lon]` as published by the source.
    pub expected_point: [f64; 2],
    /// Radius around `expected_point`, metres (default
    /// `REALWORLD_WITHIN_M`): published points are often a parcel or an
    /// entrance, not the BeSt address point.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub within_m: Option<f64>,
    /// Hand-verified BeSt address: the entry is then scored at address
    /// level, not by distance.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub expected_best_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub expected_building_group_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub notes: Option<String>,
}

/// Default radius of a real-world match.
pub const REALWORLD_WITHIN_M: f64 = 100.0;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RealWorldEvalSet {
    pub version: String,
    /// Always `realworld`: what tells this YAML from an adversarial set.
    pub kind: String,
    pub queries: Vec<RealWorldQuery>,
}

impl RealWorldEvalSet {
    pub fn load(path: &Path) -> Result<Self> {
        let body = fs::read_to_string(path)
            .with_context(|| format!("reading real-world eval set {path:?}"))?;
        serde_saphyr::from_str(&body)
            .with_context(|| format!("parsing real-world eval set {path:?} as YAML"))
    }
}

// --- Unified ----------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum EvalSet {
    Synthetic(SyntheticEvalSet),
    Adversarial(AdversarialEvalSet),
    RealWorld(RealWorldEvalSet),
}

/// Just the top-level `kind` of a YAML set (adversarial when absent).
#[derive(Deserialize)]
struct YamlKind {
    #[serde(default)]
    kind: Option<String>,
}

impl EvalSet {
    /// Dispatch by extension. JSON → synthetic; YAML/YML → real-world when
    /// it says `kind: realworld`, adversarial otherwise.
    pub fn auto_load(path: &Path) -> Result<Self> {
        match path.extension().and_then(|s| s.to_str()) {
            Some("json") => Ok(Self::Synthetic(SyntheticEvalSet::load(path)?)),
            Some("yaml") | Some("yml") => {
                let body = fs::read_to_string(path)
                    .with_context(|| format!("reading eval set {path:?}"))?;
                let kind: YamlKind = serde_saphyr::from_str(&body)
                    .with_context(|| format!("parsing eval set {path:?} as YAML"))?;
                match kind.kind.as_deref() {
                    None | Some("adversarial") => {
                        Ok(Self::Adversarial(AdversarialEvalSet::load(path)?))
                    }
                    Some("realworld") => Ok(Self::RealWorld(RealWorldEvalSet::load(path)?)),
                    Some(k) => bail!(
                        "unknown eval set kind {k:?} in {path:?}; expected adversarial or realworld"
                    ),
                }
            }
            ext => bail!(
                "unknown eval set format for {path:?} (extension={ext:?}); \
                 expected .json or .yaml/.yml"
            ),
        }
    }
}

/// An address split into fields, for geocoders that take structured
/// input (real-world entries carry them; other sets are free text only).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StructuredAddress {
    pub street: Option<String>,
    pub house_number: Option<String>,
    pub postal_code: Option<String>,
    pub city: Option<String>,
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
    pub match_level: MatchLevel,
    /// `(lat, lon)` and radius for `MatchLevel::Locality`.
    pub expected_point: Option<((f64, f64), f64)>,
    /// Where the answer is, for the distance metrics (v3 Phase 11.1): the
    /// published point of a real-world entry, the place of a locality
    /// one, else the BeSt point of the expected address when the
    /// benchmark looked it up (`--with-points`).
    pub reference_point: Option<(f64, f64)>,
    /// The same address in fields, when the set has them.
    pub structured: Option<StructuredAddress>,
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
    RealWorld {
        source: String,
        region: String,
    },
}

impl EvalQuerySource {
    /// The set a query comes from, for per-set report rows:
    /// `synthetic`, `adversarial`, `realworld`.
    pub fn set_kind(&self) -> &'static str {
        match self {
            EvalQuerySource::Synthetic { .. } => "synthetic",
            EvalQuerySource::Adversarial { .. } => "adversarial",
            EvalQuerySource::RealWorld { .. } => "realworld",
        }
    }

    /// Region of a synthetic or real-world query, spelled the same for
    /// both (`brussels` / `flanders` / `wallonia`; synthetic sets say
    /// `BE-BRU` / `BE-VLG` / `BE-WAL`). `None` for adversarial queries and
    /// unknown spellings.
    pub fn region(&self) -> Option<&'static str> {
        match self {
            EvalQuerySource::Synthetic { region, .. }
            | EvalQuerySource::RealWorld { region, .. } => normalize_region(region),
            EvalQuerySource::Adversarial { .. } => None,
        }
    }
}

pub fn normalize_region(r: &str) -> Option<&'static str> {
    match r.to_ascii_lowercase().as_str() {
        "brussels" | "be-bru" => Some("brussels"),
        "flanders" | "be-vlg" => Some("flanders"),
        "wallonia" | "be-wal" => Some("wallonia"),
        _ => None,
    }
}

impl EvalQuery {
    pub fn from_synthetic(q: &SyntheticQuery) -> Self {
        EvalQuery {
            id: q.id.clone(),
            query_string: q.query_string.clone(),
            expected_bgid_set: vec![q.expected_building_group_id.clone()],
            expected_best_id_set: vec![q.expected_best_id.clone()],
            expected_to_fail: false,
            match_level: MatchLevel::Address,
            expected_point: None,
            reference_point: None,
            structured: None,
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
            match_level: q.match_level,
            expected_point: q
                .expected_point
                .map(|[lat, lon]| ((lat, lon), q.within_m.unwrap_or(DEFAULT_WITHIN_M))),
            reference_point: q.expected_point.map(|[lat, lon]| (lat, lon)),
            structured: None,
            source: EvalQuerySource::Adversarial {
                category: q.category,
                subcategory: q.subcategory.clone(),
            },
        }
    }

    /// Scored by distance to the published point, or at address level
    /// once a BeSt address is pinned.
    pub fn from_realworld(q: &RealWorldQuery) -> Self {
        let [lat, lon] = q.expected_point;
        let match_level = if q.expected_best_id.is_some() {
            MatchLevel::Address
        } else {
            MatchLevel::Locality
        };
        EvalQuery {
            id: q.id.clone(),
            query_string: q.query.clone(),
            expected_bgid_set: q.expected_building_group_id.iter().cloned().collect(),
            expected_best_id_set: q.expected_best_id.iter().cloned().collect(),
            expected_to_fail: false,
            match_level,
            expected_point: Some(((lat, lon), q.within_m.unwrap_or(REALWORLD_WITHIN_M))),
            reference_point: Some((lat, lon)),
            structured: Some(StructuredAddress {
                street: q.street.clone(),
                house_number: q.house_number.clone(),
                postal_code: q.postal_code.clone(),
                city: q.city.clone(),
            }),
            source: EvalQuerySource::RealWorld {
                source: q.source.clone(),
                region: q.region.clone(),
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
            EvalSet::RealWorld(r) => out.extend(r.queries.iter().map(EvalQuery::from_realworld)),
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

    const REALWORLD_YAML: &str = r#"version: realworld-be-v1
kind: realworld
queries:
  - id: rw-brussels-schools-0001
    source: brussels-schools
    region: brussels
    query: "Rue de la Loi 16, 1000 Bruxelles"
    street: "Rue de la Loi"
    house_number: "16"
    postal_code: "1000"
    city: "Bruxelles"
    expected_point: [50.8466, 4.3669]
  - id: rw-flanders-schools-0001
    source: flanders-schools
    region: flanders
    query: "Grote Markt 1, 2000 Antwerpen"
    expected_point: [51.2213, 4.3997]
    within_m: 250
    expected_best_id: "https://data.vlaanderen.be/id/adres/1/2020"
    expected_building_group_id: "ns|1|1"
"#;

    #[test]
    fn realworld_yaml_is_told_apart_by_kind_and_flattens() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("rw.yaml");
        write(&path, REALWORLD_YAML);
        let EvalSet::RealWorld(set) = EvalSet::auto_load(&path).unwrap() else {
            panic!("expected real-world");
        };
        assert_eq!(set.queries.len(), 2);
        let flat = flatten(&[EvalSet::RealWorld(set)]);
        // No pinned address: by distance, 100 m by default.
        let a = &flat[0];
        assert_eq!(a.match_level, MatchLevel::Locality);
        assert_eq!(
            a.expected_point,
            Some(((50.8466, 4.3669), REALWORLD_WITHIN_M))
        );
        assert_eq!(a.reference_point, Some((50.8466, 4.3669)));
        let st = a.structured.as_ref().unwrap();
        assert_eq!(
            (st.street.as_deref(), st.house_number.as_deref()),
            (Some("Rue de la Loi"), Some("16"))
        );
        assert_eq!(a.source.set_kind(), "realworld");
        assert_eq!(a.source.region(), Some("brussels"));
        // A pinned address: address level, radius kept for distances.
        let b = &flat[1];
        assert_eq!(b.match_level, MatchLevel::Address);
        assert_eq!(b.expected_bgid_set, ["ns|1|1"]);
        assert_eq!(b.expected_point.unwrap().1, 250.0);
        assert_eq!(b.structured.as_ref().unwrap().street, None);
    }

    #[test]
    fn yaml_kind_dispatch_rejects_unknown_kinds() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("x.yaml");
        write(&path, "version: v\nkind: adversarial\nqueries: []\n");
        assert!(matches!(
            EvalSet::auto_load(&path).unwrap(),
            EvalSet::Adversarial(_)
        ));
        write(&path, "version: v\nkind: other\nqueries: []\n");
        let err = EvalSet::auto_load(&path).unwrap_err();
        assert!(
            format!("{err:#}").contains("unknown eval set kind"),
            "{err:#}"
        );
        write(&path, "[: not yaml");
        assert!(EvalSet::auto_load(&path).is_err());
        assert!(EvalSet::auto_load(&tmp.path().join("missing.yaml")).is_err());
        assert!(RealWorldEvalSet::load(&tmp.path().join("missing.yaml")).is_err());
    }

    #[test]
    fn source_kinds_and_regions() {
        let adv = EvalQuerySource::Adversarial {
            category: AdversarialCategory::Hamlet,
            subcategory: "x".into(),
        };
        assert_eq!((adv.set_kind(), adv.region()), ("adversarial", None));
        let syn = EvalQuerySource::Synthetic {
            variant_type: VariantType::Typo,
            region: "wallonia".into(),
            street_primary_lang: "fr".into(),
            boosted: false,
        };
        assert_eq!(
            (syn.set_kind(), syn.region()),
            ("synthetic", Some("wallonia"))
        );
        for (r, n) in [
            ("BE-BRU", "brussels"),
            ("BE-VLG", "flanders"),
            ("be-wal", "wallonia"),
        ] {
            assert_eq!(normalize_region(r), Some(n));
        }
        assert_eq!(normalize_region("mars"), None);
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
                    match_level: MatchLevel::Street,
                    expected_point: None,
                    within_m: None,
                })
                .collect(),
        };
        let flat = flatten(&[EvalSet::Synthetic(synth), EvalSet::Adversarial(adv)]);
        assert_eq!(flat.len(), 5);
        assert_eq!(flat[0].match_level, MatchLevel::Address);
        assert_eq!(flat[4].match_level, MatchLevel::Street);
    }
}
