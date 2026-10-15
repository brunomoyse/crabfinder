//! Precompute `(street, postal_code) → median-house-number coordinates`.
//!
//! The indexer collects every address's `(street_id, postal_code, parsed_num,
//! lat, lon)`, groups by (street, postal), and on `finish()` selects the
//! lower-discrete-median number per group (PhacochR §2.1, R's
//! `quantile(p=0.5, type=3)` — lower median for even-sized samples). The
//! resulting side-table is read by the service at startup to back the
//! `Confidence::MedianStreet` fallback when an input row's house number
//! is missing or doesn't match.
//!
//! Why precomputed, not on-the-fly:
//! - The median is a per (street, postal) constant, not per query — there's
//!   no value in recomputing it on every request.
//! - We don't want to widen the Tantivy schema with a per-doc replica of
//!   street-level data (110 MB of redundant fast fields on 6.8M docs).
//! - The side-table is rebuilt by the indexer so any algorithmic change is
//!   a re-index away — no Tantivy schema bump.
//!
//! On-disk format `mid-street-v1.bin`:
//! ```text
//! 0..8    : magic = b"CRBMID7\0"
//! 8..16   : n_entries: u64 (little-endian)
//! repeat n_entries times:
//!   street_key_len: u32 LE
//!   street_key:     bytes (UTF-8, "<namespace>|<object_id>")
//!   postal_len:     u8    (typically 4)
//!   postal_code:    bytes (UTF-8)
//!   mid_lat:        f64 LE
//!   mid_lon:        f64 LE
//! ```
//! Entries are sorted by `(street_key, postal_code)` for reproducible diffs.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::Path;

use anyhow::{Context, Result};

use crabfinder_core::house_number::leading_number;
use crabfinder_core::{Address, Coordinates};

/// Magic prefix for the v1 side-file. Bumping this string (`MID7` → `MID8`,
/// etc.) signals an on-disk format change.
pub const MID_STREET_MAGIC: &[u8; 8] = b"CRBMID7\0";
pub const MID_STREET_FILENAME: &str = "mid-street-v1.bin";

/// `(street_key, postal_code)` → every recorded `(house_number, lat, lon)`.
type StreetGroups = HashMap<(String, String), Vec<(u32, f64, f64)>>;

/// In-memory accumulator. One instance per indexer run; `record()` is called
/// once per indexed address, `finish()` flushes to disk after Tantivy commit.
pub struct MidStreetBuilder {
    /// `(street_key, postal_code) → Vec<(parsed_house_number, lat, lon)>`.
    /// At peak we hold every doc's tuple; for the full Belgium dump that's
    /// ~6.8M × ~28 bytes = ~190 MB in the worst case. In practice the
    /// `parsed_house_number` filter (digit prefix only) drops box-suffix
    /// duplicates and 80%+ of the multi-unit-building rows collapse to the
    /// same number.
    groups: StreetGroups,
}

impl MidStreetBuilder {
    pub fn new() -> Self {
        Self {
            groups: HashMap::new(),
        }
    }

    /// Record one address. Caller invokes this in the indexer's Pass-2
    /// closure, alongside `writer.add_address_with_notability()`.
    ///
    /// Silently skips addresses whose house_number doesn't have a numeric
    /// prefix (e.g. unparseable junk) or whose coordinates are not WGS84
    /// (shouldn't happen post-reprojection, but we don't trust the pipeline
    /// to that level — Lambert72 makes for poor f64 medians).
    pub fn record(&mut self, addr: &Address) {
        let Some(num) = leading_number(&addr.house_number) else {
            return;
        };
        let (lat, lon) = match addr.coordinates {
            Coordinates::Wgs84(w) => (w.lat, w.lon),
            Coordinates::Lambert72(_) => return,
        };
        let street_key = format!(
            "{}|{}",
            addr.street_id.0.namespace, addr.street_id.0.object_identifier
        );
        self.groups
            .entry((street_key, addr.postal_code.clone()))
            .or_default()
            .push((num, lat, lon));
    }

