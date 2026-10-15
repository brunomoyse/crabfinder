//! Synthetic eval set generator (file 14 §2 — MVP).
//!
//! Sampling strategy:
//!   - Walk every doc once via per-segment StoreReader::iter() (efficient
//!     sequential read; avoids 6.84M random get_first() calls).
//!   - Per region, build HashMap<bgid, DocAddress> deduplicating Brussels'
//!     per-unit modeling into a per-building pool (file 14 §2 sampling
//!     unit refinement).
//!   - Sort bgid keys for deterministic ordering, then seeded random sample.
//!   - For 200 boosted cross_language samples, iterate the per-region pool
//!     extras and pick the first N where the firing policy says fire.
//!
//! Variant taxonomy: file 14 §2 MVP — exactly 5 variants per random sample,
//! plus 1 cross_language variant per boosted sample.
//!
//! Cross_language firing policy (see `should_fire_cross_language` for the
//! exact contract; mandatory unit tests below).

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result, anyhow};
use rand::SeedableRng;
use rand::seq::IndexedRandom;
use rand_chacha::ChaCha20Rng;
use tantivy::schema::{Field, Value};
use tantivy::{DocAddress, Index, TantivyDocument};
use tracing::{info, warn};

use crabfinder_core::{Address, MultilingualName, Region};

use crate::eval_set::{SyntheticCorpus, SyntheticEvalSet, SyntheticQuery, VariantType};

/// Field handles we need from the index schema (Phase 5 v3).
struct Fields {
    region: Field,
    building_group_id: Field,
    payload: Field,
}

impl Fields {
    fn resolve(index: &Index) -> Result<Self> {
        let s = index.schema();
        Ok(Self {
            region: s.get_field("region")?,
            building_group_id: s.get_field("building_group_id")?,
            payload: s.get_field("payload")?,
        })
    }
}

/// Per-region target sample counts (file 14 §2 stratification table).
struct Targets {
    brussels: usize,
    flanders: usize,
    wallonia: usize,
    boosted_cross_language: usize,
    /// Extra samples whose house number is ≥ 1000 — too rare in the random
    /// pool to measure the 4-digit house-number / postal-code clash.
    boosted_high_house_number: usize,
}

const MVP_TARGETS: Targets = Targets {
    brussels: 100,
    flanders: 250,
    wallonia: 150,
    boosted_cross_language: 200,
    boosted_high_house_number: 100,
};

