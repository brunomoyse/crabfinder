//! Autocomplete (v3 Phase 11.5): an in-memory name index built at startup,
//! next to the Tantivy index rather than inside it (the v5 edge-ngram
//! field is not coming back: see `reports/archive/phase7a-removed/`).
//!
//! Entries:
//! - one per BeSt street and postal code (names in every language, the
//!   address count, the best notability, a representative address);
//! - the BeSt localities of `localities-v1.json`: municipalities, parts of
//!   municipality, postal areas;
//! - when the service runs with the optional OSM aids (`--places` index,
//!   `--pois`), OSM places and landmarks. They are only a way to reach
//!   the BeSt address nearest to them, the way `/geocode` uses them. Their
//!   names, positions and ids are never returned.
//!
//! Keys are analyzed names (`query_builder::analyze`) starting at each of
//! a name's first `MAX_SUFFIX_WORDS` words, so "loi" finds "Rue de la
//! Loi". They go into an `fst::Map` whose values point to postings. A
//! query's last word is a prefix, unless the query ends with a space. Its
//! earlier words are whole words (or their abbreviation forms). Words
//! after the street narrow it by municipality, part of municipality or
//! postal name. A house number lists the street's buildings whose number
//! starts with it.
//!
//! A prefix shared by more than `COMMON_KEYS` keys ("k", "rue ", "rue
//! de") matches too many to rank per query. Its best `TOP` entries are
//! precomputed, overall and per cell of a ~20 km grid (`CELL_TOP` each),
//! and used when no place word or postal code narrows the query. With a
//! `focus_point`, the cells around it join the overall list, so nearby
//! streets compete with the popular ones.
//!
//! A street's popularity is its number of buildings, not addresses: an
//! apartment block's box-level addresses would otherwise put Brussels
//! streets first everywhere.

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};
use std::ops::Bound;
use std::sync::Arc;

use fst::{Automaton, IntoStreamer, Map, MapBuilder, Streamer};
use serde::Serialize;
use tantivy::collector::TopDocs;
use tantivy::columnar::Column;
use tantivy::query::{BooleanQuery, Occur, Query, RangeQuery, TermQuery};
use tantivy::schema::{IndexRecordOption, Value};
use tantivy::{DocAddress, Index, Searcher, TantivyDocument, Term};

use crabfinder_core::house_number::leading_number;
use crabfinder_core::{Address, Coordinates, Language, MultilingualName};
use crabfinder_indexer::index::{CrabSchema, FIELD_NOTABILITY_SCORE};
use crabfinder_indexer::localities::{Locality, LocalityKind};

use crate::abbreviation::AbbreviationMap;
use crate::focus::FOCUS_HALF_LIFE_M;
use crate::landmarks::LandmarkIndex;
use crate::mid_street::{MidStreetIndex, street_key_from_bgid};
use crate::query_builder::analyze;
use crate::spatial::{SpatialIndex, haversine_m};

/// A prefix shared by more keys than this is precomputed.
const COMMON_KEYS: usize = 2_000;
/// Entries kept per common prefix, overall.
const TOP: usize = 64;
/// Entries kept per common prefix and grid cell.
const CELL_TOP: usize = 32;
/// Grid cell, in degrees (~22 × 21 km in Belgium).
const CELL_LAT: f64 = 0.2;
const CELL_LON: f64 = 0.3;
/// Keys read per prefix stream, at most.
const MAX_STREAM_KEYS: usize = 50_000;
/// A name is keyed from each of its first words (`rue de la loi`, `de la
/// loi`, `la loi`, `loi`).
const MAX_SUFFIX_WORDS: usize = 4;
/// Abbreviation combinations tried per query.
const MAX_FORMS: usize = 8;
/// Streets whose buildings an address query lists.
const ADDRESS_STREETS: usize = 3;
/// Units read per street in address mode.
const MAX_UNITS: usize = 300;
/// Suggestions per request, at most.
pub const MAX_LIMIT: usize = 20;

/// Bonus of a key that starts the name (`avenue louise` for "aven").
const FULL_BONUS: f32 = 3.0;
/// Bonus of a whole name typed out in full.
const EXACT_BONUS: f32 = 2.0;
/// Bonus per place word that matched.
const CITY_BONUS: f32 = 1.5;
/// Bonus at the focus point, decaying like `/geocode`'s.
const FOCUS_BONUS: f32 = 4.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SuggestionKind {
    Street,
    Address,
    Locality,
    Landmark,
}

enum Target {
    Street {
        street_key: Box<str>,
        postal_code: Box<str>,
    },
    Locality {
        kind: LocalityKind,
        name: MultilingualName,
        postal_codes: Vec<String>,
    },
    /// An OSM landmark (`--pois`): only leads to `Entry::rep`.
    Landmark,
}

struct Entry {
    target: Target,
    /// A BeSt address on the street, or the BeSt address nearest to the
    /// place. `None` only for a place when the R-tree is off.
    rep: Option<DocAddress>,
    /// The street's median-number point, the place's centre: ranks by
    /// focus, and is the street suggestion's position.
    lat: f64,
    lon: f64,
    count: u32,
    /// Query-independent part of the score.
    stat: f32,
    /// Analyzed place words (streets only): municipality, part of
    /// municipality and postal names.
    city: Box<[Box<str>]>,
}

impl Entry {
    fn is_street(&self) -> bool {
        matches!(self.target, Target::Street { .. })
    }

