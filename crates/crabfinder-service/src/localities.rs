//! In-memory lookup over the indexer's `localities-v1.json` (schema v8):
//! analyzed locality name → every locality carrying it. Backs the locality
//! tier (`resolver::Tier::Locality`) and locality-centred ranking.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use tantivy::Index;

use crabfinder_indexer::localities::{Locality, LocalityKind, read_localities};

use crate::query_builder::analyze;

#[derive(Debug, Default)]
pub struct LocalityIndex {
    by_name: HashMap<String, Vec<Locality>>,
    len: usize,
}

pub type SharedLocalityIndex = Arc<LocalityIndex>;

impl LocalityIndex {
    pub fn empty() -> Self {
        Self::default()
    }

    /// Key every language form of every locality by its analyzed tokens
    /// joined with a space (`"Han-sur-Lesse"` → `"han sur lesse"`).
    pub fn from_localities(index: &Index, localities: Vec<Locality>) -> Self {
        let len = localities.len();
        let mut by_name: HashMap<String, Vec<Locality>> = HashMap::new();
        for l in localities {
            let mut keys: Vec<String> = [&l.name.fr, &l.name.nl, &l.name.de]
                .into_iter()
                .flatten()
                .map(|n| analyze(index, n).join(" "))
                .filter(|k| !k.is_empty())
                .collect();
            keys.sort();
            keys.dedup();
            for k in keys {
                by_name.entry(k).or_default().push(l.clone());
            }
        }
        for v in by_name.values_mut() {
            v.sort_by(|a, b| {
                rank(a.kind)
                    .cmp(&rank(b.kind))
                    .then(b.address_count.cmp(&a.address_count))
            });
        }
        Self { by_name, len }
    }

    /// Load `localities-v1.json` from `index_dir`; empty when absent.
    pub fn load(index: &Index, index_dir: &Path) -> Result<Self> {
        Ok(Self::from_localities(index, read_localities(index_dir)?))
    }

    /// Localities whose name is exactly these analyzed tokens, most
    /// significant first (see `rank`).
    pub fn lookup(&self, tokens: &[String]) -> &[Locality] {
        self.by_name
            .get(&tokens.join(" "))
            .map_or(&[], Vec::as_slice)
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

/// Lookup order: administrative units before OSM places, bigger before
/// smaller.
fn rank(k: LocalityKind) -> u8 {
    match k {
        LocalityKind::Municipality => 0,
        LocalityKind::PartOfMunicipality => 1,
        LocalityKind::PostalArea => 2,
        LocalityKind::Village => 3,
        LocalityKind::Hamlet => 4,
        LocalityKind::Locality => 5,
        LocalityKind::IsolatedDwelling => 6,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crabfinder_core::MultilingualName;
    use crabfinder_indexer::index::{CrabSchema, register_tokenizers};

    fn loc(kind: LocalityKind, fr: &str, nl: Option<&str>, n: u32) -> Locality {
        Locality {
            kind,
            name: MultilingualName {
                fr: Some(fr.into()),
                nl: nl.map(Into::into),
                de: None,
            },
            lat: 50.0,
            lon: 4.0,
            postal_codes: vec!["5580".into()],
            address_count: n,
            region: None,
        }
    }

    fn index() -> Index {
        let schema = CrabSchema::build();
        let index = Index::create_in_ram(schema.schema.clone());
        register_tokenizers(&index);
        index
    }

    #[test]
    fn keys_are_analyzed_and_ranked() {
        let idx = LocalityIndex::from_localities(
            &index(),
            vec![
                loc(LocalityKind::Village, "Han-sur-Lesse", None, 10),
                loc(LocalityKind::PartOfMunicipality, "Han-sur-Lesse", None, 900),
                loc(LocalityKind::Municipality, "Bruxelles", Some("Brussel"), 1),
                loc(LocalityKind::Hamlet, "Bruxelles", None, 50),
            ],
        );
        assert_eq!(idx.len(), 4);
        assert!(!idx.is_empty());
        let han = idx.lookup(&["han".into(), "sur".into(), "lesse".into()]);
        assert_eq!(han.len(), 2);
        assert_eq!(han[0].kind, LocalityKind::PartOfMunicipality);
        let bxl = idx.lookup(&["brussel".into()]);
        assert_eq!(bxl.len(), 1);
        assert_eq!(
            idx.lookup(&["bruxelles".into()])[0].kind,
            LocalityKind::Municipality
        );
        assert!(idx.lookup(&["nowhere".into()]).is_empty());
        assert!(LocalityIndex::empty().is_empty());
    }

    #[test]
    fn load_missing_file_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            LocalityIndex::load(&index(), dir.path())
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn rank_order() {
        let mut kinds = [
            LocalityKind::IsolatedDwelling,
            LocalityKind::Locality,
            LocalityKind::Hamlet,
            LocalityKind::Village,
            LocalityKind::PostalArea,
            LocalityKind::PartOfMunicipality,
            LocalityKind::Municipality,
        ];
        kinds.sort_by_key(|k| rank(*k));
        assert_eq!(kinds[0], LocalityKind::Municipality);
        assert_eq!(kinds[6], LocalityKind::IsolatedDwelling);
    }
}