pub fn generate_mvp(
    index_path: &Path,
    output_path: &Path,
    seed: u64,
    dump_date: &str,
    scale: f64,
) -> Result<()> {
    info!(
        ?index_path,
        ?output_path,
        seed,
        scale,
        "generator: starting"
    );

    let targets = Targets {
        brussels: ((MVP_TARGETS.brussels as f64) * scale).round() as usize,
        flanders: ((MVP_TARGETS.flanders as f64) * scale).round() as usize,
        wallonia: ((MVP_TARGETS.wallonia as f64) * scale).round() as usize,
        boosted_cross_language: ((MVP_TARGETS.boosted_cross_language as f64) * scale).round()
            as usize,
        boosted_high_house_number: ((MVP_TARGETS.boosted_high_house_number as f64) * scale).round()
            as usize,
    };
    info!(
        brussels = targets.brussels,
        flanders = targets.flanders,
        wallonia = targets.wallonia,
        boosted = targets.boosted_cross_language,
        "generator: effective per-region targets after scale",
    );

    // 1. Open index, resolve fields
    let index = Index::open_in_dir(index_path)
        .with_context(|| format!("opening Tantivy index at {index_path:?}"))?;
    let fields = Fields::resolve(&index)?;
    let reader = index.reader().context("acquiring index reader")?;
    let searcher = reader.searcher();
    info!(num_docs = searcher.num_docs(), "generator: index opened");

    // 2. Walk all docs once, building per-region bgid pools
    let pools = build_pools(&searcher, &fields)?;
    let brussels_count = pools.get(&Region::Brussels).map(|m| m.len()).unwrap_or(0);
    let flanders_count = pools.get(&Region::Flanders).map(|m| m.len()).unwrap_or(0);
    let wallonia_count = pools.get(&Region::Wallonia).map(|m| m.len()).unwrap_or(0);
    info!(
        brussels = brussels_count,
        flanders = flanders_count,
        wallonia = wallonia_count,
        "generator: bgid pools built"
    );

    // 3. Sample bgids per region with seeded RNG
    let mut rng = ChaCha20Rng::seed_from_u64(seed);
    let brussels_samples = sample_bgids(&pools, Region::Brussels, targets.brussels, &mut rng)?;
    let flanders_samples = sample_bgids(&pools, Region::Flanders, targets.flanders, &mut rng)?;
    let wallonia_samples = sample_bgids(&pools, Region::Wallonia, targets.wallonia, &mut rng)?;
    let random_sample_addrs: Vec<DocAddress> = brussels_samples
        .iter()
        .chain(flanders_samples.iter())
        .chain(wallonia_samples.iter())
        .copied()
        .collect();
    info!(
        total_random = random_sample_addrs.len(),
        "generator: random sample drawn"
    );

    // 4. Materialize addresses for the random samples
    let mut random_addresses: Vec<(DocAddress, Address)> =
        Vec::with_capacity(random_sample_addrs.len());
    let mut chosen_bgids_random: std::collections::HashSet<String> =
        std::collections::HashSet::new();
    for addr in &random_sample_addrs {
        let address = read_address(&searcher, &fields, *addr)?;
        chosen_bgids_random.insert(address.building_group_id.0.clone());
        random_addresses.push((*addr, address));
    }

    // 5. Generate variants per random sample
    let mut queries: Vec<SyntheticQuery> = Vec::new();
    let mut next_id: usize = 1;
    for (_addr, address) in &random_addresses {
        for (variant_type, query_string) in generate_variants(address, seed) {
            queries.push(SyntheticQuery {
                id: format!("mvp-v1-{:06}", next_id),
                sample_best_id: address.id.to_string(),
                sample_building_group_id: address.building_group_id.0.clone(),
                sample_region: region_iso(address.region).to_string(),
                sample_street_primary_lang: street_primary_lang(&address.street_name)
                    .map(|l| l.to_string())
                    .unwrap_or_else(|| "?".to_string()),
                variant_type,
                boosted: false,
                query_string,
                expected_best_id: address.id.to_string(),
                expected_building_group_id: address.building_group_id.0.clone(),
            });
            next_id += 1;
        }
    }

    // 6. Boosted cross_language: scan additional pool entries until we collect
    //    `boosted_cross_language` samples where the firing policy fires AND
    //    bgid wasn't already used in the random pool.
    let boosted_addresses = collect_boosted(
        &searcher,
        &fields,
        &pools,
        &chosen_bgids_random,
        targets.boosted_cross_language,
        &mut rng,
        should_fire_cross_language,
    )?;
    info!(
        boosted_collected = boosted_addresses.len(),
        target = targets.boosted_cross_language,
        "generator: boosted cross_language pool collected"
    );

    // For boosted samples, emit ONLY the cross_language variant.
    for (_addr, address) in &boosted_addresses {
        let variant = build_cross_language(address)
            .expect("collect_boosted only returns addresses where firing succeeds");
        queries.push(SyntheticQuery {
            id: format!("mvp-v1-{:06}", next_id),
            sample_best_id: address.id.0.to_string(),
            sample_building_group_id: address.building_group_id.0.clone(),
            sample_region: region_iso(address.region).to_string(),
            sample_street_primary_lang: street_primary_lang(&address.street_name)
                .map(|l| l.to_string())
                .unwrap_or_else(|| "?".to_string()),
            variant_type: VariantType::CrossLanguage,
            boosted: true,
            query_string: variant,
            expected_best_id: address.id.0.to_string(),
            expected_building_group_id: address.building_group_id.0.clone(),
        });
        next_id += 1;
    }

    // 6b. Boosted high house numbers: emit only that variant.
    let mut used = chosen_bgids_random.clone();
    used.extend(
        boosted_addresses
            .iter()
            .map(|(_, a)| a.building_group_id.0.clone()),
    );
    let high = collect_boosted(
        &searcher,
        &fields,
        &pools,
        &used,
        targets.boosted_high_house_number,
        &mut rng,
        |a| build_high_house_number(a).is_some(),
    )?;
    info!(
        boosted_collected = high.len(),
        target = targets.boosted_high_house_number,
        "generator: boosted high_house_number pool collected"
    );
    for (_addr, address) in &high {
        queries.push(SyntheticQuery {
            id: format!("mvp-v1-{:06}", next_id),
            sample_best_id: address.id.0.to_string(),
            sample_building_group_id: address.building_group_id.0.clone(),
            sample_region: region_iso(address.region).to_string(),
            sample_street_primary_lang: street_primary_lang(&address.street_name)
                .map(|l| l.to_string())
                .unwrap_or_else(|| "?".to_string()),
            variant_type: VariantType::HighHouseNumber,
            boosted: true,
            query_string: build_high_house_number(address).expect("filtered above"),
            expected_best_id: address.id.0.to_string(),
            expected_building_group_id: address.building_group_id.0.clone(),
        });
        next_id += 1;
    }

    // 7. Write JSON
    let set = SyntheticEvalSet {
        version: "v2".to_string(),
        seed,
        corpus: SyntheticCorpus {
            schema_version: 3,
            doc_count: searcher.num_docs(),
            dump_date: dump_date.to_string(),
        },
        queries,
    };
    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    set.save(output_path)?;
    info!(
        ?output_path,
        total_queries = set.queries.len(),
        "generator: written"
    );

    // Per-variant breakdown
    let mut by_variant: HashMap<&'static str, usize> = HashMap::new();
    let mut cross_language_fire_by_region: HashMap<&str, usize> = HashMap::new();
    let mut samples_by_region: HashMap<&str, usize> = HashMap::new();
    for q in &set.queries {
        *by_variant.entry(q.variant_type.as_str()).or_default() += 1;
        // Track cross_language firing rate per region (only counts random samples)
        if !q.boosted && q.variant_type == VariantType::CrossLanguage {
            *cross_language_fire_by_region
                .entry(region_iso_from_str(&q.sample_region))
                .or_default() += 1;
        }
        if !q.boosted && q.variant_type == VariantType::Canonical {
            *samples_by_region
                .entry(region_iso_from_str(&q.sample_region))
                .or_default() += 1;
        }
    }
    println!();
    println!("====== generator summary ======");
    println!("Total queries: {}", set.queries.len());
    println!("Per-variant breakdown:");
    for v in [
        "canonical",
        "minimal",
        "with_postal",
        "cross_language",
        "casing",
        "abbreviated",
        "typo",
        "box_number",
        "high_house_number",
        "messy_punctuation",
    ] {
        println!("  {v:<16}: {}", by_variant.get(v).copied().unwrap_or(0));
    }
    println!("Cross_language firing rate (random samples, excluding boosted 200):");
    for r in ["brussels", "flanders", "wallonia"] {
        let fires = cross_language_fire_by_region.get(r).copied().unwrap_or(0);
        let samples = samples_by_region.get(r).copied().unwrap_or(0);
        let pct = if samples > 0 {
            100.0 * fires as f64 / samples as f64
        } else {
            0.0
        };
        println!("  {r:<10}: {fires:>3} / {samples:>3} ({pct:.1}%)");
    }

    Ok(())
}