    /// Flush the builder to disk. Consumes `self` because the in-memory map
    /// has served its purpose and freeing it as soon as we're done shrinks
    /// the indexer's peak RSS.
    pub fn finish(mut self, out: &Path) -> Result<usize> {
        let f = File::create(out)
            .with_context(|| format!("creating mid_street side-table at {out:?}"))?;
        let mut w = BufWriter::new(f);

        // Header: magic + placeholder n_entries (rewritten after the body).
        w.write_all(MID_STREET_MAGIC).context("write magic")?;
        w.write_all(&0u64.to_le_bytes())
            .context("write n_entries placeholder")?;

        // Deterministic ordering: sort keys so the file is byte-stable across
        // runs (helps with eval reproducibility and `diff` debugging).
        let mut keys: Vec<(String, String)> = self.groups.keys().cloned().collect();
        keys.sort();

        let mut n_written = 0usize;
        for key in &keys {
            // `take()` (not get-by-ref) so we move the Vec out and drop it
            // as we go, capping peak memory.
            let mut items = self.groups.remove(key).expect("key from keys() must exist");
            items.sort_by_key(|(num, _, _)| *num);
            // Discrete lower median: index (n-1)/2 in 0-based terms, matching
            // R's `quantile(p=0.5, type=3)` (PhacochR §2.1).
            //   n=1 → 0   n=2 → 0   n=3 → 1   n=4 → 1   n=5 → 2 …
            let idx = (items.len() - 1) / 2;
            let (_num, lat, lon) = items[idx];

            let street_bytes = key.0.as_bytes();
            let postal_bytes = key.1.as_bytes();
            // Skip pathological entries rather than failing the whole index.
            if street_bytes.len() > u32::MAX as usize {
                continue;
            }
            if postal_bytes.len() > u8::MAX as usize {
                continue;
            }
            w.write_all(&(street_bytes.len() as u32).to_le_bytes())?;
            w.write_all(street_bytes)?;
            w.write_all(&(postal_bytes.len() as u8).to_le_bytes())?;
            w.write_all(postal_bytes)?;
            w.write_all(&lat.to_le_bytes())?;
            w.write_all(&lon.to_le_bytes())?;
            n_written += 1;
        }
        w.flush().context("flush mid_street")?;
        // Drop the BufWriter to release the file handle, then re-open for
        // the header rewrite. Cheaper than tracking the underlying File
        // through the BufWriter.
        drop(w);

        let mut g = OpenOptions::new()
            .write(true)
            .open(out)
            .with_context(|| format!("reopening {out:?} to fix header"))?;
        g.seek(SeekFrom::Start(MID_STREET_MAGIC.len() as u64))?;
        g.write_all(&(n_written as u64).to_le_bytes())?;
        g.flush()?;

        Ok(n_written)
    }

    /// Number of distinct `(street, postal)` keys collected so far. Useful
    /// for indexer-time logging.
    pub fn n_groups(&self) -> usize {
        self.groups.len()
    }
}