    /// OSM-derived (`--places` village / hamlet…, `--pois` landmark).
    fn is_osm(&self) -> bool {
        match &self.target {
            Target::Locality { kind, .. } => kind.is_osm(),
            Target::Landmark => true,
            Target::Street { .. } => false,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Posting {
    entry: u32,
    /// The key is the whole name from its first word.
    full: bool,
    /// Language of the name form the key came from: the suggestion's
    /// label follows what was typed when no `lang` is asked.
    lang: Option<Language>,
}

/// See the module docs.
pub struct SuggestIndex {
    entries: Vec<Entry>,
    keys: Map<Vec<u8>>,
    postings: Vec<Box<[Posting]>>,
    /// Prefixes shared by more than `COMMON_KEYS` keys.
    common: HashMap<String, Top>,
    n_streets: usize,
}

/// The best postings of a common prefix.
struct Top {
    all: Box<[Posting]>,
    cells: HashMap<Cell, Box<[Posting]>>,
}

type Cell = (i16, i16);

fn cell(lat: f64, lon: f64) -> Cell {
    (
        (lat / CELL_LAT).floor() as i16,
        (lon / CELL_LON).floor() as i16,
    )
}

pub type SharedSuggestIndex = Arc<SuggestIndex>;

impl Default for SuggestIndex {
    fn default() -> Self {
        Self::empty()
    }
}

impl SuggestIndex {
    pub fn empty() -> Self {
        Self {
            entries: Vec::new(),
            keys: Map::default(),
            postings: Vec::new(),
            common: HashMap::new(),
            n_streets: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn n_streets(&self) -> usize {
        self.n_streets
    }

    pub fn n_keys(&self) -> usize {
        self.keys.len()
    }

    /// Precomputed common prefixes.
    pub fn n_common(&self) -> usize {
        self.common.len()
    }
}

// --- Build -------------------------------------------------------------------

struct StreetAcc {
    street_key: String,
    postal_code: String,
    rep: DocAddress,
    names: MultilingualName,
    places: Vec<String>,
    count: u32,
    /// Distinct buildings (a street's documents mostly come in building
    /// order, so a change of `building_group_id` is a new building).
    buildings: u32,
    last_bgid: String,
    notability: f64,
    lat: f64,
    lon: f64,
}

/// Collects the streets from every stored document (fed by the
/// address-count pass, `AddressCountIndex::build_visiting`, so the store
/// is read once).
pub struct SuggestBuilder {
    by_key: HashMap<String, usize>,
    streets: Vec<StreetAcc>,
    last: Option<(String, String, usize)>,
    notability: Vec<Option<Column<f64>>>,
}

impl SuggestBuilder {
    pub fn new(searcher: &Searcher) -> Self {
        let notability = searcher
            .segment_readers()
            .iter()
            .map(|s| s.fast_fields().f64(FIELD_NOTABILITY_SCORE).ok())
            .collect();
        Self {
            by_key: HashMap::new(),
            streets: Vec::new(),
            last: None,
            notability,
        }
    }

    /// Count `doc` (at `addr`) on its street.
    pub fn visit(&mut self, schema: &CrabSchema, doc: &TantivyDocument, addr: DocAddress) {
        let text = |f| doc.get_first(f).and_then(|v| v.as_str());
        let (Some(bgid), Some(pc)) = (text(schema.building_group_id), text(schema.postal_code))
        else {
            return;
        };
        let Some(key) = street_key_from_bgid(bgid) else {
            return;
        };
        let notability = self
            .notability
            .get(addr.segment_ord as usize)
            .and_then(|c| c.as_ref()?.first(addr.doc_id))
            .unwrap_or(1.0);
        // Documents of a street mostly come in a row.
        let idx = match &self.last {
            Some((k, p, i)) if k == key && p == pc => Some(*i),
            _ => self.by_key.get(&format!("{key}\u{0}{pc}")).copied(),
        };
        if let Some(i) = idx {
            let s = &mut self.streets[i];
            s.count += 1;
            if s.last_bgid != bgid {
                s.buildings += 1;
                s.last_bgid = bgid.to_string();
            }
            s.notability = s.notability.max(notability);
            self.last = Some((key.to_string(), pc.to_string(), i));
            return;
        }
        let Some(a) = text(schema.payload).and_then(|p| serde_json::from_str::<Address>(p).ok())
        else {
            return;
        };
        let (lat, lon) = match a.coordinates {
            Coordinates::Wgs84(w) => (w.lat, w.lon),
            Coordinates::Lambert72(_) => (0.0, 0.0),
        };
        let names = |n: &MultilingualName| -> Vec<String> {
            forms(n).map(|(_, s)| s.to_string()).collect()
        };
        let mut places = names(&a.municipality_name);
        places.extend(names(&a.postal_name));
        if let Some(p) = &a.part_of_municipality_name {
            places.extend(names(p));
        }
        let i = self.streets.len();
        self.streets.push(StreetAcc {
            street_key: key.to_string(),
            postal_code: pc.to_string(),
            rep: addr,
            names: a.street_name.clone(),
            places,
            count: 1,
            buildings: 1,
            last_bgid: bgid.to_string(),
            notability,
            lat,
            lon,
        });
        self.by_key.insert(format!("{key}\u{0}{pc}"), i);
        self.last = Some((key.to_string(), pc.to_string(), i));
    }

    /// The finished index: the streets, the BeSt localities, and the OSM
    /// places and landmarks when loaded. `spatial` finds the BeSt address
    /// nearest to each place (empty: places carry no address).
    pub fn finish(
        self,
        index: &Index,
        mid_street: &MidStreetIndex,
        localities: &[Locality],
        landmarks: &LandmarkIndex,
        spatial: &SpatialIndex,
    ) -> SuggestIndex {
        let mut entries = Vec::new();
        let mut keyed: BTreeMap<String, Vec<Posting>> = BTreeMap::new();
        let mut add_keys = |names: &MultilingualName, entry: u32| {
            for (lang, name) in forms(names) {
                let tokens = analyze(index, name);
                for i in 0..tokens.len().min(MAX_SUFFIX_WORDS) {
                    let full = i == 0;
                    let postings = keyed.entry(tokens[i..].join(" ")).or_default();
                    match postings.iter_mut().find(|p| p.entry == entry) {
                        Some(p) if full && !p.full => {
                            p.full = true;
                            p.lang = Some(lang);
                        }
                        Some(_) => {}
                        None => postings.push(Posting {
                            entry,
                            full,
                            lang: Some(lang),
                        }),
                    }
                }
            }
        };

        let n_streets = self.streets.len();
        for s in self.streets {
            let (lat, lon) = mid_street
                .lookup(&s.street_key, &s.postal_code)
                .unwrap_or((s.lat, s.lon));
            let mut city: Vec<Box<str>> = s
                .places
                .iter()
                .flat_map(|p| analyze(index, p))
                .map(String::into_boxed_str)
                .collect();
            city.sort();
            city.dedup();
            add_keys(&s.names, entries.len() as u32);
            entries.push(Entry {
                target: Target::Street {
                    street_key: s.street_key.into(),
                    postal_code: s.postal_code.into(),
                },
                rep: Some(s.rep),
                lat,
                lon,
                count: s.count,
                stat: popularity(s.buildings) + 1.5 * (s.notability.max(1.0).ln() as f32),
                city: city.into(),
            });
        }

        let nearest = |lat, lon| {
            spatial
                .nearest(lat, lon, 1, None)
                .first()
                .map(|h| h.doc_addr)
        };
        for l in localities {
            add_keys(&l.name, entries.len() as u32);
            entries.push(Entry {
                target: Target::Locality {
                    kind: l.kind,
                    name: l.name.clone(),
                    postal_codes: l.postal_codes.clone(),
                },
                rep: nearest(l.lat, l.lon),
                lat: l.lat,
                lon: l.lon,
                count: l.address_count,
                stat: popularity(l.address_count) + kind_bonus(l.kind),
                city: Box::new([]),
            });
        }
        for lm in landmarks.all() {
            add_keys(&lm.name, entries.len() as u32);
            entries.push(Entry {
                target: Target::Landmark,
                rep: nearest(lm.lat, lm.lon),
                lat: lm.lat,
                lon: lm.lon,
                count: 0,
                stat: 4.0 + f32::from(lm.names),
                city: Box::new([]),
            });
        }

        let rank = |p: &Posting| entries[p.entry as usize].stat + full_bonus(p.full);
        let mut builder = MapBuilder::memory();
        let mut postings = Vec::with_capacity(keyed.len());
        let mut sorted_keys = Vec::with_capacity(keyed.len());
        for (i, (key, ps)) in keyed.into_iter().enumerate() {
            let _ = builder.insert(&key, i as u64);
            postings.push(ps.into_boxed_slice());
            sorted_keys.push(key);
        }
        let common = common_prefixes(&sorted_keys)
            .into_iter()
            .map(|(prefix, range)| {
                let mut all: Vec<Posting> = Vec::new();
                let mut cells: HashMap<Cell, Vec<Posting>> = HashMap::new();
                for p in postings[range].iter().flat_map(|ps| ps.iter()) {
                    // Keep the lists short while building.
                    all.push(*p);
                    if all.len() > 8 * TOP {
                        trim(&mut all, &rank, TOP);
                    }
                    let e = &entries[p.entry as usize];
                    let c = cells.entry(cell(e.lat, e.lon)).or_default();
                    c.push(*p);
                    if c.len() > 8 * CELL_TOP {
                        trim(c, &rank, CELL_TOP);
                    }
                }
                trim(&mut all, &rank, TOP);
                let cells = cells
                    .into_iter()
                    .map(|(k, mut v)| {
                        trim(&mut v, &rank, CELL_TOP);
                        (k, v.into_boxed_slice())
                    })
                    .collect();
                let all = all.into_boxed_slice();
                (prefix, Top { all, cells })
            })
            .collect();
        let keys = builder
            .into_inner()
            .ok()
            .and_then(|b| Map::new(b).ok())
            .unwrap_or_default();
        SuggestIndex {
            entries,
            keys,
            postings,
            common,
            n_streets,
        }
    }
}

/// The prefixes shared by more than `COMMON_KEYS` of the sorted `keys`,
/// with their key range.
fn common_prefixes(keys: &[String]) -> Vec<(String, std::ops::Range<usize>)> {
    // Open prefixes of the current key: (end byte, first key index).
    let mut open: Vec<(usize, usize)> = Vec::new();
    let mut out = Vec::new();
    let mut close = |open: &mut Vec<(usize, usize)>, keep: usize, prev: &str, i: usize| {
        while let Some(&(end, start)) = open.last() {
            if end <= keep {
                break;
            }
            open.pop();
            if i - start > COMMON_KEYS {
                out.push((prev[..end].to_string(), start..i));
            }
        }
    };
    for (i, key) in keys.iter().enumerate() {
        let lcp = match i.checked_sub(1).map(|j| keys[j].as_str()) {
            Some(prev) => {
                let n = prev
                    .bytes()
                    .zip(key.bytes())
                    .take_while(|(a, b)| a == b)
                    .count();
                // Back to a char boundary of both.
                let n = (0..=n)
                    .rev()
                    .find(|&n| key.is_char_boundary(n))
                    .unwrap_or(0);
                close(&mut open, n, prev, i);
                n
            }
            None => 0,
        };
        for (end, _) in key.char_indices().skip(1).chain([(key.len(), ' ')]) {
            if end > lcp {
                open.push((end, i));
            }
        }
    }
    if let Some(last) = keys.last() {
        close(&mut open, 0, last, keys.len());
    }
    out
}

/// Each language form of `n`.
fn forms(n: &MultilingualName) -> impl Iterator<Item = (Language, &str)> {
    [
        (Language::Fr, &n.fr),
        (Language::Nl, &n.nl),
        (Language::De, &n.de),
    ]
    .into_iter()
    .filter_map(|(l, s)| Some((l, s.as_deref()?)))
}

fn popularity(count: u32) -> f32 {
    (f64::from(count).ln_1p() * 0.5) as f32
}

fn full_bonus(full: bool) -> f32 {
    if full { FULL_BONUS } else { 0.0 }
}

fn kind_bonus(kind: LocalityKind) -> f32 {
    match kind {
        LocalityKind::Municipality => 3.0,
        LocalityKind::PartOfMunicipality => 2.0,
        LocalityKind::PostalArea => 1.5,
        _ => 1.0,
    }
}

/// Best `n` postings by `rank`, one per entry.
fn trim(list: &mut Vec<Posting>, rank: &impl Fn(&Posting) -> f32, n: usize) {
    list.sort_by(|a, b| {
        rank(b)
            .partial_cmp(&rank(a))
            .unwrap_or(Ordering::Equal)
            .then(a.entry.cmp(&b.entry))
    });
    let mut seen = std::collections::HashSet::new();
    list.retain(|p| seen.insert(p.entry));
    list.truncate(n);
}

// --- Query -------------------------------------------------------------------

/// One `/autocomplete` request.
#[derive(Debug, Clone, Default)]
pub struct SuggestQuery {
    pub q: String,
    pub limit: usize,
    pub lang: Option<Language>,
    pub focus: Option<(f64, f64)>,
    /// Hard filter.
    pub postal_code: Option<String>,
}

/// What `suggest` needs from the service state.
pub struct SuggestContext<'a> {
    pub index: &'a Index,
    pub searcher: &'a Searcher,
    pub schema: &'a CrabSchema,
    pub suggest: &'a SuggestIndex,
    pub abbrev: &'a AbbreviationMap,
}

/// One suggestion, every field BeSt's.
#[derive(Debug, Clone, Serialize)]
pub struct Suggestion {
    pub kind: SuggestionKind,
    /// The suggestion in `lang` on one line.
    pub label: String,
    pub score: f32,
    /// `[lon, lat]`: the street's median-number BeSt address, the
    /// address, or the BeSt address nearest to the place.
    pub coordinates: [f64; 2],
    /// The BeSt address payload (address, locality and landmark kinds; for
    /// a street, the address the position belongs to is not meaningful,
    /// so the street fields only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub address: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub street: Option<StreetOut>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locality: Option<LocalityOut>,
    /// The words matched an OpenStreetMap name (`--places` / `--pois`);
    /// what is returned is the BeSt address nearest to it.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub osm_hint: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct StreetOut {
    pub street_id: serde_json::Value,
    pub street_name: serde_json::Value,
    pub postal_code: String,
    pub postal_name: serde_json::Value,
    pub municipality_id: serde_json::Value,
    pub municipality_name: serde_json::Value,
    pub region: serde_json::Value,
    pub address_count: u32,
    /// `GET /building/{street_key}|{number}/units` lists a building.
    pub street_key: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct LocalityOut {
    pub kind: &'static str,
    pub name: MultilingualName,
    pub postal_codes: Vec<String>,
    pub address_count: u32,
}

/// Which part of the query its last token is in.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum Last {
    #[default]
    Words,
    House,
    Postal,
    Places,
}

/// A query read for suggestion: `[postal] words [house] [postal|places…]`.
#[derive(Debug, Default, PartialEq)]
struct Parsed {
    words: Vec<String>,
    house: Option<String>,
    postal: Option<String>,
    places: Vec<String>,
    /// The query's last token is a prefix (no trailing space).
    open: bool,
    last: Last,
}

fn is_postal(t: &str) -> bool {
    t.len() == 4 && t.bytes().all(|b| b.is_ascii_digit())
}

fn parse(index: &Index, q: &str) -> Parsed {
    let open = !q.ends_with(|c: char| c.is_whitespace() || c == ',');
    let numeric = |t: &str| t.starts_with(|c: char| c.is_ascii_digit());
    let mut p = Parsed {
        open,
        ..Default::default()
    };
    let mut it = analyze(index, q).into_iter().peekable();
    if let Some(t) = it.next_if(|t| is_postal(t)) {
        p.postal = Some(t);
        p.last = Last::Postal;
    }
    while let Some(t) = it.next_if(|t| !numeric(t)) {
        p.words.push(t);
        p.last = Last::Words;
    }
    if !p.words.is_empty()
        && let Some(t) = it.next()
    {
        p.house = Some(t);
        p.last = Last::House;
    }
    for t in it {
        if is_postal(&t) && p.postal.is_none() {
            p.postal = Some(t);
            p.last = Last::Postal;
        } else {
            p.places.push(t);
            p.last = Last::Places;
        }
    }
    p
}

/// Every token as a street word ("Rue du 11 Novembre").
fn all_words(index: &Index, q: &str, open: bool) -> Parsed {
    Parsed {
        words: analyze(index, q),
        open,
        ..Default::default()
    }
}

#[derive(Debug, Clone, Copy)]
struct Hit {
    entry: u32,
    full: bool,
    /// The typed words end the name (`louise` for Avenue Louise).
    whole: bool,
    /// The whole name, typed out.
    exact: bool,
    lang: Option<Language>,
}

/// A ranked entry.
#[derive(Debug, Clone, Copy)]
struct Ranked {
    entry: u32,
    score: f32,
    whole: bool,
    lang: Option<Language>,
}

impl SuggestIndex {
    /// Entries whose name runs through `words` (the last one a prefix
    /// when `prefix`).
    fn lookup(
        &self,
        words: &[String],
        prefix: bool,
        narrowed: bool,
        focus: Option<(f64, f64)>,
        abbrev: &AbbreviationMap,
    ) -> Vec<Hit> {
        let Some((last, init)) = words.split_last() else {
            return Vec::new();
        };
        let mut forms: Vec<String> = vec![String::new()];
        for w in init {
            let expanded = abbrev.expand(w);
            forms = forms
                .iter()
                .flat_map(|f| expanded.iter().map(move |e| join(f, e)))
                .take(MAX_FORMS)
                .collect();
        }
        let last_forms = if prefix {
            vec![last.clone()]
        } else {
            abbrev.expand(last)
        };
        let mut out: HashMap<u32, Hit> = HashMap::new();
        let mut add = |p: &Posting, whole: bool| {
            let h = out.entry(p.entry).or_insert(Hit {
                entry: p.entry,
                full: false,
                whole: false,
                exact: false,
                lang: p.lang,
            });
            if p.full && !h.full {
                h.lang = p.lang;
            }
            h.full |= p.full;
            h.whole |= whole;
            h.exact |= whole && p.full;
        };
        for f in &forms {
            for l in &last_forms {
                let s = join(f, l);
                if let Some(i) = self.keys.get(&s) {
                    for p in self.postings[i as usize].iter() {
                        add(p, true);
                    }
                }
                let start = if prefix { s.clone() } else { format!("{s} ") };
                // Nothing narrows a prefix this common: its precomputed
                // best, and the best around the focus point.
                if !narrowed && let Some(top) = self.common.get(&start) {
                    let near =
                        focus
                            .map(|(lat, lon)| cell(lat, lon))
                            .into_iter()
                            .flat_map(|(r, c)| {
                                (-1..=1)
                                    .flat_map(move |dr| (-1..=1).map(move |dc| (r + dr, c + dc)))
                            });
                    let near = near
                        .filter_map(|k| top.cells.get(&k))
                        .flat_map(|v| v.iter());
                    for p in top.all.iter().chain(near) {
                        add(p, false);
                    }
                    continue;
                }
                let aut = fst::automaton::Str::new(&start).starts_with();
                let mut stream = self.keys.search(aut).into_stream();
                let mut n = 0;
                while let Some((key, i)) = stream.next() {
                    n += 1;
                    if n > MAX_STREAM_KEYS {
                        break;
                    }
                    if key == s.as_bytes() {
                        continue;
                    }
                    for p in self.postings[i as usize].iter() {
                        add(p, false);
                    }
                }
            }
        }
        out.into_values().collect()
    }

    fn score(&self, h: &Hit, places: &[String], last_open: bool, q: &SuggestQuery) -> Option<f32> {
        let e = &self.entries[h.entry as usize];
        if let Some(pc) = &q.postal_code {
            let ok = match &e.target {
                Target::Street { postal_code, .. } => &**postal_code == pc,
                Target::Locality { postal_codes, .. } => postal_codes.contains(pc),
                Target::Landmark => false,
            };
            if !ok {
                return None;
            }
        }
        let mut s = e.stat + full_bonus(h.full) + if h.exact { EXACT_BONUS } else { 0.0 };
        for (i, w) in places.iter().enumerate() {
            let open = last_open && i + 1 == places.len();
            let hit = e
                .city
                .iter()
                .any(|c| &**c == w || (open && c.starts_with(w.as_str())));
            if !hit {
                return None;
            }
            s += CITY_BONUS;
        }
        if let Some((lat, lon)) = q.focus {
            let d = haversine_m(lat, lon, e.lat, e.lon);
            s += FOCUS_BONUS * (-d / FOCUS_HALF_LIFE_M).exp() as f32;
        }
        Some(s)
    }

    /// Ranked entries for `p`: the street words are the longest run of
    /// leading words that names something; the words after it narrow by
    /// place.
    fn ranked(
        &self,
        p: &Parsed,
        q: &SuggestQuery,
        abbrev: &AbbreviationMap,
        streets_only: bool,
    ) -> Vec<Ranked> {
        let words_open = p.open && p.last == Last::Words;
        for k in (1..=p.words.len()).rev() {
            let street_open = words_open && k == p.words.len();
            let mut places: Vec<String> = p.words[k..].to_vec();
            places.extend(p.places.iter().cloned());
            let places_open = p.open && (p.last == Last::Places || words_open && k < p.words.len());
            let q2 = SuggestQuery {
                postal_code: q.postal_code.clone().or(p.postal.clone()),
                ..q.clone()
            };
            let narrowed = !places.is_empty() || q2.postal_code.is_some();
            let mut out: Vec<Ranked> = self
                .lookup(&p.words[..k], street_open, narrowed, q.focus, abbrev)
                .iter()
                .filter(|h| !streets_only || self.entries[h.entry as usize].is_street())
                .filter_map(|h| {
                    Some(Ranked {
                        entry: h.entry,
                        score: self.score(h, &places, places_open, &q2)?,
                        whole: h.whole,
                        lang: h.lang,
                    })
                })
                .collect();
            if !out.is_empty() {
                out.sort_by(|a, b| {
                    b.score
                        .partial_cmp(&a.score)
                        .unwrap_or(Ordering::Equal)
                        .then(a.entry.cmp(&b.entry))
                });
                return out;
            }
        }
        Vec::new()
    }
}

fn join(a: &str, b: &str) -> String {
    if a.is_empty() {
        b.to_string()
    } else {
        format!("{a} {b}")
    }
}

/// Suggestions for `q`, best first.
pub fn suggest(ctx: &SuggestContext<'_>, q: &SuggestQuery) -> tantivy::Result<Vec<Suggestion>> {
    let limit = q.limit.clamp(1, MAX_LIMIT);
    let mut p = parse(ctx.index, &q.q);
    let named = |p: &Parsed| {
        ctx.suggest
            .ranked(&all_words(ctx.index, &q.q, p.open), q, ctx.abbrev, false)
    };
    // A name starting with a number ("11 Novemberstraat").
    if p.words.is_empty() {
        return entries_out(ctx, named(&p), limit, q.lang);
    }
    if let Some(house) = p.house.clone() {
        let streets = ctx.suggest.ranked(&p, q, ctx.abbrev, true);
        let whole: Vec<Ranked> = streets.iter().filter(|r| r.whole).copied().collect();
        // A number inside a street name ("Rue du 11 Novembre"): the words
        // don't end a street name, or more words follow the number.
        if whole.is_empty() || !p.places.is_empty() {
            // A later number is the house number ("Rue du 11 Novembre 5").
            if let Some(alt) = later_house(ctx.index, &q.q, p.open, p.words.len()) {
                let streets = ctx.suggest.ranked(&alt, q, ctx.abbrev, true);
                let whole: Vec<Ranked> = streets.into_iter().filter(|r| r.whole).collect();
                let out = buildings(ctx, &whole, alt.house.as_deref().unwrap_or(""), q, limit)?;
                if !out.is_empty() {
                    return Ok(out);
                }
            }
            let alt = named(&p);
            if !alt.is_empty() {
                return entries_out(ctx, alt, limit, q.lang);
            }
        }
        let streets = if whole.is_empty() { streets } else { whole };
        let out = buildings(ctx, &streets, &house, q, limit)?;
        if !out.is_empty() {
            return Ok(out);
        }
        // No such number: a 4-digit one was the postal code.
        if is_postal(&house) && p.postal.is_none() {
            p.postal = Some(house);
            p.house = None;
            if p.last == Last::House {
                p.last = Last::Postal;
            }
        }
    }
    let mut ranked = ctx.suggest.ranked(&p, q, ctx.abbrev, false);
    if ranked.is_empty() && (p.house.is_some() || !p.places.is_empty()) {
        ranked = named(&p);
    }
    entries_out(ctx, ranked, limit, q.lang)
}

/// The buildings of the best `streets` whose number starts with `house`;
/// the number as typed first, from whichever street.
fn buildings(
    ctx: &SuggestContext<'_>,
    streets: &[Ranked],
    house: &str,
    q: &SuggestQuery,
    limit: usize,
) -> tantivy::Result<Vec<Suggestion>> {
    let mut found: Vec<(bool, f32, Option<Language>, DocAddress)> = Vec::new();
    for r in streets.iter().take(ADDRESS_STREETS) {
        for (rank, (hn, doc)) in units(ctx, r.entry, house, limit)?.into_iter().enumerate() {
            let exact = hn.eq_ignore_ascii_case(house);
            found.push((exact, r.score - rank as f32 * 1e-3, r.lang, doc));
        }
    }
    found.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then(b.1.partial_cmp(&a.1).unwrap_or(Ordering::Equal))
    });
    found
        .into_iter()
        .take(limit)
        .map(|(_, score, lang, doc)| address_suggestion(ctx, doc, score, q.lang.or(lang)))
        .collect()
}

/// `q` read with its last house number after more than `words` street
/// words (a trailing 4-digit token after it is the postal code), or
/// `None` when there is no such number.
fn later_house(index: &Index, q: &str, open: bool, words: usize) -> Option<Parsed> {
    let tokens = analyze(index, q);
    let numeric = |t: &String| t.starts_with(|c: char| c.is_ascii_digit());
    let mut at: Vec<usize> = (0..tokens.len()).filter(|&i| numeric(&tokens[i])).collect();
    let mut postal = None;
    if at.len() > 1 && at.last().is_some_and(|&i| is_postal(&tokens[i])) {
        postal = at.pop();
    }
    let i = *at.last()?;
    if i <= words {
        return None;
    }
    let places: Vec<String> = tokens[i + 1..]
        .iter()
        .enumerate()
        .filter(|(j, _)| Some(i + 1 + j) != postal)
        .map(|(_, t)| t.clone())
        .collect();
    let last = match tokens.len() - 1 {
        n if n == i => Last::House,
        n if Some(n) == postal => Last::Postal,
        _ => Last::Places,
    };
    Some(Parsed {
        words: tokens[..i].to_vec(),
        house: Some(tokens[i].clone()),
        postal: postal.map(|j| tokens[j].clone()),
        places,
        open,
        last,
    })
}

/// The suggestions of `ranked` entries, up to `limit`.
fn entries_out(
    ctx: &SuggestContext<'_>,
    ranked: Vec<Ranked>,
    limit: usize,
    lang: Option<Language>,
) -> tantivy::Result<Vec<Suggestion>> {
    let mut out = Vec::new();
    // Two suggestions leading to the same BeSt address say the same
    // thing; so do two places of the same name (the municipality of Namur
    // and its part of municipality Namur).
    let mut seen = std::collections::HashSet::new();
    let mut places = std::collections::HashSet::new();
    for r in ranked {
        if out.len() >= limit {
            break;
        }
        let e = &ctx.suggest.entries[r.entry as usize];
        if !e.is_street() && e.rep.is_some_and(|a| !seen.insert(a)) {
            continue;
        }
        if let Some(s) = entry_suggestion(ctx, e, r.score, lang.or(r.lang))?
            && (s.kind != SuggestionKind::Locality || places.insert(s.label.clone()))
        {
            out.push(s);
        }
    }
    Ok(out)
}

/// The buildings of street entry `entry` whose number starts with
/// `prefix`, in number order (one unit each, the one without box first).
fn units(
    ctx: &SuggestContext<'_>,
    entry: u32,
    prefix: &str,
    limit: usize,
) -> tantivy::Result<Vec<(String, DocAddress)>> {
    let Target::Street {
        street_key,
        postal_code,
    } = &ctx.suggest.entries[entry as usize].target
    else {
        return Ok(Vec::new());
    };
    let mut forms = vec![prefix.to_string(), prefix.to_uppercase()];
    forms.dedup();
    let field = ctx.schema.building_group_id;
    let ranges: Vec<(Occur, Box<dyn Query>)> = forms
        .iter()
        .map(|f| -> (Occur, Box<dyn Query>) {
            let lo = format!("{street_key}|{f}");
            let hi = format!("{lo}\u{10FFFF}");
            (
                Occur::Should,
                Box::new(RangeQuery::new(
                    Bound::Included(Term::from_field_text(field, &lo)),
                    Bound::Excluded(Term::from_field_text(field, &hi)),
                )),
            )
        })
        .collect();
    let q = BooleanQuery::new(vec![
        (Occur::Must, Box::new(BooleanQuery::new(ranges))),
        (
            Occur::Must,
            Box::new(TermQuery::new(
                Term::from_field_text(ctx.schema.postal_code, postal_code),
                IndexRecordOption::Basic,
            )),
        ),
    ]);
    let top = ctx
        .searcher
        .search(&q, &TopDocs::with_limit(MAX_UNITS).order_by_score())?;
    // building_group_id → (number, house, box, doc).
    let mut best: HashMap<String, (u32, String, Option<String>, DocAddress)> = HashMap::new();
    for (_, addr) in top {
        let doc = ctx.searcher.doc::<TantivyDocument>(addr)?;
        let text = |f| doc.get_first(f).and_then(|v| v.as_str()).map(String::from);
        let (Some(bgid), Some(hn)) = (
            text(ctx.schema.building_group_id),
            text(ctx.schema.house_number),
        ) else {
            continue;
        };
        let bx = text(ctx.schema.box_number).filter(|b| !b.is_empty());
        let cand = (leading_number(&hn).unwrap_or(u32::MAX), hn, bx, addr);
        // The unit without box, else the lowest box.
        let rank = |b: &Option<String>| (b.is_some(), b.clone());
        match best.get(&bgid) {
            Some(old) if rank(&old.2) <= rank(&cand.2) => {}
            _ => {
                best.insert(bgid, cand);
            }
        }
    }
    let mut v: Vec<_> = best.into_values().collect();
    v.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    Ok(v.into_iter().take(limit).map(|x| (x.1, x.3)).collect())
}

fn load(
    ctx: &SuggestContext<'_>,
    addr: DocAddress,
) -> tantivy::Result<(Address, serde_json::Value, [f64; 2])> {
    let doc = ctx.searcher.doc::<TantivyDocument>(addr)?;
    let payload: serde_json::Value = doc
        .get_first(ctx.schema.payload)
        .and_then(|v| v.as_str())
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();
    let address: Address = serde_json::from_value(payload.clone())
        .map_err(|e| tantivy::TantivyError::InternalError(e.to_string()))?;
    let f = |field| doc.get_first(field).and_then(|v| v.as_f64()).unwrap_or(0.0);
    Ok((address, payload, [f(ctx.schema.lon), f(ctx.schema.lat)]))
}

fn address_suggestion(
    ctx: &SuggestContext<'_>,
    addr: DocAddress,
    score: f32,
    lang: Option<Language>,
) -> tantivy::Result<Suggestion> {
    let (a, payload, coordinates) = load(ctx, addr)?;
    Ok(Suggestion {
        kind: SuggestionKind::Address,
        label: a.display_name(lang),
        score,
        coordinates,
        address: Some(payload),
        street: None,
        locality: None,
        osm_hint: false,
    })
}

fn entry_suggestion(
    ctx: &SuggestContext<'_>,
    e: &Entry,
    score: f32,
    lang: Option<Language>,
) -> tantivy::Result<Option<Suggestion>> {
    let Some(rep) = e.rep else {
        return Ok(None);
    };
    let (mut a, payload, coordinates) = load(ctx, rep)?;
    let s = match &e.target {
        Target::Street {
            street_key,
            postal_code,
        } => {
            a.house_number.clear();
            a.box_number = None;
            Suggestion {
                kind: SuggestionKind::Street,
                label: a.display_name(lang),
                score,
                // The street's median-number BeSt address
                // (`mid-street-v1.bin`), else the representative one.
                coordinates: [e.lon, e.lat],
                address: None,
                street: Some(StreetOut {
                    street_id: payload["street_id"].clone(),
                    street_name: payload["street_name"].clone(),
                    postal_code: postal_code.to_string(),
                    postal_name: payload["postal_name"].clone(),
                    municipality_id: payload["municipality_id"].clone(),
                    municipality_name: payload["municipality_name"].clone(),
                    region: payload["region"].clone(),
                    address_count: e.count,
                    street_key: street_key.to_string(),
                }),
                locality: None,
                osm_hint: false,
            }
        }
        // A BeSt locality: its BeSt name, at the BeSt address nearest to
        // its centre.
        Target::Locality {
            kind,
            name,
            postal_codes,
        } if !kind.is_osm() => Suggestion {
            kind: SuggestionKind::Locality,
            label: pick(name, lang, a.default_language()),
            score,
            coordinates,
            address: Some(payload),
            street: None,
            locality: Some(LocalityOut {
                kind: kind.as_str(),
                name: name.clone(),
                postal_codes: postal_codes.clone(),
                address_count: e.count,
            }),
            osm_hint: false,
        },
        // OSM place or landmark: only the BeSt address nearest to it.
        _ => Suggestion {
            kind: if matches!(e.target, Target::Landmark) {
                SuggestionKind::Landmark
            } else {
                SuggestionKind::Locality
            },
            label: a.display_name(lang),
            score,
            coordinates,
            address: Some(payload),
            street: None,
            locality: None,
            osm_hint: e.is_osm(),
        },
    };
    Ok(Some(s))
}

fn pick(n: &MultilingualName, lang: Option<Language>, fallback: Language) -> String {
    lang.and_then(|l| n.get(l))
        .or_else(|| n.get(fallback))
        .or_else(|| n.primary())
        .unwrap_or_default()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crabfinder_test_support::TestIndex;

    #[test]
    fn parse_splits_words_number_postal_and_places() {
        let ti = TestIndex::build(&[]);
        let p = parse(&ti.index, "Rue de la Loi 16 1000 Bruxel");
        assert_eq!(p.words, ["rue", "de", "la", "loi"]);
        assert_eq!(p.house.as_deref(), Some("16"));
        assert_eq!(p.postal.as_deref(), Some("1000"));
        assert_eq!(p.places, ["bruxel"]);
        assert!(p.open);
        assert_eq!(p.last, Last::Places);
        let p = parse(&ti.index, "Avenue Louise ");
        assert!(!p.open && p.house.is_none());
        assert_eq!(p.last, Last::Words);
        // A leading postal code is no house number.
        let p = parse(&ti.index, "1000 Bruxelles");
        assert_eq!(p.postal.as_deref(), Some("1000"));
        assert_eq!(p.words, ["bruxelles"]);
    }

    #[test]
    fn later_house_takes_the_last_number_after_the_name() {
        let ti = TestIndex::build(&[]);
        let p = later_house(&ti.index, "Rue du 11 Novembre 5 1040 Etter", true, 2).unwrap();
        assert_eq!(p.words, ["rue", "du", "11", "novembre"]);
        assert_eq!(p.house.as_deref(), Some("5"));
        assert_eq!(p.postal.as_deref(), Some("1040"));
        assert_eq!(p.places, ["etter"]);
        assert_eq!(p.last, Last::Places);
        let p = later_house(&ti.index, "Rue du 11 Novembre 5 1040", false, 2).unwrap();
        assert_eq!(p.last, Last::Postal);
        let p = later_house(&ti.index, "Rue du 11 Novembre 5", true, 2).unwrap();
        assert_eq!((p.last, p.postal), (Last::House, None));
        // The only number is the one already read as the house number.
        assert!(later_house(&ti.index, "Rue du 11 Novembre", true, 2).is_none());
        assert!(later_house(&ti.index, "Novembre", true, 0).is_none());
    }

    #[test]
    fn common_prefixes_are_those_of_many_keys() {
        let mut keys: Vec<String> = (0..COMMON_KEYS + 5)
            .map(|i| format!("rue {i:05}"))
            .chain((0..10).map(|i| format!("rué {i}")))
            .chain(["zz".to_string()])
            .collect();
        keys.sort();
        let got = common_prefixes(&keys);
        let names: Vec<&str> = got.iter().map(|(p, _)| p.as_str()).collect();
        // "rue 0" covers every numbered key ("rue 00000" to "rue 02004").
        for p in ["r", "ru", "rue", "rue ", "rue 0"] {
            assert!(names.contains(&p), "{p} missing from {names:?}");
        }
        assert!(!names.iter().any(|p| p.starts_with("rué") || *p == "zz"));
        let (_, r) = got.iter().find(|(p, _)| p == "rue ").unwrap();
        assert_eq!(r.clone(), 0..COMMON_KEYS + 5);
        let (_, r) = got.iter().find(|(p, _)| p == "r").unwrap();
        assert_eq!(r.len(), COMMON_KEYS + 15);
        // Prefixes end on char boundaries.
        assert!(got.iter().all(|(p, _)| p.is_char_boundary(p.len())));
        assert!(common_prefixes(&[]).is_empty());
    }

    #[test]
    fn cells_and_trim() {
        assert_eq!(cell(50.85, 4.35), (254, 14));
        assert_eq!(cell(50.79, 4.35).0, 253);
        let p = |entry, full| Posting {
            entry,
            full,
            lang: None,
        };
        let mut v = vec![
            p(3, false),
            p(1, true),
            p(2, false),
            p(1, false),
            p(4, true),
        ];
        // Rank: full first, then by entry number.
        let rank = |x: &Posting| f32::from(u8::from(x.full)) * 10.0 - x.entry as f32;
        trim(&mut v, &rank, 3);
        let got: Vec<(u32, bool)> = v.iter().map(|x| (x.entry, x.full)).collect();
        assert_eq!(got, [(1, true), (4, true), (2, false)]);
    }
}