fn region_iso_from_str(s: &str) -> &str {
    match s {
        "BE-BRU" => "brussels",
        "BE-VLG" => "flanders",
        "BE-WAL" => "wallonia",
        _ => "?",
    }
}

fn region_iso(r: Region) -> &'static str {
    match r {
        Region::Brussels => "BE-BRU",
        Region::Flanders => "BE-VLG",
        Region::Wallonia => "BE-WAL",
    }
}

/// Walk every doc once, route into per-region pools keyed by bgid.
fn build_pools(
    searcher: &tantivy::Searcher,
    fields: &Fields,
) -> Result<HashMap<Region, HashMap<String, DocAddress>>> {
    let mut pools: HashMap<Region, HashMap<String, DocAddress>> = HashMap::new();

    for (seg_ord, seg_reader) in searcher.segment_readers().iter().enumerate() {
        let seg_ord = seg_ord as u32;
        let store_reader = seg_reader
            .get_store_reader(10)
            .context("opening store reader")?;
        let alive = seg_reader.alive_bitset();
        for (doc_id, doc_res) in (0u32..).zip(store_reader.iter::<TantivyDocument>(alive)) {
            let doc = doc_res?;
            let region_str = doc
                .get_first(fields.region)
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let bgid = doc
                .get_first(fields.building_group_id)
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if !bgid.is_empty() {
                let region_opt = match region_str {
                    "BE-BRU" => Some(Region::Brussels),
                    "BE-VLG" => Some(Region::Flanders),
                    "BE-WAL" => Some(Region::Wallonia),
                    _ => None,
                };
                if let Some(region) = region_opt {
                    pools
                        .entry(region)
                        .or_default()
                        .entry(bgid.to_string())
                        .or_insert(DocAddress::new(seg_ord, doc_id));
                }
            }
        }
    }

    Ok(pools)
}

fn sample_bgids(
    pools: &HashMap<Region, HashMap<String, DocAddress>>,
    region: Region,
    target: usize,
    rng: &mut ChaCha20Rng,
) -> Result<Vec<DocAddress>> {
    let pool = pools
        .get(&region)
        .ok_or_else(|| anyhow!("no docs found for region {:?}", region))?;
    // Sort bgid keys for deterministic ordering before sampling (HashMap iter
    // order is not deterministic across runs).
    let mut sorted_bgids: Vec<&String> = pool.keys().collect();
    sorted_bgids.sort();
    if sorted_bgids.len() < target {
        warn!(
            ?region,
            available = sorted_bgids.len(),
            target,
            "region pool smaller than target; using all available"
        );
    }
    let actual_target = target.min(sorted_bgids.len());
    let chosen = sorted_bgids
        .sample(rng, actual_target)
        .map(|bgid_ref| pool[*bgid_ref])
        .collect();
    Ok(chosen)
}

fn read_address(
    searcher: &tantivy::Searcher,
    fields: &Fields,
    addr: DocAddress,
) -> Result<Address> {
    let doc: TantivyDocument = searcher.doc(addr).context("reading doc from store")?;
    let payload = doc
        .get_first(fields.payload)
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing payload at {:?}", addr))?;
    serde_json::from_str::<Address>(payload)
        .with_context(|| format!("parsing Address payload at {:?}", addr))
}

