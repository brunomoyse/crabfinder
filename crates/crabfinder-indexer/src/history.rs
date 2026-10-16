//! History side-file (schema v12, v3 Phase 11.4): the BeSt addresses that
//! are not searchable (`retired`, `reserved`, `rejected`), so an id a user
//! kept from an earlier dump still resolves to something.
//!
//! BeSt carries no successor link. A record keeps what is needed to derive
//! one at lookup time (service `history.rs`): its street object and house
//! number (the `building_group_id` of a current address at the same
//! number) and its last position (the current address standing there).
//! Written next to the index as `history-v1.json.gz`.

use std::io::{BufReader, BufWriter, Write};
use std::path::Path;

use anyhow::{Context, Result, bail};
use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use serde::{Deserialize, Serialize};

use crabfinder_core::LambertCoord;

use crate::reproject::Reprojector;
use crate::xml::RawAddress;

pub const HISTORY_FILENAME: &str = "history-v1.json.gz";

/// File format version (independent of the index schema version).
const FORMAT_VERSION: u32 = 1;

/// One non-searchable BeSt address record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryRecord {
    /// Canonical versioned id (`{namespace}/{objectIdentifier}/{version}`).
    pub id: String,
    /// BeSt status: `retired`, `reserved` or `rejected`.
    pub status: String,
    /// `{street namespace}|{street objectIdentifier}`: with `|{house_number}`
    /// appended, the `building_group_id` of the current addresses at the
    /// same number.
    pub street_key: String,
    pub house_number: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub box_number: Option<String>,
    pub postal_code: String,
    /// `endLifeSpanVersion`: when the record was retired, if BeSt says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<String>,
    /// Last WGS84 position; `None` when BeSt had none (`0 0`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lat: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lon: Option<f64>,
}

impl HistoryRecord {
    /// The record's object key (its id without the version), the
    /// `best_oid` form of current addresses.
    pub fn object_key(&self) -> &str {
        self.id.rsplit_once('/').map_or(&self.id, |(k, _)| k)
    }

    /// `building_group_id` of the current addresses at the same number.
    pub fn building_group_id(&self) -> String {
        format!("{}|{}", self.street_key, self.house_number)
    }
}

#[derive(Serialize, Deserialize)]
struct HistoryFile {
    version: u32,
    records: Vec<HistoryRecord>,
}

/// Accumulates the records the status filter drops (`pipeline.rs`).
#[derive(Default)]
pub struct HistoryBuilder {
    records: Vec<HistoryRecord>,
}

impl HistoryBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// Keep an already-built record (tests, tools).
    pub fn push(&mut self, record: HistoryRecord) {
        self.records.push(record);
    }

    /// Keep `raw`, a record left out of the search index.
    pub fn record(&mut self, raw: &RawAddress, reprojector: &Reprojector) {
        let position = reprojector
            .lambert72_to_wgs84(LambertCoord {
                easting: raw.easting,
                northing: raw.northing,
            })
            .ok()
            .filter(|w| w.is_in_belgium());
        self.records.push(HistoryRecord {
            id: raw.id.to_string(),
            status: raw.status.as_str().to_string(),
            street_key: format!(
                "{}|{}",
                raw.street_ref.namespace, raw.street_ref.object_identifier
            ),
            house_number: raw.house_number.clone(),
            box_number: raw.box_number.clone(),
            postal_code: raw.postal_info_ref.object_identifier.clone(),
            end: raw.end_life_span.clone(),
            lat: position.map(|w| w.lat),
            lon: position.map(|w| w.lon),
        });
    }

    /// Write `<dir>/history-v1.json.gz`; returns the record count.
    pub fn finish(self, dir: &Path) -> Result<usize> {
        let path = dir.join(HISTORY_FILENAME);
        let n = self.records.len();
        let f = std::fs::File::create(&path).with_context(|| format!("creating {path:?}"))?;
        let mut gz = GzEncoder::new(BufWriter::new(f), Compression::default());
        serde_json::to_writer(
            &mut gz,
            &HistoryFile {
                version: FORMAT_VERSION,
                records: self.records,
            },
        )
        .with_context(|| format!("writing {path:?}"))?;
        gz.finish()?.flush()?;
        Ok(n)
    }
}

