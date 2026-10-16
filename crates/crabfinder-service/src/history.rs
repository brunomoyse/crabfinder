//! BeSt id resolution (v3 Phase 11.4): `/lookup` and the batch `best_id`
//! column turn an id kept from any earlier dump into today's address.
//!
//! An id resolves, in order, to:
//! 1. the address with exactly that id (`current`);
//! 2. the address whose object key (`best_oid`, schema v12) is the id, for
//!    an id given without its version (`current`);
//! 3. the address with the id's object under another version
//!    (`outdated_version`);
//! 4. a record of the history side-file (`history-v1.json.gz`): `retired`,
//!    `reserved` or `rejected`;
//! 5. nothing (`unknown`).
//!
//! BeSt carries no successor link, so a history record's successor is
//! derived here, from BeSt alone: a current address at the same street
//! object, house number and box (`same_number_box`), else at the same
//! number (`same_number`), else the current address standing within
//! `SAME_POSITION_M` of the record's last position (`same_position`; needs
//! the R-tree, so the batch CLI, which skips it, never uses this rule).

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use serde::Serialize;
use tantivy::collector::TopDocs;
use tantivy::query::TermQuery;
use tantivy::schema::IndexRecordOption;
use tantivy::schema::Value;
use tantivy::{DocAddress, Searcher, TantivyDocument, Term};

use crabfinder_indexer::history::{self, HistoryRecord};
use crabfinder_indexer::index::CrabSchema;

use crate::resolver::normalize_box;
use crate::spatial::SpatialIndex;

/// How far a current address may stand from a retired one's last position
/// to be its `same_position` successor.
pub const SAME_POSITION_M: f64 = 5.0;

/// Units read when looking for a same-number successor.
const MAX_UNITS: usize = 1000;

/// History records by object key (`HistoryRecord::object_key`), every
/// version of an object together.
#[derive(Default)]
pub struct HistoryIndex {
    by_key: HashMap<String, Vec<HistoryRecord>>,
    len: usize,
}

pub type SharedHistoryIndex = Arc<HistoryIndex>;

impl HistoryIndex {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn from_records(records: Vec<HistoryRecord>) -> Self {
        let len = records.len();
        let mut by_key: HashMap<String, Vec<HistoryRecord>> = HashMap::new();
        for r in records {
            by_key
                .entry(r.object_key().to_string())
                .or_default()
                .push(r);
        }
        Self { by_key, len }
    }

    /// `<index_dir>/history-v1.json.gz`; empty when the file is absent.
    pub fn load(index_dir: &Path) -> Result<Self> {
        Ok(Self::from_records(history::load(index_dir)?))
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The record `id` names: that exact version, else (an id without
    /// version, or a version BeSt never shipped) the object's latest.
    pub fn find(&self, id: &str) -> Option<&HistoryRecord> {
        let versions = self
            .by_key
            .get(id)
            .or_else(|| self.by_key.get(id.rsplit_once('/')?.0))?;
        versions
            .iter()
            .find(|r| r.id == id)
            .or_else(|| versions.iter().max_by(|a, b| a.id.cmp(&b.id)))
    }
}

/// What an id is today.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IdStatus {
    Current,
    OutdatedVersion,
    Retired,
    Reserved,
    Rejected,
    Unknown,
}

impl IdStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::OutdatedVersion => "outdated_version",
            Self::Retired => "retired",
            Self::Reserved => "reserved",
            Self::Rejected => "rejected",
            Self::Unknown => "unknown",
        }
    }

    fn of_record(r: &HistoryRecord) -> Self {
        match r.status.as_str() {
            "reserved" => Self::Reserved,
            "rejected" => Self::Rejected,
            _ => Self::Retired,
        }
    }
}

/// How a history record's successor was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SuccessorRule {
    SameNumberBox,
    SameNumber,
    SamePosition,
}

impl SuccessorRule {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SameNumberBox => "same_number_box",
            Self::SameNumber => "same_number",
            Self::SamePosition => "same_position",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Successor {
    pub rule: SuccessorRule,
    pub doc: DocAddress,
    /// Metres from the record's last position (`same_position` only).
    pub distance_m: Option<f64>,
}

/// `resolve_id`'s answer.
#[derive(Debug, Clone)]
pub struct IdResolution {
    pub status: IdStatus,
    /// The address itself (`current`, `outdated_version`).
    pub doc: Option<DocAddress>,
    /// The history record (`retired`, `reserved`, `rejected`).
    pub record: Option<HistoryRecord>,
    pub successor: Option<Successor>,
}

impl IdResolution {
    fn unknown() -> Self {
        Self {
            status: IdStatus::Unknown,
            doc: None,
            record: None,
            successor: None,
        }
    }