impl Default for MidStreetBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crabfinder_core::{
        AddressId, BestId, BuildingGroupId, MultilingualName, MunicipalityId, PostalInfoId, Region,
        Status, StreetId, StreetType, WgsCoord,
    };

    fn fix(
        addr_oid: &str,
        street_oid: &str,
        house: &str,
        postal: &str,
        lat: f64,
        lon: f64,
    ) -> Address {
        let ns_addr = "https://databrussels.be/id/address".to_string();
        let ns_street = "https://databrussels.be/id/streetname".to_string();
        let ns_muni = "https://databrussels.be/id/municipality".to_string();
        let ns_postal = "https://databrussels.be/id/postalinfo".to_string();
        let street_id = StreetId::new(BestId::new(ns_street, street_oid, "1"));
        let building_group_id = BuildingGroupId::from_parts(&street_id, house);
        Address {
            id: AddressId::new(BestId::new(ns_addr, addr_oid, "1")),
            street_id,
            municipality_id: MunicipalityId::new(BestId::new(ns_muni, "21001", "1")),
            postal_info_id: PostalInfoId::new(BestId::new(ns_postal, postal, "1")),
            part_of_municipality_id: None,
            region: Region::Brussels,
            status: Status::Current,
            house_number: house.into(),
            box_number: None,
            officially_assigned: true,
            street_name: MultilingualName {
                fr: Some("Avenue Test".into()),
                nl: None,
                de: None,
            },
            street_type: StreetType::Streetname,
            municipality_name: MultilingualName::default(),
            postal_code: postal.into(),
            postal_name: MultilingualName::default(),
            part_of_municipality_name: None,
            coordinates: Coordinates::Wgs84(WgsCoord { lat, lon }),
            building_group_id,
            place_name: None,
        }
    }

    #[test]
    fn median_picks_lower_when_even_count() {
        // Numbers 2,4,6,8 → lower median = 4 (idx (4-1)/2 = 1).
        let mut b = MidStreetBuilder::new();
        b.record(&fix("a", "S1", "2", "1000", 50.1, 4.1));
        b.record(&fix("b", "S1", "4", "1000", 50.2, 4.2));
        b.record(&fix("c", "S1", "6", "1000", 50.3, 4.3));
        b.record(&fix("d", "S1", "8", "1000", 50.4, 4.4));
        let tmp = tempfile::Builder::new()
            .prefix("mid-test")
            .suffix(".bin")
            .tempfile()
            .unwrap();
        let n = b.finish(tmp.path()).unwrap();
        assert_eq!(n, 1);
        // Read it back ad-hoc to verify the lat is 50.2 (the #4 row).
        let bytes = std::fs::read(tmp.path()).unwrap();
        assert_eq!(&bytes[..8], MID_STREET_MAGIC);
        let n_read = u64::from_le_bytes(bytes[8..16].try_into().unwrap());
        assert_eq!(n_read, 1);
        // Walk the single entry.
        let mut p = 16usize;
        let sk_len = u32::from_le_bytes(bytes[p..p + 4].try_into().unwrap()) as usize;
        p += 4;
        let sk = std::str::from_utf8(&bytes[p..p + sk_len]).unwrap();
        assert!(sk.ends_with("|S1"));
        p += sk_len;
        let pc_len = bytes[p] as usize;
        p += 1;
        let pc = std::str::from_utf8(&bytes[p..p + pc_len]).unwrap();
        assert_eq!(pc, "1000");
        p += pc_len;
        let lat = f64::from_le_bytes(bytes[p..p + 8].try_into().unwrap());
        p += 8;
        let lon = f64::from_le_bytes(bytes[p..p + 8].try_into().unwrap());
        assert!((lat - 50.2).abs() < 1e-9, "lat={lat}");
        assert!((lon - 4.2).abs() < 1e-9, "lon={lon}");
    }

    #[test]
    fn median_picks_middle_rank_when_odd_count() {
        // Numbers 2,4,6,8,10 → median = 6 (idx (5-1)/2 = 2).
        let mut b = MidStreetBuilder::new();
        for (num, lat) in [(2, 50.1), (4, 50.2), (6, 50.3), (8, 50.4), (10, 50.5)] {
            b.record(&fix(
                &format!("a{num}"),
                "S2",
                &num.to_string(),
                "1000",
                lat,
                4.0,
            ));
        }
        let tmp = tempfile::Builder::new()
            .prefix("mid-test-odd")
            .suffix(".bin")
            .tempfile()
            .unwrap();
        let n = b.finish(tmp.path()).unwrap();
        assert_eq!(n, 1);
        let bytes = std::fs::read(tmp.path()).unwrap();
        // skip header
        let mut p = 16usize;
        let sk_len = u32::from_le_bytes(bytes[p..p + 4].try_into().unwrap()) as usize;
        p += 4 + sk_len;
        let pc_len = bytes[p] as usize;
        p += 1 + pc_len;
        let lat = f64::from_le_bytes(bytes[p..p + 8].try_into().unwrap());
        assert!(
            (lat - 50.3).abs() < 1e-9,
            "expected 50.3 (#6's lat), got {lat}"
        );
    }

    #[test]
    fn distinct_postal_codes_are_distinct_groups() {
        let mut b = MidStreetBuilder::new();
        // Same street, 2 postal codes — two entries in the output.
        b.record(&fix("a", "S3", "10", "1000", 50.1, 4.1));
        b.record(&fix("b", "S3", "20", "1000", 50.2, 4.2));
        b.record(&fix("c", "S3", "30", "2000", 51.1, 5.1));
        let tmp = tempfile::Builder::new()
            .prefix("mid-test-distinct")
            .suffix(".bin")
            .tempfile()
            .unwrap();
        let n = b.finish(tmp.path()).unwrap();
        assert_eq!(n, 2);
    }

    #[test]
    fn non_numeric_house_numbers_are_skipped() {
        let mut b = MidStreetBuilder::new();
        b.record(&fix("a", "S4", "ZZZ", "1000", 50.1, 4.1));
        // No numeric data → n_groups still 0.
        assert_eq!(b.n_groups(), 0);
    }
}
