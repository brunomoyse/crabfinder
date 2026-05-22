//! Per-segment `DocId → street_address_count` lookup for /geocode ranking.
//!
//! The /geocode tweak_score multiplies BM25 by
//!
//! ```text
//! (1 + notability) × (1 + ADDR_WEIGHT × ln(1 + address_count))
//! ```
//!
//! `address_count` is the number of indexed addresses on the same *street*
//! as the candidate doc. The factor is small (controlled by `ADDR_WEIGHT`
//! at the call site) and is intended to break ties on ambiguous queries —
//! `Rue de la Gare` should surface the busiest streets first, not the
//! hamlet variants. We do *not* use it as a primary signal.
//!
//! Approach: build a `Vec<u32>` per segment at service startup, indexed
//! by `DocId`. Two-pass walk of the doc store:
//!   1. Tally `HashMap<street_key, count>` across every live doc. The
//!      `street_key` is `building_group_id` truncated at the second `|`
//!      separator (everything before the house number).
//!   2. For each doc, look up its count and write it into `vec[doc_id]`.
//!
//! Built once per service start; the `Arc<Vec<u32>>` handle is cheap to
//! clone into a `'static` tweak_score closure.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use tantivy::index::SegmentId;
use tantivy::schema::Value;
use tantivy::{IndexReader, SegmentReader, TantivyDocument};

use crabfinder_indexer::index::CrabSchema;

#[derive(Debug)]
pub struct AddressCountIndex {
    per_segment: HashMap<SegmentId, Arc<Vec<u32>>>,
    pub n_docs: u64,
    pub n_streets: usize,
    pub max_count: u32,
}

pub type SharedAddressCountIndex = Arc<AddressCountIndex>;

/// Truncate a `building_group_id` to the street portion (`{ns}|{oid}`).
/// The full form is `{ns}|{oid}|{house_number}` (see
/// `crabfinder_core::ids::BuildingGroupId`); we drop everything from the
/// second `|` onward so all addresses on the same street share a key.
fn street_key_from_bgid(bgid: &str) -> &str {
    let mut i = 0;
    let mut pipe_count = 0;
    for (idx, b) in bgid.bytes().enumerate() {
        if b == b'|' {
            pipe_count += 1;
            if pipe_count == 2 {
                i = idx;
                return &bgid[..i];
            }
        }
    }
    let _ = i;
    bgid
}

impl AddressCountIndex {
    pub fn build(reader: &IndexReader, schema: &CrabSchema) -> Result<Self> {
        let searcher = reader.searcher();

        let mut street_counts: HashMap<String, u32> = HashMap::new();
        let mut total_docs: u64 = 0;
        for seg in searcher.segment_readers() {
            let store = seg
                .get_store_reader(10)
                .context("opening store reader (Pass 1)")?;
            for doc_res in store.iter::<TantivyDocument>(seg.alive_bitset()) {
                let doc = doc_res.context("reading doc (Pass 1)")?;
                if let Some(bgid) = doc.get_first(schema.building_group_id).and_then(|v| v.as_str()) {
                    let key = street_key_from_bgid(bgid);
                    *street_counts.entry(key.to_string()).or_insert(0) += 1;
                }
                total_docs += 1;
            }
        }
        let max_count = street_counts.values().copied().max().unwrap_or(0);

        let mut per_segment: HashMap<SegmentId, Arc<Vec<u32>>> = HashMap::new();
        for seg in searcher.segment_readers() {
            let n = seg.max_doc() as usize;
            let mut counts = vec![1u32; n];
            let store = seg
                .get_store_reader(10)
                .context("opening store reader (Pass 2)")?;
            let mut doc_id: u32 = 0;
            for doc_res in store.iter::<TantivyDocument>(seg.alive_bitset()) {
                let doc = doc_res.context("reading doc (Pass 2)")?;
                if let Some(bgid) = doc.get_first(schema.building_group_id).and_then(|v| v.as_str()) {
                    let key = street_key_from_bgid(bgid);
                    if let Some(&c) = street_counts.get(key) {
                        counts[doc_id as usize] = c;
                    }
                }
                doc_id += 1;
            }
            per_segment.insert(seg.segment_id(), Arc::new(counts));
        }

        Ok(Self {
            per_segment,
            n_docs: total_docs,
            n_streets: street_counts.len(),
            max_count,
        })
    }

    /// Returns the per-doc count handle for `seg`, or `None` if the segment
    /// wasn't present at build time. The returned `Arc` can be cloned into
    /// a `'static` closure cheaply.
    pub fn for_segment(&self, seg: &SegmentReader) -> Option<Arc<Vec<u32>>> {
        self.per_segment.get(&seg.segment_id()).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn street_key_strips_house_number() {
        assert_eq!(
            street_key_from_bgid("https://databrussels.be/id/streetname|2461|1"),
            "https://databrussels.be/id/streetname|2461"
        );
        assert_eq!(
            street_key_from_bgid("ns|oid|12A"),
            "ns|oid"
        );
    }

    #[test]
    fn street_key_passthrough_when_no_second_pipe() {
        // Defensive: malformed bgid — treat the whole thing as the street key.
        assert_eq!(street_key_from_bgid("ns|oid"), "ns|oid");
        assert_eq!(street_key_from_bgid("nopipe"), "nopipe");
    }
}
