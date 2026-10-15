//! Notability whitelist loader (schema v4, file 13 §6 Option A).
//!
//! Reads `data/notability-whitelist-v1.yaml`, builds a
//! `HashMap<building_group_id, f64>` mapping each whitelisted address's
//! bgid to its boost multiplier. The indexer joins this map against each
//! address's bgid in Pass 2 and stamps the `notability_score` FAST field.
//!
//! Addresses NOT in the whitelist get notability_score = 1.0 (no boost).

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;
use tracing::{info, warn};

/// One whitelist entry. Mirrors the YAML schema.
#[derive(Debug, Deserialize)]
pub struct WhitelistEntry {
    pub notability_id: String,
    pub canonical_name: String,
    pub target_building_group_ids: Vec<String>,
    pub boost_multiplier: f64,
    #[serde(default)]
    pub rationale: String,
}

#[derive(Debug, Deserialize)]
pub struct WhitelistFile {
    pub version: String,
    pub entries: Vec<WhitelistEntry>,
}

/// Loaded whitelist in lookup form. Constructed once at indexer startup.
pub struct NotabilityMap {
    by_bgid: HashMap<String, f64>,
    pub n_entries: usize,
    pub n_unique_bgids: usize,
}

impl NotabilityMap {
    /// Empty map — every doc gets notability_score = 1.0.
    pub fn empty() -> Self {
        Self {
            by_bgid: HashMap::new(),
            n_entries: 0,
            n_unique_bgids: 0,
        }
    }

    /// Load from YAML file. The same bgid appearing under multiple entries
    /// (rare but possible if two notability_ids share a target) takes the
    /// MAX multiplier — most-famous wins.
    pub fn load(path: &Path) -> Result<Self> {
        let body =
            fs::read_to_string(path).with_context(|| format!("reading whitelist {path:?}"))?;
        let parsed: WhitelistFile = serde_saphyr::from_str(&body)
            .with_context(|| format!("parsing whitelist {path:?} as YAML"))?;

        if parsed.version != "v1" {
            warn!(
                version = %parsed.version,
                "notability whitelist version != 'v1' — interpretation may drift"
            );
        }

        let mut by_bgid: HashMap<String, f64> = HashMap::new();
        for entry in &parsed.entries {
            if entry.boost_multiplier < 1.0 {
                warn!(
                    notability_id = %entry.notability_id,
                    boost = entry.boost_multiplier,
                    "notability entry has boost_multiplier < 1.0 — would penalise, not boost; coerced to 1.0"
                );
                continue;
            }
            for bgid in &entry.target_building_group_ids {
                let cur = by_bgid.entry(bgid.clone()).or_insert(1.0);
                if entry.boost_multiplier > *cur {
                    *cur = entry.boost_multiplier;
                }
            }
        }

        info!(
            ?path,
            n_entries = parsed.entries.len(),
            n_unique_bgids = by_bgid.len(),
            "notability: whitelist loaded"
        );
        Ok(Self {
            n_entries: parsed.entries.len(),
            n_unique_bgids: by_bgid.len(),
            by_bgid,
        })
    }

    /// Look up the boost for a building_group_id. Returns 1.0 if not in
    /// the whitelist (= no boost).
    pub fn boost_for(&self, bgid: &str) -> f64 {
        self.by_bgid.get(bgid).copied().unwrap_or(1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_tmp(yaml: &str) -> tempfile::NamedTempFile {
        let mut f = tempfile::Builder::new().suffix(".yaml").tempfile().unwrap();
        f.write_all(yaml.as_bytes()).unwrap();
        f
    }

    #[test]
    fn empty_map_returns_unit_boost() {
        let m = NotabilityMap::empty();
        assert_eq!(m.boost_for("anything"), 1.0);
    }

    #[test]
    fn loaded_entry_returns_its_multiplier() {
        let yaml = r#"
version: v1
entries:
  - notability_id: nota-test
    canonical_name: Test
    target_building_group_ids:
      - bgid-A
      - bgid-B
    boost_multiplier: 2.5
"#;
        let f = write_tmp(yaml);
        let m = NotabilityMap::load(f.path()).unwrap();
        assert_eq!(m.boost_for("bgid-A"), 2.5);
        assert_eq!(m.boost_for("bgid-B"), 2.5);
        assert_eq!(m.boost_for("bgid-missing"), 1.0);
    }

    #[test]
    fn overlapping_entries_take_max() {
        let yaml = r#"
version: v1
entries:
  - notability_id: nota-low
    canonical_name: Low
    target_building_group_ids: [bgid-shared]
    boost_multiplier: 2.0
  - notability_id: nota-high
    canonical_name: High
    target_building_group_ids: [bgid-shared]
    boost_multiplier: 3.0
"#;
        let f = write_tmp(yaml);
        let m = NotabilityMap::load(f.path()).unwrap();
        assert_eq!(m.boost_for("bgid-shared"), 3.0);
    }

    #[test]
    fn sub_unit_multiplier_is_skipped_with_warning() {
        let yaml = r#"
version: v1
entries:
  - notability_id: nota-bogus
    canonical_name: Bogus
    target_building_group_ids: [bgid-X]
    boost_multiplier: 0.5
"#;
        let f = write_tmp(yaml);
        let m = NotabilityMap::load(f.path()).unwrap();
        // Sub-unit boost should not produce a stored entry; bgid stays at default.
        assert_eq!(m.boost_for("bgid-X"), 1.0);
    }
}
