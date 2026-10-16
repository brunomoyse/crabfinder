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
use tantivy::{DocAddress, IndexReader, SegmentReader, TantivyDocument};

use crabfinder_indexer::index::CrabSchema;

use crate::mid_street::street_key_from_bgid;

#[derive(Debug)]
pub struct AddressCountIndex {
    per_segment: HashMap<SegmentId, Arc<Vec<u32>>>,
    pub n_docs: u64,
    pub n_streets: usize,
    pub max_count: u32,
}

pub type SharedAddressCountIndex = Arc<AddressCountIndex>;

impl AddressCountIndex {
    pub fn build(reader: &IndexReader, schema: &CrabSchema) -> Result<Self> {
        Self::build_visiting(reader, schema, |_, _| {})
    }

    /// `build`, handing every stored document of Pass 1 to `visit` too, so
    /// another startup structure (autocomplete, v3 Phase 11.5) needs no
    /// second walk of the doc store.
    pub fn build_visiting(
        reader: &IndexReader,
        schema: &CrabSchema,
        mut visit: impl FnMut(&TantivyDocument, DocAddress),
    ) -> Result<Self> {
        let searcher = reader.searcher();

        let mut street_counts: HashMap<String, u32> = HashMap::new();
        let mut total_docs: u64 = 0;
        for (seg_ord, seg) in searcher.segment_readers().iter().enumerate() {
            let store = seg
                .get_store_reader(10)
                .context("opening store reader (Pass 1)")?;
            for (doc_id, doc_res) in store
                .iter::<TantivyDocument>(seg.alive_bitset())
                .enumerate()
            {
                let doc = doc_res.context("reading doc (Pass 1)")?;
                visit(&doc, DocAddress::new(seg_ord as u32, doc_id as u32));
                if let Some(bgid) = doc
                    .get_first(schema.building_group_id)
                    .and_then(|v| v.as_str())
                {
                    let key = street_key_from_bgid(bgid).unwrap_or(bgid);
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
            for (doc_id, doc_res) in store
                .iter::<TantivyDocument>(seg.alive_bitset())
                .enumerate()
            {
                let doc = doc_res.context("reading doc (Pass 2)")?;
                if let Some(bgid) = doc
                    .get_first(schema.building_group_id)
                    .and_then(|v| v.as_str())
                {
                    let key = street_key_from_bgid(bgid).unwrap_or(bgid);
                    if let Some(&c) = street_counts.get(key) {
                        counts[doc_id] = c;
                    }
                }
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
    use crabfinder_test_support::{TestIndex, fixtures};

    #[test]
    fn counts_addresses_per_street() {
        // Louise: 8 addresses; Rue Paire: 3.
        let ti = TestIndex::build(&fixtures::louise_street());
        let idx = AddressCountIndex::build(&ti.reader, &ti.schema).unwrap();
        assert_eq!(idx.n_docs, 11);
        assert_eq!(idx.n_streets, 2);
        assert_eq!(idx.max_count, 8);
        let searcher = ti.reader.searcher();
        let mut all: Vec<u32> = searcher
            .segment_readers()
            .iter()
            .flat_map(|seg| idx.for_segment(seg).unwrap().as_ref().clone())
            .collect();
        all.sort_unstable();
        assert_eq!(all, [3, 3, 3, 8, 8, 8, 8, 8, 8, 8, 8]);
    }
}
