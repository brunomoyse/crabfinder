//! OSM landmarks (v2 Phase 10): `--pois data/osm-pois-be-v1.csv`
//! (`scripts/build_osm_pois.py`) loaded at startup into an analyzed name →
//! landmarks map. A query naming a landmark ("Atomium", "Manneken Pis",
//! "Gare du Midi") that matches no street is answered with the BeSt
//! addresses around it (`resolver::search_landmark`,
//! `Confidence::Landmark`).
//!
//! An optional query-interpretation aid, off unless `--pois` is given:
//! BeSt stays the only source of what is returned (address, ids, position,
//! label). The landmark only says where to look.
//!
//! Loaded by the service rather than the indexer, like the abbreviation
//! dictionary: refreshing the landmarks needs no reindex. Data © OpenStreetMap
//! contributors, ODbL 1.0.

use std::collections::HashMap;
use std::io::Read;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use serde::Deserialize;
use tantivy::Index;

use crabfinder_core::MultilingualName;

use crate::query_builder::analyze;

/// One landmark: its OSM tag (`tourism=attraction`), names and position.
#[derive(Debug, Clone, PartialEq)]
pub struct Landmark {
    /// `node/240866019`.
    pub osm: String,
    pub kind: String,
    /// FR / NL / DE names, the plain `name` standing in when none is set.
    pub name: MultilingualName,
    pub lat: f64,
    pub lon: f64,
    /// Distinct name forms (`name`, FR, NL, DE, EN): a landmark known in
    /// several languages is the more prominent of two namesakes.
    pub names: u8,
}

#[derive(Debug, Default)]
pub struct LandmarkIndex {
    by_name: HashMap<String, Vec<Landmark>>,
    len: usize,
}

pub type SharedLandmarkIndex = Arc<LandmarkIndex>;

#[derive(Deserialize)]
struct Row {
    osm_type: String,
    osm_id: u64,
    kind: String,
    name: String,
    #[serde(default)]
    name_fr: String,
    #[serde(default)]
    name_nl: String,
    #[serde(default)]
    name_de: String,
    #[serde(default)]
    name_en: String,
    lat: f64,
    lon: f64,
}

fn non_empty(s: &str) -> Option<String> {
    let s = s.trim();
    (!s.is_empty()).then(|| s.to_string())
}

impl LandmarkIndex {
    pub fn empty() -> Self {
        Self::default()
    }

    /// Read the CSV (header `osm_type,osm_id,kind,name,name_fr,name_nl,
    /// name_de,name_en,lat,lon`) and key every name form — and each side of
    /// a bilingual `"Bruxelles-Midi - Brussel-Zuid"` — by its analyzed
    /// tokens.
    pub fn from_csv(index: &Index, input: impl Read) -> Result<Self> {
        let mut by_name: HashMap<String, Vec<Landmark>> = HashMap::new();
        let mut len = 0;
        for (i, row) in csv::Reader::from_reader(input)
            .deserialize::<Row>()
            .enumerate()
        {
            let r = row.with_context(|| format!("landmarks csv record {}", i + 1))?;
            let mut name = MultilingualName {
                fr: non_empty(&r.name_fr),
                nl: non_empty(&r.name_nl),
                de: non_empty(&r.name_de),
            };
            if name.fr.is_none() && name.nl.is_none() && name.de.is_none() {
                name.fr = non_empty(&r.name);
            }
            let mut forms: Vec<&str> = [&r.name, &r.name_fr, &r.name_nl, &r.name_de, &r.name_en]
                .into_iter()
                .map(|n| n.trim())
                .filter(|n| !n.is_empty())
                .collect();
            forms.sort_unstable();
            forms.dedup();
            let lm = Landmark {
                names: forms.len() as u8,
                osm: format!("{}/{}", r.osm_type, r.osm_id),
                kind: r.kind,
                name,
                lat: r.lat,
                lon: r.lon,
            };
            let mut keys: Vec<String> = [&r.name, &r.name_fr, &r.name_nl, &r.name_de, &r.name_en]
                .into_iter()
                .flat_map(|n| {
                    let mut forms = vec![n.as_str()];
                    forms.extend(n.split(" - ").chain(n.split(" / ")));
                    forms
                })
                .map(|n| analyze(index, n).join(" "))
                .filter(|k| !k.is_empty())
                .collect();
            keys.sort();
            keys.dedup();
            for k in keys {
                by_name.entry(k).or_default().push(lm.clone());
            }
            len += 1;
        }
        for v in by_name.values_mut() {
            v.sort_by(|a, b| {
                rank(&a.kind)
                    .cmp(&rank(&b.kind))
                    .then(b.names.cmp(&a.names))
                    .then(a.osm.cmp(&b.osm))
            });
        }
        Ok(Self { by_name, len })
    }