/// Walk the pools (minus `chosen_bgids_random`) in a seeded shuffled order
/// and keep the first `target` addresses accepted by `keep`.
fn collect_boosted(
    searcher: &tantivy::Searcher,
    fields: &Fields,
    pools: &HashMap<Region, HashMap<String, DocAddress>>,
    chosen_bgids_random: &std::collections::HashSet<String>,
    target: usize,
    rng: &mut ChaCha20Rng,
    keep: impl Fn(&Address) -> bool,
) -> Result<Vec<(DocAddress, Address)>> {
    let mut candidates: Vec<(String, DocAddress)> = Vec::new();
    for pool in pools.values() {
        for (bgid, addr) in pool.iter() {
            if !chosen_bgids_random.contains(bgid) {
                candidates.push((bgid.clone(), *addr));
            }
        }
    }
    candidates.sort_by(|a, b| a.0.cmp(&b.0));
    // Deterministic shuffle via the seeded RNG
    use rand::seq::SliceRandom;
    candidates.shuffle(rng);

    let mut out: Vec<(DocAddress, Address)> = Vec::with_capacity(target);
    for (_bgid, addr) in candidates {
        if out.len() >= target {
            break;
        }
        let address = read_address(searcher, fields, addr)?;
        if keep(&address) {
            out.push((addr, address));
        }
    }
    Ok(out)
}

// --- Variant generation -----------------------------------------------------

/// Pick the primary language slot of a MultilingualName, preferring NL → FR → DE.
fn street_primary_lang(name: &MultilingualName) -> Option<&'static str> {
    if name.nl.is_some() {
        Some("nl")
    } else if name.fr.is_some() {
        Some("fr")
    } else if name.de.is_some() {
        Some("de")
    } else {
        None
    }
}

fn pick_in_lang<'a>(name: &'a MultilingualName, lang: &str) -> Option<&'a String> {
    match lang {
        "nl" => name.nl.as_ref(),
        "fr" => name.fr.as_ref(),
        "de" => name.de.as_ref(),
        _ => None,
    }
}

/// Generate the variant taxonomy: at minimum 5 variants (canonical,
/// minimal, with_postal, casing, messy_punctuation) plus cross_language,
/// abbreviated, typo, box_number and high_house_number when they apply.
/// `seed` makes the typo edit reproducible per (seed, address).
pub(crate) fn generate_variants(addr: &Address, seed: u64) -> Vec<(VariantType, String)> {
    let mut out: Vec<(VariantType, String)> = Vec::with_capacity(10);

    let lang = match street_primary_lang(&addr.street_name) {
        Some(l) => l,
        None => return out, // shouldn't happen — every Address has a street name
    };
    let street_primary = pick_in_lang(&addr.street_name, lang)
        .cloned()
        .unwrap_or_default();
    let muni_primary = pick_in_lang(&addr.municipality_name, lang)
        .cloned()
        .or_else(|| addr.municipality_name.fr.clone())
        .or_else(|| addr.municipality_name.nl.clone())
        .or_else(|| addr.municipality_name.de.clone())
        .unwrap_or_default();

    let hn = &addr.house_number;
    let postal = &addr.postal_code;

    let canonical = format!("{street_primary} {hn}, {postal} {muni_primary}");
    let minimal = format!("{street_primary} {hn}");
    let with_postal = format!("{street_primary} {hn} {postal}");
    let casing = canonical.to_lowercase();

    out.push((VariantType::Canonical, canonical.clone()));
    out.push((VariantType::Minimal, minimal));
    out.push((VariantType::WithPostal, with_postal));
    out.push((VariantType::Casing, casing));
    if let Some(cl) = build_cross_language(addr) {
        out.push((VariantType::CrossLanguage, cl));
    }
    if let Some(abbr) = build_abbreviated(&canonical, &street_primary, lang) {
        out.push((VariantType::Abbreviated, abbr));
    }
    if let Some(typo) = typo_street(&street_primary, typo_seed(seed, &addr.id.to_string())) {
        out.push((
            VariantType::Typo,
            canonical.replacen(&street_primary, &typo, 1),
        ));
    }
    if let Some(b) = addr.box_number.as_deref().filter(|b| !b.is_empty()) {
        let kw = if lang == "fr" { "bte" } else { "bus" };
        out.push((
            VariantType::BoxNumber,
            format!("{street_primary} {hn} {kw} {b}, {postal} {muni_primary}"),
        ));
    }
    if let Some(h) = build_high_house_number(addr) {
        out.push((VariantType::HighHouseNumber, h));
    }
    out.push((
        VariantType::MessyPunctuation,
        format!("{street_primary}, {hn}, B-{postal}, {muni_primary}"),
    ));

    out
}

