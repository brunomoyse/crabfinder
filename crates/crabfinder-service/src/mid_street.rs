//! Runtime loader for the `mid-street-v1.bin` side-table produced by
//! `crabfinder-indexer` at schema v7.
//!
//! See `crabfinder_indexer::mid_street` for the on-disk format. This module
//! reads the file into an in-memory `HashMap` once at service startup; lookup
//! is then constant-time per resolve call.
//!
//! Compatibility: when the side-file is missing (e.g. the index was built by
//! a stale indexer binary, or by some future format we don't recognise yet),
//! `load()` returns `Self::empty()` and logs a warning. The resolver then
//! degrades gracefully — `MedianStreet` never fires and we fall through to
//! `FuzzyStreet` / `NotFound`. Refusing to start over a missing side-file
//! would conflate "no batch support" with "broken index", which is hostile
//! to users running the service for `/geocode` only.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

use anyhow::{Context, Result};
use tracing::{info, warn};

/// Magic for the v1 side-file. Must match `crabfinder_indexer::mid_street::MID_STREET_MAGIC`.
const MAGIC_V1: &[u8; 8] = b"CRBMID7\0";

/// In-memory mid_street lookup. Keyed by `(street_key, postal_code)` where
/// `street_key = "<namespace>|<object_id>"` — same string the resolver
/// extracts from a Tantivy `building_group_id` by stripping the trailing
/// `|<house>` segment.
#[derive(Debug)]
pub struct MidStreetIndex {
    map: HashMap<(String, String), (f64, f64)>,
}

impl MidStreetIndex {
    pub fn empty() -> Self {
        Self {
            map: HashMap::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// Open the side-file at `path`. Returns `Self::empty()` (with a warning)
    /// if the file is absent. Returns an error only on IO / format failures
    /// of an *existing* file — we never want to silently feed garbage into
    /// `MedianStreet`.
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            warn!(
                path = %path.display(),
                "mid_street side-file missing — Confidence::MedianStreet disabled. \
                 Rebuild the index with the v7+ indexer to enable it."
            );
            return Ok(Self::empty());
        }
        let f = File::open(path)
            .with_context(|| format!("opening mid_street side-file {path:?}"))?;
        let mut r = BufReader::new(f);

        let mut magic = [0u8; 8];
        r.read_exact(&mut magic).context("read magic")?;
        if &magic != MAGIC_V1 {
            anyhow::bail!(
                "mid_street side-file at {path:?} has wrong magic: got {:?}, expected {:?}. \
                 Re-run the indexer.",
                magic,
                MAGIC_V1,
            );
        }
        let mut n_buf = [0u8; 8];
        r.read_exact(&mut n_buf).context("read n_entries")?;
        let n_entries = u64::from_le_bytes(n_buf);

        let mut map: HashMap<(String, String), (f64, f64)> =
            HashMap::with_capacity(n_entries as usize);

        for i in 0..n_entries {
            let mut sk_len_buf = [0u8; 4];
            r.read_exact(&mut sk_len_buf)
                .with_context(|| format!("read street_key_len at entry {i}"))?;
            let sk_len = u32::from_le_bytes(sk_len_buf) as usize;
            let mut sk_buf = vec![0u8; sk_len];
            r.read_exact(&mut sk_buf)
                .with_context(|| format!("read street_key at entry {i}"))?;
            let street_key =
                String::from_utf8(sk_buf).with_context(|| format!("street_key at entry {i}"))?;

            let mut pc_len_buf = [0u8; 1];
            r.read_exact(&mut pc_len_buf)
                .with_context(|| format!("read postal_len at entry {i}"))?;
            let pc_len = pc_len_buf[0] as usize;
            let mut pc_buf = vec![0u8; pc_len];
            r.read_exact(&mut pc_buf)
                .with_context(|| format!("read postal_code at entry {i}"))?;
            let postal_code =
                String::from_utf8(pc_buf).with_context(|| format!("postal_code at entry {i}"))?;

            let mut lat_buf = [0u8; 8];
            r.read_exact(&mut lat_buf)
                .with_context(|| format!("read lat at entry {i}"))?;
            let lat = f64::from_le_bytes(lat_buf);
            let mut lon_buf = [0u8; 8];
            r.read_exact(&mut lon_buf)
                .with_context(|| format!("read lon at entry {i}"))?;
            let lon = f64::from_le_bytes(lon_buf);

            map.insert((street_key, postal_code), (lat, lon));
        }

        info!(
            path = %path.display(),
            entries = map.len(),
            "mid_street side-file loaded"
        );

        Ok(Self { map })
    }

    /// Look up the median coordinates for a `(street_key, postal_code)` pair.
    /// `street_key` is the `"namespace|object_id"` slice (i.e. a
    /// `building_group_id` with the trailing `|<house>` stripped).
    pub fn lookup(&self, street_key: &str, postal_code: &str) -> Option<(f64, f64)> {
        // Avoid an allocation: HashMap lookup on owned key requires constructing
        // the tuple. With ~400k entries the cost is dominated by the HashMap
        // probe, not the two String clones, so we just do the natural thing.
        // (If this ever shows up in a profile, swap to a Borrow-style key wrap.)
        self.map
            .get(&(street_key.to_string(), postal_code.to_string()))
            .copied()
    }
}