    pub fn load(index: &Index, path: &Path) -> Result<Self> {
        let f = std::fs::File::open(path).with_context(|| format!("opening {path:?}"))?;
        Self::from_csv(index, f).with_context(|| format!("reading landmarks {path:?}"))
    }

    /// Landmarks named exactly these analyzed tokens, most landmark-like
    /// first (`rank`), then the one with more name forms.
    pub fn lookup(&self, tokens: &[String]) -> &[Landmark] {
        self.by_name
            .get(&tokens.join(" "))
            .map_or(&[], Vec::as_slice)
    }

    /// Every landmark once (a landmark sits under each of its names).
    pub fn all(&self) -> Vec<&Landmark> {
        let mut out: Vec<&Landmark> = self.by_name.values().flatten().collect();
        out.sort_by(|a, b| a.osm.cmp(&b.osm));
        out.dedup_by(|a, b| a.osm == b.osm);
        out
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

/// Which of several same-name landmarks a bare name most likely means:
/// sights and monuments before transport and institutions, before parks
/// and memorials, before churches (thousands of "Sint-Martinuskerk").
fn rank(kind: &str) -> u8 {
    let (key, value) = kind.split_once('=').unwrap_or((kind, ""));
    match (key, value) {
        ("tourism", "attraction" | "zoo" | "theme_park" | "aquarium") => 0,
        ("historic", "castle" | "monument" | "fort" | "citadel" | "city_gate") => 1,
        ("aeroway", _) | ("railway", _) | ("leisure", "stadium") => 2,
        ("amenity", "townhall" | "university") | ("building", _) => 2,
        ("tourism", _) | ("amenity", "theatre" | "concert_hall" | "hospital") => 3,
        ("man_made", _) | ("historic", _) | ("leisure", _) => 4,
        _ => 5,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crabfinder_indexer::index::{CrabSchema, register_tokenizers};

    fn index() -> Index {
        let i = Index::create_in_ram(CrabSchema::build().schema);
        register_tokenizers(&i);
        i
    }

    const CSV: &str = "osm_type,osm_id,kind,name,name_fr,name_nl,name_de,name_en,lat,lon
node,17401552,railway=station,Bruxelles-Midi - Brussel-Zuid,Bruxelles-Midi,Brussel-Zuid,,Brussels-Midi,50.8357755,4.3360541
relation,19189218,tourism=attraction,Atomium,Atomium,Atomium,,Atomium,50.8949213,4.3415005
way,1,amenity=place_of_worship,Atomium,,,,,51.0,4.0
node,2,tourism=museum,Musée Wiertz,,,,,50.83,4.37
node,13388461560,railway=station,Gare Centrale,,,,,50.40,4.43
node,142700522,railway=station,Gare Centrale - Centraal Station,Gare Centrale,Centraal Station,,,50.84,4.35
";

    fn toks(i: &Index, s: &str) -> Vec<String> {
        analyze(i, s)
    }

    #[test]
    fn every_name_form_and_bilingual_part_is_a_key() {
        let i = index();
        let lm = LandmarkIndex::from_csv(&i, CSV.as_bytes()).unwrap();
        assert_eq!(lm.len(), 6);
        for q in [
            "Bruxelles-Midi - Brussel-Zuid",
            "Bruxelles-Midi",
            "brussel zuid",
            "Brussels Midi",
        ] {
            assert_eq!(lm.lookup(&toks(&i, q))[0].osm, "node/17401552", "{q}");
        }
        // No FR/NL/DE name: the plain name stands in.
        let w = &lm.lookup(&toks(&i, "musee wiertz"))[0];
        assert_eq!(w.name.fr.as_deref(), Some("Musée Wiertz"));
        assert!(lm.lookup(&toks(&i, "midi")).is_empty());
    }

    #[test]
    fn sights_rank_before_churches_of_the_same_name() {
        let i = index();
        let lm = LandmarkIndex::from_csv(&i, CSV.as_bytes()).unwrap();
        let hits = lm.lookup(&toks(&i, "atomium"));
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].kind, "tourism=attraction");
        assert_eq!(rank("amenity=place_of_worship"), 5);
        assert_eq!(rank("historic=memorial"), 4);
        assert_eq!(rank("odd"), 5);
        // Same kind: the one with more name forms (Brussels' bilingual
        // station, not Charleroi's), whatever the id order.
        let gc = lm.lookup(&toks(&i, "gare centrale"));
        assert_eq!((gc[0].osm.as_str(), gc[0].names), ("node/142700522", 3));
    }

    #[test]
    fn bad_rows_and_files_are_errors() {
        let i = index();
        let err = LandmarkIndex::from_csv(&i, "osm_type,osm_id\nnode,x\n".as_bytes()).unwrap_err();
        assert!(format!("{err:#}").contains("record 1"), "{err:#}");
        assert!(LandmarkIndex::load(&i, Path::new("/nonexistent.csv")).is_err());
        assert!(LandmarkIndex::empty().is_empty());
    }
}