/// `{street} {hn} {postal} {muni}` when the house number's numeric prefix
/// is ≥ 1000, so it sits next to the (also 4-digit) postal code.
pub(crate) fn build_high_house_number(addr: &Address) -> Option<String> {
    let digits: String = addr
        .house_number
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    if digits.parse::<u32>().ok()? < 1000 {
        return None;
    }
    let lang = street_primary_lang(&addr.street_name)?;
    let street = pick_in_lang(&addr.street_name, lang)?;
    let muni = pick_in_lang(&addr.municipality_name, lang)
        .or(addr.municipality_name.fr.as_ref())
        .or(addr.municipality_name.nl.as_ref())
        .or(addr.municipality_name.de.as_ref())?;
    Some(format!(
        "{street} {} {} {muni}",
        addr.house_number, addr.postal_code
    ))
}

/// FNV-1a over `(seed, id)` — a stable per-address seed for the typo edit.
fn typo_seed(seed: u64, id: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325 ^ seed;
    for b in id.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// Apply one edit (substitution, deletion, transposition or insertion) to
/// the longest word of `street` with ≥ 5 letters, never touching its first
/// letter. `None` when no word is long enough.
pub(crate) fn typo_street(street: &str, seed: u64) -> Option<String> {
    use rand::RngExt;
    let word = street
        .split(|c: char| !c.is_alphabetic())
        .filter(|w| w.chars().count() >= 5)
        .max_by_key(|w| w.chars().count())?;
    let chars: Vec<char> = word.chars().collect();
    let mut rng = ChaCha20Rng::seed_from_u64(seed);
    let letters: Vec<char> = "abcdefghijklmnopqrstuvwxyz".chars().collect();
    let pos = rng.random_range(1..chars.len());
    let mut edited = chars.clone();
    match rng.random_range(0..4u8) {
        0 => {
            let cur = chars[pos].to_ascii_lowercase();
            let pick = letters
                .iter()
                .copied()
                .filter(|c| *c != cur)
                .collect::<Vec<_>>();
            edited[pos] = pick[rng.random_range(0..pick.len())];
        }
        1 => {
            edited.remove(pos);
        }
        2 if pos + 1 < chars.len() && chars[pos] != chars[pos + 1] => edited.swap(pos, pos + 1),
        _ => edited.insert(pos, letters[rng.random_range(0..letters.len())]),
    }
    let edited: String = edited.into_iter().collect();
    Some(street.replacen(word, &edited, 1))
}

/// Rewrite the canonical query string so its street-type word is replaced
/// with a conventional abbreviation. Returns None when no abbreviation
/// applies (e.g. the street has no recognisable type token).
///
/// The mappings here mirror `data/street-type-abbreviations-v1.yaml` in the
/// *reverse* direction (canonical → user abbreviation). They're hand-coded
/// rather than loaded from the YAML so the eval set generates without
/// requiring the service or the index. Coverage must be re-synced if the
/// YAML changes.
pub(crate) fn build_abbreviated(
    canonical_query: &str,
    street_primary: &str,
    lang: &str,
) -> Option<String> {
    // FR: prefix street type at the start of the street name.
    let fr_prefix_map: &[(&str, &str)] = &[
        ("Avenue ", "Av. "),
        ("Boulevard ", "Bd "),
        ("Chaussée ", "Ch. "),
        ("Place ", "Pl. "),
        ("Square ", "Sq. "),
        ("Impasse ", "Imp. "),
        ("Chemin ", "Chem. "),
    ];
    // NL: suffix attached to compound — e.g. "Stationsstraat" → "Stationsstr.".
    let nl_suffix_map: &[(&str, &str)] = &[
        ("steenweg", "stwg"),
        ("straat", "str."),
        ("plein", "pl."),
        ("laan", "ln."),
    ];
    // DE: suffix attached to compound — e.g. "Rathausplatz" → "Rathauspl.".
    let de_suffix_map: &[(&str, &str)] =
        &[("straße", "str."), ("strasse", "str."), ("platz", "pl.")];

    match lang {
        "fr" => {
            for (full, abbr) in fr_prefix_map {
                if let Some(rest) = street_primary.strip_prefix(*full) {
                    let new_street = format!("{abbr}{rest}");
                    return Some(canonical_query.replacen(street_primary, &new_street, 1));
                }
            }
        }
        "nl" => {
            // Case-insensitive suffix match on the lower-cased street.
            let lower = street_primary.to_lowercase();
            for (full, abbr) in nl_suffix_map {
                if lower.ends_with(full) && lower.len() > full.len() {
                    let head_len = street_primary.len() - full.len();
                    let new_street = format!("{}{abbr}", &street_primary[..head_len]);
                    return Some(canonical_query.replacen(street_primary, &new_street, 1));
                }
            }
        }
        "de" => {
            let lower = street_primary.to_lowercase();
            for (full, abbr) in de_suffix_map {
                if lower.ends_with(full) && lower.len() > full.len() {
                    let head_len = street_primary.len() - full.len();
                    let new_street = format!("{}{abbr}", &street_primary[..head_len]);
                    return Some(canonical_query.replacen(street_primary, &new_street, 1));
                }
            }
        }
        _ => {}
    }
    None
}

/// Cross_language firing policy (file 14 §2 — mandatory unit-test surface).
///
/// Fires iff:
///   - Street has EXACTLY ONE non-empty language slot, AND
///   - Municipality has a non-empty name in a DIFFERENT language whose value
///     differs textually from the street's own-language muni name.
pub(crate) fn should_fire_cross_language(addr: &Address) -> bool {
    build_cross_language(addr).is_some()
}

/// Build the cross_language variant string, or None if the firing rules
/// suppress it. Composition: `{muni_otherlang} {street_in_native_lang} {hn}`.
pub(crate) fn build_cross_language(addr: &Address) -> Option<String> {
    let street = &addr.street_name;
    let nonempty_langs: Vec<(&'static str, &String)> = [
        ("nl", street.nl.as_ref()),
        ("fr", street.fr.as_ref()),
        ("de", street.de.as_ref()),
    ]
    .into_iter()
    .filter_map(|(l, v)| v.map(|s| (l, s)))
    .collect();

    if nonempty_langs.len() != 1 {
        return None; // street is multilingual → suppress (file 14 rule)
    }
    let (street_lang, street_value) = nonempty_langs[0];

    // Find a different-language muni alias whose text differs from the
    // street-language muni name.
    let muni_in_street_lang = pick_in_lang(&addr.municipality_name, street_lang)
        .cloned()
        .unwrap_or_default();
    for cand_lang in ["fr", "nl", "de"] {
        if cand_lang == street_lang {
            continue;
        }
        if let Some(cand) = pick_in_lang(&addr.municipality_name, cand_lang)
            && !cand.is_empty()
            && cand != &muni_in_street_lang
        {
            return Some(format!("{} {} {}", cand, street_value, addr.house_number));
        }
    }
    None
}

// --- Tests ------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crabfinder_core::ids::{
        AddressId, BestId, BuildingGroupId, MunicipalityId, PostalInfoId, StreetId,
    };
    use crabfinder_core::{
        Address, Coordinates, MultilingualName, Region, Status, StreetType, WgsCoord,
    };

    fn ml(fr: Option<&str>, nl: Option<&str>, de: Option<&str>) -> MultilingualName {
        MultilingualName {
            fr: fr.map(String::from),
            nl: nl.map(String::from),
            de: de.map(String::from),
        }
    }

    fn bid(ns: &str, oid: &str) -> BestId {
        BestId::new(ns, oid, "1")
    }

    fn make_addr(region: Region, street: MultilingualName, muni: MultilingualName) -> Address {
        Address {
            id: AddressId(bid("test", "1")),
            street_id: StreetId(bid("test", "1")),
            municipality_id: MunicipalityId(bid("test", "11002")),
            postal_info_id: PostalInfoId(bid("test", "2000")),
            part_of_municipality_id: None,
            region,
            status: Status::Current,
            house_number: "1".into(),
            box_number: None,
            officially_assigned: true,
            street_name: street,
            street_type: StreetType::Streetname,
            municipality_name: muni,
            postal_code: "2000".into(),
            postal_name: ml(None, Some("ANTWERPEN"), None),
            part_of_municipality_name: None,
            coordinates: Coordinates::Wgs84(WgsCoord {
                lat: 51.0,
                lon: 4.0,
            }),
            building_group_id: BuildingGroupId("bgid-1".into()),
            place_name: None,
        }
    }

    #[test]
    fn brussels_trilingual_street_suppresses_crosslang() {
        // Avenue Louise has FR + NL forms; muni Bruxelles has all 3 langs.
        // Street is NOT monolingual → suppress.
        let addr = make_addr(
            Region::Brussels,
            ml(Some("Avenue Louise"), Some("Louizalaan"), None),
            ml(Some("Bruxelles"), Some("Brussel"), Some("Brüssel")),
        );
        assert!(!should_fire_cross_language(&addr));
    }

    #[test]
    fn flemish_monolingual_with_fr_alias_muni_fires() {
        // Grote Markt is NL-only; Antwerpen has FR alias Anvers (different
        // text from NL Antwerpen) → fire.
        let addr = make_addr(
            Region::Flanders,
            ml(None, Some("Grote Markt"), None),
            ml(Some("Anvers"), Some("Antwerpen"), Some("Antwerpen")),
        );
        assert!(should_fire_cross_language(&addr));
        let cl = build_cross_language(&addr).unwrap();
        assert!(cl.starts_with("Anvers "), "got: {cl}");
        assert!(cl.contains("Grote Markt"));
    }

    #[test]
    fn flemish_facilites_street_bilingual_suppresses() {
        // Drogenbos facilités: street name has both NL and FR → not
        // monolingual → suppress.
        let addr = make_addr(
            Region::Flanders,
            ml(Some("Rue des Pâquerettes"), Some("Madeliefjesstraat"), None),
            ml(Some("Drogenbos"), Some("Drogenbos"), None),
        );
        assert!(!should_fire_cross_language(&addr));
    }

    #[test]
    fn wallonia_fr_only_with_nl_alias_muni_fires() {
        // Place Saint-Lambert is FR-only; Liège has NL alias "Luik" → fire.
        let addr = make_addr(
            Region::Wallonia,
            ml(Some("Place Saint-Lambert"), None, None),
            ml(Some("Liège"), Some("Luik"), Some("Lüttich")),
        );
        assert!(should_fire_cross_language(&addr));
        let cl = build_cross_language(&addr).unwrap();
        // First non-fr candidate is "nl" — Luik should be chosen.
        assert!(cl.starts_with("Luik "), "got: {cl}");
        assert!(cl.contains("Place Saint-Lambert"));
    }

    #[test]
    fn de_only_street_with_fr_alias_muni_fires() {
        // Hypothetical: a DE-only street in Sankt Vith; muni FR alias differs.
        let addr = make_addr(
            Region::Wallonia,
            ml(None, None, Some("Rathausplatz")),
            ml(Some("Saint-Vith"), None, Some("Sankt Vith")),
        );
        assert!(should_fire_cross_language(&addr));
        let cl = build_cross_language(&addr).unwrap();
        // street_lang is "de", first candidate scanned is "fr" (Saint-Vith)
        assert!(cl.starts_with("Saint-Vith "), "got: {cl}");
        assert!(cl.contains("Rathausplatz"));
    }

    #[test]
    fn eupen_de_only_with_identical_fr_muni_suppresses() {
        // Eupen muni name is "Eupen" in all langs — no different-text alias,
        // so even with a DE-only street the policy must suppress.
        let addr = make_addr(
            Region::Wallonia,
            ml(None, None, Some("Langgasse")),
            ml(Some("Eupen"), None, Some("Eupen")),
        );
        assert!(!should_fire_cross_language(&addr));
    }

    fn kinds_of(v: &[(VariantType, String)]) -> Vec<VariantType> {
        v.iter().map(|(vt, _)| *vt).collect()
    }

    #[test]
    fn generate_variants_emits_base_set_plus_optional_variants() {
        // Bilingual NL+FR "Louizalaan"/"Avenue Louise": primary-lang picker
        // chooses NL → street_primary = "Louizalaan" (in the NL suffix map).
        // No cross_language (street has 2 langs); abbreviated and typo fire.
        // Total: 5 base (incl. messy_punctuation) + abbreviated + typo = 7.
        let addr_no_cl = make_addr(
            Region::Brussels,
            ml(Some("Avenue Louise"), Some("Louizalaan"), None),
            ml(Some("Bruxelles"), Some("Brussel"), None),
        );
        let vars_no_cl = generate_variants(&addr_no_cl, 1);
        let kinds = kinds_of(&vars_no_cl);
        assert_eq!(vars_no_cl.len(), 7, "got kinds: {:?}", kinds);
        assert!(kinds.contains(&VariantType::Abbreviated));
        assert!(kinds.contains(&VariantType::Typo));
        assert!(kinds.contains(&VariantType::MessyPunctuation));
        assert!(!kinds.contains(&VariantType::BoxNumber));
        assert!(!kinds.contains(&VariantType::HighHouseNumber));

        // NL-monolingual "Grote Markt" with FR muni alias "Anvers".
        // Cross_language fires; "Markt" is not in the NL suffix map so
        // abbreviated does NOT fire. Total: 5 base + cross_language + typo.
        let addr_with_cl = make_addr(
            Region::Flanders,
            ml(None, Some("Grote Markt"), None),
            ml(Some("Anvers"), Some("Antwerpen"), None),
        );
        let vars = generate_variants(&addr_with_cl, 1);
        let kinds = kinds_of(&vars);
        assert_eq!(vars.len(), 7, "got kinds: {:?}", kinds);
        assert!(kinds.contains(&VariantType::Canonical));
        assert!(kinds.contains(&VariantType::CrossLanguage));
        assert!(!kinds.contains(&VariantType::Abbreviated));
    }

    fn variant(v: &[(VariantType, String)], kind: VariantType) -> Option<&str> {
        v.iter().find(|(k, _)| *k == kind).map(|(_, q)| q.as_str())
    }

    #[test]
    fn box_high_number_and_messy_variants() {
        let mut addr = make_addr(
            Region::Brussels,
            ml(Some("Chaussée de Waterloo"), None, None),
            ml(Some("Uccle"), Some("Ukkel"), None),
        );
        addr.house_number = "1357".into();
        addr.postal_code = "1180".into();
        addr.box_number = Some("3".into());
        let vars = generate_variants(&addr, 1);
        assert_eq!(
            variant(&vars, VariantType::BoxNumber),
            Some("Chaussée de Waterloo 1357 bte 3, 1180 Uccle")
        );
        assert_eq!(
            variant(&vars, VariantType::HighHouseNumber),
            Some("Chaussée de Waterloo 1357 1180 Uccle")
        );
        assert_eq!(
            variant(&vars, VariantType::MessyPunctuation),
            Some("Chaussée de Waterloo, 1357, B-1180, Uccle")
        );

        // Dutch street → `bus` keyword; empty box → no variant.
        let mut nl = make_addr(
            Region::Flanders,
            ml(None, Some("Meir"), None),
            ml(None, Some("Antwerpen"), None),
        );
        nl.box_number = Some("2".into());
        assert!(
            variant(&generate_variants(&nl, 1), VariantType::BoxNumber)
                .unwrap()
                .contains(" bus 2,")
        );
        nl.box_number = Some(String::new());
        assert!(variant(&generate_variants(&nl, 1), VariantType::BoxNumber).is_none());
    }

    #[test]
    fn high_house_number_needs_four_digits() {
        let mut addr = make_addr(
            Region::Flanders,
            ml(None, Some("Meir"), None),
            ml(None, Some("Antwerpen"), None),
        );
        addr.house_number = "999".into();
        assert!(build_high_house_number(&addr).is_none());
        addr.house_number = "1000A".into();
        assert_eq!(
            build_high_house_number(&addr).as_deref(),
            Some("Meir 1000A 2000 Antwerpen")
        );
        addr.house_number = "A1".into();
        assert!(build_high_house_number(&addr).is_none());
    }

    /// Optimal-string-alignment distance (Damerau with adjacent swaps).
    fn osa(a: &str, b: &str) -> usize {
        let a: Vec<char> = a.chars().collect();
        let b: Vec<char> = b.chars().collect();
        let mut d = vec![vec![0usize; b.len() + 1]; a.len() + 1];
        for (i, row) in d.iter_mut().enumerate() {
            row[0] = i;
        }
        for (j, cell) in d[0].iter_mut().enumerate() {
            *cell = j;
        }
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

    #[test]
    fn typo_is_one_edit_on_longest_word_and_deterministic() {
        let street = "Rue de la Station";
        for seed in 0..200u64 {
            let t = typo_street(street, seed).unwrap();
            assert_eq!(t, typo_street(street, seed).unwrap(), "deterministic");
            assert!(t.starts_with("Rue de la S"), "first letter kept: {t}");
            assert_eq!(osa(street, &t), 1, "{street} → {t}");
        }
        // Different seeds actually produce different edits.
        let distinct: std::collections::HashSet<String> =
            (0..50).map(|s| typo_street(street, s).unwrap()).collect();
        assert!(distinct.len() > 10);
    }

    #[test]
    fn typo_skips_short_streets() {
        assert!(typo_street("Rue du Lac", 1).is_none());
        assert!(typo_street("", 1).is_none());
    }

    #[test]
    fn typo_seed_depends_on_seed_and_id() {
        assert_eq!(typo_seed(1, "a"), typo_seed(1, "a"));
        assert_ne!(typo_seed(1, "a"), typo_seed(2, "a"));
        assert_ne!(typo_seed(1, "a"), typo_seed(1, "b"));
    }

    #[test]
    fn new_variant_types_have_stable_names() {
        assert_eq!(VariantType::Typo.as_str(), "typo");
        assert_eq!(VariantType::BoxNumber.as_str(), "box_number");
        assert_eq!(VariantType::HighHouseNumber.as_str(), "high_house_number");
        assert_eq!(VariantType::MessyPunctuation.as_str(), "messy_punctuation");
    }

    #[test]
    fn abbreviated_fr_prefix() {
        let out = build_abbreviated("Avenue Louise 32, 1050 Ixelles", "Avenue Louise", "fr");
        assert_eq!(out.as_deref(), Some("Av. Louise 32, 1050 Ixelles"));
    }

    #[test]
    fn abbreviated_nl_suffix() {
        let out = build_abbreviated("Stationsstraat 1, 2000 Antwerpen", "Stationsstraat", "nl");
        assert_eq!(out.as_deref(), Some("Stationsstr. 1, 2000 Antwerpen"));
    }

    #[test]
    fn abbreviated_de_suffix() {
        let out = build_abbreviated("Rathausplatz 15, 4700 Eupen", "Rathausplatz", "de");
        assert_eq!(out.as_deref(), Some("Rathauspl. 15, 4700 Eupen"));
    }

    #[test]
    fn abbreviated_returns_none_for_unrecognized_street_type() {
        assert!(build_abbreviated("Markt 1, 2000 Antwerpen", "Markt", "nl").is_none());
        assert!(build_abbreviated("Maredsous 1, 5530 Anhée", "Maredsous", "fr").is_none());
    }
}