/// Strip the trailing `|<house>` from a Tantivy `building_group_id` to get
/// the street_key used in the mid_street side-table.
///
/// `building_group_id` format: `"{namespace}|{object_id}|{house_number}"`
/// (see `crabfinder_core::BuildingGroupId::from_parts`).
pub fn street_key_from_bgid(bgid: &str) -> Option<&str> {
    bgid.rsplit_once('|').map(|(prefix, _house)| prefix)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn build_test_file(entries: &[(&str, &str, f64, f64)]) -> tempfile::NamedTempFile {
        let mut f = tempfile::Builder::new()
            .prefix("mid-svc-test")
            .suffix(".bin")
            .tempfile()
            .unwrap();
        f.write_all(MAGIC_V1).unwrap();
        f.write_all(&(entries.len() as u64).to_le_bytes()).unwrap();
        for (sk, pc, lat, lon) in entries {
            f.write_all(&(sk.len() as u32).to_le_bytes()).unwrap();
            f.write_all(sk.as_bytes()).unwrap();
            f.write_all(&(pc.len() as u8).to_le_bytes()).unwrap();
            f.write_all(pc.as_bytes()).unwrap();
            f.write_all(&lat.to_le_bytes()).unwrap();
            f.write_all(&lon.to_le_bytes()).unwrap();
        }
        f.flush().unwrap();
        f
    }

    #[test]
    fn load_and_lookup_round_trips() {
        let f = build_test_file(&[
            ("https://x|S1", "1000", 50.1234, 4.5678),
            ("https://x|S2", "1050", 50.9876, 4.4321),
        ]);
        let m = MidStreetIndex::load(f.path()).unwrap();
        assert_eq!(m.len(), 2);
        let (lat, lon) = m.lookup("https://x|S1", "1000").unwrap();
        assert!((lat - 50.1234).abs() < 1e-9);
        assert!((lon - 4.5678).abs() < 1e-9);
        assert!(m.lookup("https://x|S1", "9999").is_none());
        assert!(m.lookup("https://x|UNKNOWN", "1000").is_none());
    }

    #[test]
    fn missing_file_returns_empty_index() {
        let m =
            MidStreetIndex::load(std::path::Path::new("/tmp/definitely-not-a-real-mid-file.bin"))
                .unwrap();
        assert_eq!(m.len(), 0);
    }

    #[test]
    fn wrong_magic_is_reported() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(b"XXXXXXXX").unwrap();
        f.write_all(&0u64.to_le_bytes()).unwrap();
        f.flush().unwrap();
        let err = MidStreetIndex::load(f.path()).unwrap_err();
        assert!(err.to_string().contains("wrong magic"));
    }

    #[test]
    fn truncated_file_after_header_is_reported_not_silent() {
        // A common partial-write failure mode: the writer wrote the magic
        // and the n_entries header but crashed before any entries were
        // committed. load() must fail loudly rather than silently
        // returning an empty index — silent corruption would conflate
        // "indexer crashed mid-write" with "indexer ran without batch
        // support", and the user would never discover the missing data.
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(MAGIC_V1).unwrap();
        // n_entries = 3 but we'll write zero entries below.
        f.write_all(&3u64.to_le_bytes()).unwrap();
        f.flush().unwrap();
        let err = MidStreetIndex::load(f.path()).unwrap_err();
        // The error wraps an IO error with the read_exact failing on the
        // first entry's street_key_len field. We assert on the structured
        // chain rather than the precise message string.
        assert!(
            err.to_string().contains("street_key_len")
                || err.to_string().contains("entry 0")
                || err.chain().any(|c| c.to_string().contains("failed to fill whole buffer")),
            "expected EOF-style error, got {err:?}"
        );
    }

    #[test]
    fn truncated_lat_lon_payload_is_reported() {
        // Header + entry header + truncated coordinates. Same property as
        // above but mid-entry: we want the loader to fail on a malformed
        // record rather than load garbage f64 bytes.
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(MAGIC_V1).unwrap();
        f.write_all(&1u64.to_le_bytes()).unwrap();
        let sk = "ns|S1";
        f.write_all(&(sk.len() as u32).to_le_bytes()).unwrap();
        f.write_all(sk.as_bytes()).unwrap();
        let pc = "1000";
        f.write_all(&(pc.len() as u8).to_le_bytes()).unwrap();
        f.write_all(pc.as_bytes()).unwrap();
        // Write lat but truncate before lon.
        f.write_all(&50.83f64.to_le_bytes()).unwrap();
        f.flush().unwrap();
        let err = MidStreetIndex::load(f.path()).unwrap_err();
        assert!(
            err.to_string().contains("lon")
                || err.chain().any(|c| c.to_string().contains("failed to fill whole buffer")),
            "expected truncated-lon error, got {err:?}"
        );
    }

    #[test]
    fn street_key_from_bgid_strips_last_pipe_segment() {
        assert_eq!(
            street_key_from_bgid("https://x.be|3919|32"),
            Some("https://x.be|3919")
        );
        assert_eq!(
            street_key_from_bgid("ns|oid|hn"),
            Some("ns|oid")
        );
        // No pipe → None (defensive; the indexer always emits pipes).
        assert_eq!(street_key_from_bgid("nope"), None);
    }
}
