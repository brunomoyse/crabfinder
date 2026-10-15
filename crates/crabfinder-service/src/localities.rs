//! In-memory lookup over the indexer's `localities-v1.json` (schema v8):
//! analyzed locality name → every locality carrying it. Backs the locality
//! tier (`resolver::Tier::Locality`) and locality-centred ranking.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use tantivy::Index;

use crabfinder_indexer::localities::{Locality, LocalityKind, postal_centres, read_localities};

use crate::query_builder::analyze;

#[derive(Debug, Default)]
pub struct LocalityIndex {
    by_name: HashMap<String, Vec<Locality>>,
    /// Postal code → centre (`postal_centres`), for the postal-code
    /// relaxation (v2 Phase 10).
    postal: HashMap<String, (f64, f64)>,
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
        let postal = postal_centres(&localities);
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
        Self {
            by_name,
            postal,
            len,
        }
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

    /// Localities whose name starts with these whole tokens and goes on
    /// (`"la roche"` → La Roche-en-Ardenne), most significant first. A
    /// linear scan: only for the outer part of a two-part locality query.
    pub fn lookup_prefix(&self, tokens: &[String]) -> Vec<&Locality> {
        let prefix = format!("{} ", tokens.join(" "));
        let mut out: Vec<&Locality> = self
            .by_name
            .iter()
            .filter(|(k, _)| k.starts_with(&prefix))
            .flat_map(|(_, v)| v)
            .collect();
        out.sort_by(|a, b| {
            rank(a.kind)
                .cmp(&rank(b.kind))
                .then(b.address_count.cmp(&a.address_count))
        });
        out
    }

    /// Centre of postal code `pc`, when the index knows it.
    pub fn postal_centre(&self, pc: &str) -> Option<(f64, f64)> {
        self.postal.get(pc).copied()
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

/// The ten provinces plus Brussels, for "`<village> <province>`" queries
/// (`"ave namur"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Province {
    Brussels,
    BrabantWallon,
    VlaamsBrabant,
    Antwerpen,
    Limburg,
    Liege,
    Namur,
    Luxembourg,
    Hainaut,
    WestVlaanderen,
    OostVlaanderen,
}

impl Province {
    /// Province of a Belgian postal code (bpost ranges).
    pub fn of_postal_code(pc: &str) -> Option<Self> {
        let n: u32 = pc.parse().ok()?;
        Some(match n {
            1000..=1299 => Self::Brussels,
            1300..=1499 => Self::BrabantWallon,
            1500..=1999 | 3000..=3499 => Self::VlaamsBrabant,
            2000..=2999 => Self::Antwerpen,
            3500..=3999 => Self::Limburg,
            4000..=4999 => Self::Liege,
            5000..=5999 => Self::Namur,
            6000..=6599 | 7000..=7999 => Self::Hainaut,
            6600..=6999 => Self::Luxembourg,
            8000..=8999 => Self::WestVlaanderen,
            9000..=9999 => Self::OostVlaanderen,
            _ => return None,
        })
    }

    /// The province these analyzed tokens name, in French, Dutch or German
    /// (`["oost", "vlaanderen"]`, `["province", "de", "namur"]`).
    pub fn named(tokens: &[String]) -> Option<Self> {
        let mut t: &[String] = tokens;
        for lead in [
            &["province", "de"][..],
            &["provincie"],
            &["provinz"],
            &["prov"],
        ] {
            if t.len() > lead.len() && t.iter().zip(lead).all(|(a, b)| a == b) {
                t = &t[lead.len()..];
                break;
            }
        }
        Some(match t.join(" ").as_str() {
            "brabant wallon" | "waals brabant" | "wallonisch brabant" => Self::BrabantWallon,
            "vlaams brabant" | "brabant flamand" | "flamisch brabant" => Self::VlaamsBrabant,
            "antwerpen" | "anvers" => Self::Antwerpen,
            "limburg" => Self::Limburg,
            "liege" | "luik" | "luttich" => Self::Liege,
            "namur" | "namen" => Self::Namur,
            "luxembourg" | "luxemburg" => Self::Luxembourg,
            "hainaut" | "henegouwen" | "hennegau" => Self::Hainaut,
            "west vlaanderen" | "flandre occidentale" | "westflandern" => Self::WestVlaanderen,
            "oost vlaanderen" | "flandre orientale" | "ostflandern" => Self::OostVlaanderen,
            _ => return None,
        })
    }

    /// Whether any of `postal_codes` lies in this province.
    pub fn contains_any(self, postal_codes: &[String]) -> bool {
        postal_codes
            .iter()
            .any(|pc| Self::of_postal_code(pc) == Some(self))
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
        let pre = idx.lookup_prefix(&["han".into(), "sur".into()]);
        assert_eq!(pre.len(), 2);
        assert_eq!(pre[0].kind, LocalityKind::PartOfMunicipality);
        // Whole tokens only, and the name must go on.
        assert!(idx.lookup_prefix(&["ha".into()]).is_empty());
        assert!(idx.lookup_prefix(&["brussel".into()]).is_empty());
    }

    fn toks(s: &str) -> Vec<String> {
        s.split(' ').map(String::from).collect()
    }

    #[test]
    fn provinces_by_postal_code_and_name() {
        for (pc, p) in [
            ("1000", Province::Brussels),
            ("1348", Province::BrabantWallon),
            ("1500", Province::VlaamsBrabant),
            ("3000", Province::VlaamsBrabant),
            ("2000", Province::Antwerpen),
            ("3500", Province::Limburg),
            ("4000", Province::Liege),
            ("5580", Province::Namur),
            ("6000", Province::Hainaut),
            ("7000", Province::Hainaut),
            ("6700", Province::Luxembourg),
            ("8000", Province::WestVlaanderen),
            ("9000", Province::OostVlaanderen),
        ] {
            assert_eq!(Province::of_postal_code(pc), Some(p), "{pc}");
        }
        assert_eq!(Province::of_postal_code("0999"), None);
        assert_eq!(Province::of_postal_code("abcd"), None);

        assert_eq!(Province::named(&toks("namur")), Some(Province::Namur));
        assert_eq!(
            Province::named(&toks("province de namur")),
            Some(Province::Namur)
        );
        assert_eq!(
            Province::named(&toks("provincie antwerpen")),
            Some(Province::Antwerpen)
        );
        assert_eq!(Province::named(&toks("prov luik")), Some(Province::Liege));
        assert_eq!(
            Province::named(&toks("oost vlaanderen")),
            Some(Province::OostVlaanderen)
        );
        assert_eq!(
            Province::named(&toks("flandre occidentale")),
            Some(Province::WestVlaanderen)
        );
        assert_eq!(
            Province::named(&toks("brabant wallon")),
            Some(Province::BrabantWallon)
        );
        assert_eq!(
            Province::named(&toks("vlaams brabant")),
            Some(Province::VlaamsBrabant)
        );
        assert_eq!(Province::named(&toks("limburg")), Some(Province::Limburg));
        assert_eq!(
            Province::named(&toks("luxemburg")),
            Some(Province::Luxembourg)
        );
        assert_eq!(Province::named(&toks("hennegau")), Some(Province::Hainaut));
        assert_eq!(Province::named(&toks("anvers")), Some(Province::Antwerpen));
        assert_eq!(Province::named(&toks("provinz")), None);
        assert_eq!(Province::named(&toks("bruxelles")), None);
        assert!(Province::Namur.contains_any(&["5000".into(), "1000".into()]));
        assert!(!Province::Namur.contains_any(&["4000".into()]));
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
