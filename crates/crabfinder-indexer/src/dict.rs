//! In-memory dictionaries assembled in Pass 1, queried during Pass 2.
//!
//! Key shape per file 07 §5.1 / file 11 §9.2:
//! `HashMap<(namespace, object_identifier), Vec<(version_identifier, entry)>>`.
//!
//! Lookup is exact-triple first, falling back to most-recent version with a
//! `tracing::warn!` (the fallback never fires on a stock BOSA bundle but
//! protects against future drift).

use std::collections::HashMap;

use crabfinder_core::{BestId, Municipality, PartOfMunicipality, PostalInfo, Street};

type JoinKey = (String, String);

#[derive(Debug)]
pub struct VersionedDict<V> {
    inner: HashMap<JoinKey, Vec<(String, V)>>,
}

impl<V> Default for VersionedDict<V> {
    fn default() -> Self {
        Self {
            inner: HashMap::new(),
        }
    }
}

impl<V> VersionedDict<V> {
    pub fn insert(&mut self, id: &BestId, value: V) {
        let key: JoinKey = (id.namespace.clone(), id.object_identifier.clone());
        self.inner
            .entry(key)
            .or_default()
            .push((id.version_identifier.clone(), value));
    }

    pub fn resolve(&self, ref_id: &BestId) -> Option<&V> {
        let key = (ref_id.namespace.clone(), ref_id.object_identifier.clone());
        let versions = self.inner.get(&key)?;
        if let Some((_, v)) = versions
            .iter()
            .find(|(ver, _)| ver == &ref_id.version_identifier)
        {
            return Some(v);
        }
        // Fallback: most-recent (last) version with a warning
        tracing::warn!(
            ns = %ref_id.namespace,
            oid = %ref_id.object_identifier,
            requested = %ref_id.version_identifier,
            "dict version fallback (no exact match)"
        );
        versions.last().map(|(_, v)| v)
    }

    pub fn len(&self) -> usize {
        // total number of (ns, oid) entries — not counting versions
        self.inner.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Iterate the most-recent version of each (ns, oid) entry. Order is
    /// HashMap-arbitrary — callers that need stability should sort.
    pub fn iter_latest(&self) -> impl Iterator<Item = &V> {
        self.inner
            .values()
            .filter_map(|versions| versions.last().map(|(_, v)| v))
    }
}

/// All four dictionaries combined. Built once in Pass 1, shared as `Arc` by
/// Pass 2 workers (Phase 2). Phase 1 walking skeleton uses it single-threaded.
#[derive(Debug, Default)]
pub struct DictBundle {
    pub municipalities: VersionedDict<Municipality>,
    pub postal_infos: VersionedDict<PostalInfo>,
    pub streets: VersionedDict<Street>,
    pub parts_of_municipality: VersionedDict<PartOfMunicipality>,
}

impl DictBundle {
    pub fn from_records(
        municipalities: Vec<Municipality>,
        postal_infos: Vec<PostalInfo>,
        streets: Vec<Street>,
        parts_of_municipality: Vec<PartOfMunicipality>,
    ) -> Self {
        let mut b = DictBundle::default();
        for m in municipalities {
            let id = m.id.0.clone();
            b.municipalities.insert(&id, m);
        }
        for p in postal_infos {
            let id = p.id.0.clone();
            b.postal_infos.insert(&id, p);
        }
        for s in streets {
            let id = s.id.0.clone();
            b.streets.insert(&id, s);
        }
        for p in parts_of_municipality {
            let id = p.id.0.clone();
            b.parts_of_municipality.insert(&id, p);
        }
        b
    }

    pub fn summary(&self) -> String {
        format!(
            "municipalities={} postal_infos={} streets={} parts_of_municipality={}",
            self.municipalities.len(),
            self.postal_infos.len(),
            self.streets.len(),
            self.parts_of_municipality.len(),
        )
    }
}