/// Read `<dir>/history-v1.json.gz`. A missing file yields no records.
pub fn load(dir: &Path) -> Result<Vec<HistoryRecord>> {
    let path = dir.join(HISTORY_FILENAME);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let f = std::fs::File::open(&path).with_context(|| format!("opening {path:?}"))?;
    let file: HistoryFile = serde_json::from_reader(BufReader::new(GzDecoder::new(f)))
        .with_context(|| format!("parsing {path:?}"))?;
    if file.version != FORMAT_VERSION {
        bail!(
            "{path:?} has format version {}, expected {FORMAT_VERSION}",
            file.version
        );
    }
    Ok(file.records)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crabfinder_core::{BestId, Status};

    fn raw(oid: &str, status: Status, x: f64, y: f64) -> RawAddress {
        RawAddress {
            id: BestId::new("https://data.vlaanderen.be/id/adres/", oid, "2023-11-01"),
            status,
            house_number: "53".into(),
            box_number: Some("2".into()),
            officially_assigned: true,
            street_ref: BestId::new("https://data.vlaanderen.be/id/straatnaam/", "69906", "1"),
            municipality_ref: BestId::new("https://data.vlaanderen.be/id/gemeente/", "44021", "1"),
            postal_info_ref: BestId::new("https://data.vlaanderen.be/id/postinfo/", "9000", "1"),
            part_of_municipality_ref: None,
            easting: x,
            northing: y,
            end_life_span: Some("2023-11-01T15:15:58Z".into()),
        }
    }

    #[test]
    fn records_round_trip_through_the_file() {
        let rp = Reprojector::new().unwrap();
        let mut b = HistoryBuilder::new();
        assert!(b.is_empty());
        b.record(&raw("200068", Status::Retired, 106_169.49, 194_126.62), &rp);
        b.record(&raw("200069", Status::Reserved, 0.0, 0.0), &rp);
        assert_eq!(b.len(), 2);
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(b.finish(dir.path()).unwrap(), 2);

        let recs = load(dir.path()).unwrap();
        assert_eq!(recs.len(), 2);
        let r = &recs[0];
        assert_eq!(r.status, "retired");
        assert_eq!(r.object_key(), "https://data.vlaanderen.be/id/adres/200068");
        assert_eq!(
            r.building_group_id(),
            "https://data.vlaanderen.be/id/straatnaam/|69906|53"
        );
        assert_eq!(r.postal_code, "9000");
        assert_eq!(r.box_number.as_deref(), Some("2"));
        assert_eq!(r.end.as_deref(), Some("2023-11-01T15:15:58Z"));
        let (lat, lon) = (r.lat.unwrap(), r.lon.unwrap());
        assert!((51.0..51.1).contains(&lat) && (3.7..3.8).contains(&lon));
        // `0 0` is no position.
        assert_eq!(recs[1].status, "reserved");
        assert!(recs[1].lat.is_none() && recs[1].lon.is_none());
    }

    #[test]
    fn missing_file_is_empty_and_bad_version_fails() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load(dir.path()).unwrap().is_empty());

        let f = std::fs::File::create(dir.path().join(HISTORY_FILENAME)).unwrap();
        let mut gz = GzEncoder::new(f, Compression::default());
        gz.write_all(br#"{"version":99,"records":[]}"#).unwrap();
        gz.finish().unwrap();
        assert!(
            load(dir.path())
                .unwrap_err()
                .to_string()
                .contains("format version 99")
        );
    }

    #[test]
    fn object_key_of_an_unversioned_id_is_the_id() {
        let r = HistoryRecord {
            id: "plain".into(),
            status: "retired".into(),
            street_key: "s|1".into(),
            house_number: "1".into(),
            box_number: None,
            postal_code: "1000".into(),
            end: None,
            lat: None,
            lon: None,
        };
        assert_eq!(r.object_key(), "plain");
    }
}