    /// The address the id leads to today: itself, or its successor.
    pub fn target(&self) -> Option<DocAddress> {
        self.doc.or(self.successor.map(|s| s.doc))
    }
}

/// Resolve one BeSt id (see the module docs for the order).
pub fn resolve_id(
    searcher: &Searcher,
    schema: &CrabSchema,
    history: &HistoryIndex,
    spatial: &SpatialIndex,
    id: &str,
) -> tantivy::Result<IdResolution> {
    let id = id.trim();
    let current = |doc, status| IdResolution {
        status,
        doc: Some(doc),
        record: None,
        successor: None,
    };
    if id.is_empty() {
        return Ok(IdResolution::unknown());
    }
    if let Some(doc) = first(searcher, schema.best_id, id)? {
        return Ok(current(doc, IdStatus::Current));
    }
    if let Some(doc) = first(searcher, schema.best_oid, id)? {
        return Ok(current(doc, IdStatus::Current));
    }
    if let Some((key, _version)) = id.rsplit_once('/')
        && let Some(doc) = first(searcher, schema.best_oid, key)?
    {
        return Ok(current(doc, IdStatus::OutdatedVersion));
    }
    let Some(record) = history.find(id) else {
        return Ok(IdResolution::unknown());
    };
    Ok(IdResolution {
        status: IdStatus::of_record(record),
        doc: None,
        successor: successor(searcher, schema, spatial, record)?,
        record: Some(record.clone()),
    })
}

/// The current address that took `record`'s place, if BeSt has one.
fn successor(
    searcher: &Searcher,
    schema: &CrabSchema,
    spatial: &SpatialIndex,
    record: &HistoryRecord,
) -> tantivy::Result<Option<Successor>> {
    let q = TermQuery::new(
        Term::from_field_text(schema.building_group_id, &record.building_group_id()),
        IndexRecordOption::Basic,
    );
    let mut units: Vec<(Option<String>, String, DocAddress)> = Vec::new();
    for (_, addr) in searcher.search(&q, &TopDocs::with_limit(MAX_UNITS).order_by_score())? {
        let doc = searcher.doc::<TantivyDocument>(addr)?;
        let text = |f| doc.get_first(f).and_then(|v| v.as_str()).map(String::from);
        units.push((
            text(schema.box_number).map(|b| normalize_box(&b)),
            text(schema.best_id).unwrap_or_default(),
            addr,
        ));
    }
    // The building's own entry first (no box), then by box and id: the
    // same order as `/building/{id}/units`.
    units.sort_by(|a, b| {
        a.0.is_some()
            .cmp(&b.0.is_some())
            .then(a.0.cmp(&b.0))
            .then(a.1.cmp(&b.1))
    });
    let wanted = record.box_number.as_deref().map(normalize_box);
    let found = |rule, doc| {
        Some(Successor {
            rule,
            doc,
            distance_m: None,
        })
    };
    if let Some((_, _, doc)) = units.iter().find(|u| u.0 == wanted) {
        return Ok(found(SuccessorRule::SameNumberBox, *doc));
    }
    if let Some((_, _, doc)) = units.first() {
        return Ok(found(SuccessorRule::SameNumber, *doc));
    }
    let (Some(lat), Some(lon)) = (record.lat, record.lon) else {
        return Ok(None);
    };
    Ok(spatial
        .nearest(lat, lon, 1, Some(SAME_POSITION_M))
        .first()
        .map(|h| Successor {
            rule: SuccessorRule::SamePosition,
            doc: h.doc_addr,
            distance_m: Some(h.distance_m),
        }))
}

fn first(
    searcher: &Searcher,
    field: tantivy::schema::Field,
    value: &str,
) -> tantivy::Result<Option<DocAddress>> {
    let q = TermQuery::new(
        Term::from_field_text(field, value),
        IndexRecordOption::Basic,
    );
    Ok(searcher
        .search(&q, &TopDocs::with_limit(1).order_by_score())?
        .first()
        .map(|(_, a)| *a))
}
