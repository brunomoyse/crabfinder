//! CSV reader + writer for batch geocoding.
//!
//! Schema (per `docs/design-batch-geocoding.md` §3.2, extended in v2):
//! ```text
//! Input:   id, street, house_number?, postal_code?, box_number?, city?
//!     or:  id, address, postal_code?            (free text, v2 Phase 6)
//!     and/or: best_id                           (v3 Phase 11.4)
//! Output:  id, street, house_number, postal_code,
//!          matched_street, matched_house_number, matched_postal_code,
//!          lat, lng,
//!          confidence, fallback_reason, house_number_delta, street_match_score,
//!          building_group_id, best_id,
//!          address, label, position_source, ambiguous  (appended in v2)
//!          status, corrections         (only with `corrections`, v3 Phase 11.2)
//!          score_gap, alternatives     (only with `alternatives`, v3 Phase 11.3)
//!          id_status, successor_best_id (only when a row has a `best_id`, v3 Phase 11.4)
//! ```
//!
//! Output is CSV, or a GeoJSON FeatureCollection (`OutputFormat`).
//!
//! Delimiter handling: comma by default, semicolon supported (Excel-BE
//! defaults). The façade uses `sniff_delimiter()` to peek at the first line.

use std::io::{BufRead, Read, Write};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// One row of input. Matches the design §3.2 input schema.
///
/// The deserializer is permissive: empty strings in `house_number` /
/// `postal_code` map to `None`, so an Excel-exported CSV with trailing
/// commas behaves the way users expect.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct InputRow {
    pub id: String,
    /// Street name. Empty on free-text rows.
    #[serde(default)]
    pub street: String,
    #[serde(default, deserialize_with = "empty_string_as_none")]
    pub house_number: Option<String>,
    #[serde(default, deserialize_with = "empty_string_as_none")]
    pub postal_code: Option<String>,
    /// Optional box / unit column (Phase 2). Prefers the matching unit as
    /// the building's representative (`best_id`); never filters.
    #[serde(default, deserialize_with = "empty_string_as_none")]
    pub box_number: Option<String>,
    /// Whole address as free text (v2 Phase 6): when present, it is parsed
    /// like `/geocode?q=` and the street / number / box columns are
    /// ignored; a `postal_code` column still overrides the parsed one.
    #[serde(default, deserialize_with = "empty_string_as_none")]
    pub address: Option<String>,
    /// Municipality or locality name (v3 Phase 11.2): matched like street
    /// words, and compared with the address's place names by the
    /// `corrections` option. Ignored on free-text rows.
    #[serde(default, deserialize_with = "empty_string_as_none")]
    pub city: Option<String>,
    /// A BeSt address id, with or without version (v3 Phase 11.4): looked
    /// up first, old ids included (`resolver::resolve_by_id`); the address
    /// columns are geocoded only when it leads to no current address.
    #[serde(default, deserialize_with = "empty_string_as_none")]
    pub best_id: Option<String>,
}

/// One row of output. Field order is the wire contract — `csv::Writer`
/// emits columns in struct-declaration order, so reshuffling fields is
/// a breaking change.
///
/// `Option<T>` fields render as empty strings via `csv` + serde defaults,
/// keeping the file shape stable even on `not_found` rows.
#[derive(Debug, Clone, Serialize)]
pub struct OutputRow {
    pub id: String,
    pub street: String,
    pub house_number: Option<String>,
    pub postal_code: Option<String>,
    pub matched_street: Option<String>,
    pub matched_house_number: Option<String>,
    pub matched_postal_code: Option<String>,
    pub lat: Option<f64>,
    pub lng: Option<f64>,
    pub confidence: String,
    pub fallback_reason: Option<String>,
    pub house_number_delta: Option<i32>,
    pub street_match_score: Option<f32>,
    pub building_group_id: Option<String>,
    pub best_id: Option<String>,
    // --- appended in v2 Phase 6 (keep after the v1 columns) ---
    /// The free-text input, echoed.
    pub address: Option<String>,
    /// The match on one line (`Address::display_name`).
    pub label: Option<String>,
    /// How the matched coordinates were obtained: `measured`, or an
    /// imputation (`same_number`, `interpolated`, `street`, `locality`).
    pub position_source: Option<String>,
    /// Two near-equal answers on different streets (`/geocode`'s
    /// `ambiguous`).
    pub ambiguous: bool,
    // --- only with the `corrections` option (v3 Phase 11.2) ---
    /// `/validate`'s status: `valid`, `corrected`, `ambiguous`,
    /// `incomplete` or `not_found`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// `kind:field:input->official` per correction, `; `-separated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub corrections: Option<String>,
    // --- only with the `alternatives` option (v3 Phase 11.3) ---
    /// `/geocode`'s `score_gap` (empty when not comparable).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score_gap: Option<Option<f32>>,
    /// The best answers' municipalities, best first:
    /// `Charleroi (6000); La Louvière (7100)`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alternatives: Option<String>,
    // --- only when some row has a `best_id` (v3 Phase 11.4) ---
    /// `/lookup`'s status of the input id: `current`, `outdated_version`,
    /// `retired`, `reserved`, `rejected` or `unknown` (empty on rows
    /// without one).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id_status: Option<String>,
    /// The id of the address that replaced a retired / reserved /
    /// rejected one (the row's address).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub successor_best_id: Option<String>,
}

/// Batch output format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OutputFormat {
    #[default]
    Csv,
    /// A GeoJSON FeatureCollection: one Point feature per row (null
    /// geometry when not found), the CSV columns as properties.
    GeoJson,
}

impl std::str::FromStr for OutputFormat {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "csv" => Ok(Self::Csv),
            "geojson" | "json" => Ok(Self::GeoJson),
            other => Err(format!(
                "unknown format '{other}' (expected csv or geojson)"
            )),
        }
    }
}

impl OutputFormat {
    pub fn content_type(self) -> &'static str {
        match self {
            Self::Csv => "text/csv; charset=utf-8",
            Self::GeoJson => "application/geo+json",
        }
    }
}

/// Write `rows` as one GeoJSON FeatureCollection. Properties keep the
/// CSV column order.
pub fn write_geojson<W: Write>(rows: &[OutputRow], w: W) -> Result<(), BatchError> {
    let mut sink = OutputSink::new(OutputFormat::GeoJson, w, 0);
    for r in rows {
        sink.write_row(r)?;
    }
    sink.finish().map(drop)
}

/// The opening of a GeoJSON output, up to its first feature.
const GEOJSON_OPEN: &[u8] = br#"{"type":"FeatureCollection","features":["#;

/// One row as a GeoJSON feature: a Point (null geometry when not found),
/// the CSV columns as properties.
#[derive(Serialize)]
struct Feature<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    geometry: Option<Point>,
    properties: &'a OutputRow,
}

#[derive(Serialize)]
struct Point {
    #[serde(rename = "type")]
    kind: &'static str,
    coordinates: [f64; 2],
}

/// Row-at-a-time writer of either output format (v3 Phase 11.6): the
/// HTTP route streams through it, batch jobs append to a file, the CLI
/// writes to stdout. Same bytes as writing every row at once.
pub enum OutputSink<W: Write> {
    Csv(Box<BatchWriter<W>>),
    GeoJson {
        w: W,
        /// Features already in the output.
        rows: usize,
    },
}

impl<W: Write> OutputSink<W> {
    /// A sink over `w`. `rows_before` > 0 continues an output that already
    /// holds that many rows (a resumed job): no second CSV header or
    /// GeoJSON opening.
    pub fn new(format: OutputFormat, w: W, rows_before: usize) -> Self {
        match format {
            OutputFormat::Csv if rows_before > 0 => Self::Csv(Box::new(BatchWriter::continuing(w))),
            OutputFormat::Csv => Self::Csv(Box::new(BatchWriter::new(w))),
            OutputFormat::GeoJson => Self::GeoJson {
                w,
                rows: rows_before,
            },
        }
    }

    pub fn write_row(&mut self, row: &OutputRow) -> Result<(), BatchError> {
        match self {
            Self::Csv(w) => w.write_row(row),
            Self::GeoJson { w, rows } => {
                w.write_all(if *rows == 0 { GEOJSON_OPEN } else { b"," })?;
                let feature = Feature {
                    kind: "Feature",
                    geometry: row.lat.zip(row.lng).map(|(lat, lng)| Point {
                        kind: "Point",
                        coordinates: [lng, lat],
                    }),
                    properties: row,
                };
                serde_json::to_writer(&mut *w, &feature).map_err(|e| BatchError::Io(e.into()))?;
                *rows += 1;
                Ok(())
            }
        }
    }

    /// Push what was written so far down to the underlying writer.
    pub fn flush(&mut self) -> Result<(), BatchError> {
        match self {
            Self::Csv(w) => w.flush(),
            Self::GeoJson { w, .. } => Ok(w.flush()?),
        }
    }

    /// Close the output and return the writer, flushed. A CSV without
    /// rows stays empty (no header); a GeoJSON one is an empty
    /// FeatureCollection.
    pub fn finish(self) -> Result<W, BatchError> {
        match self {
            Self::Csv(w) => (*w).into_inner(),
            Self::GeoJson { mut w, rows } => {
                if rows == 0 {
                    w.write_all(GEOJSON_OPEN)?;
                }
                w.write_all(b"]}")?;
                w.flush()?;
                Ok(w)
            }
        }
    }
}

#[derive(Debug, Error)]
pub enum BatchError {
    #[error("csv read at line {line}: {source}")]
    Read {
        line: u64,
        #[source]
        source: csv::Error,
    },
    #[error("csv write: {0}")]
    Write(#[from] csv::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("csv header has none of the `street`, `address` and `best_id` columns")]
    MissingColumn,
}

/// Empty-string-as-None serde helper for optional CSV columns.
fn empty_string_as_none<'de, D>(d: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let opt: Option<String> = Option::deserialize(d)?;
    Ok(opt.filter(|s| !s.is_empty()))
}

/// Sniff the delimiter from a sample of the input.
///
/// Strategy: count semicolons and commas in the first ~1 KiB; whichever
/// is more frequent wins. Defaults to comma on a tie or on empty input —
/// that's the convention most tools default to.
pub fn sniff_delimiter(sample: &[u8]) -> u8 {
    let slice = if sample.len() > 1024 {
        &sample[..1024]
    } else {
        sample
    };
    let semis = slice.iter().filter(|&&b| b == b';').count();
    let commas = slice.iter().filter(|&&b| b == b',').count();
    if semis > commas { b';' } else { b',' }
}

/// Read CSV rows from `r`. The caller is responsible for delimiter detection
/// (use [`sniff_delimiter`] when needed). Returns an iterator that yields
/// one `Result<InputRow>` per data row; the iterator is lazy so 100k+ row
/// files don't all sit in memory.
///
/// `flexible(true)` lets the csv parser tolerate trailing-comma rows where
/// optional columns are absent — Excel exports of 2-column files into a
/// 4-column template would otherwise fail.
///
/// A header without a `street` or `address` column yields one
/// `BatchError::MissingColumn` and nothing else.
pub fn read_rows<R: Read + 'static>(
    r: R,
    delim: u8,
) -> impl Iterator<Item = Result<InputRow, BatchError>> {
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(delim)
        .has_headers(true)
        .flexible(true)
        .trim(csv::Trim::All)
        .from_reader(r);
    let missing = reader.headers().is_ok_and(|h| {
        !h.is_empty()
            && !h
                .iter()
                .any(|c| c == "street" || c == "address" || c == "best_id")
    });
    let header_error = missing.then_some(Err(BatchError::MissingColumn));
    let rows = reader
        .into_deserialize::<InputRow>()
        .take(if missing { 0 } else { usize::MAX })
        .enumerate()
        .map(|(idx, res)| {
            res.map_err(|e| BatchError::Read {
                // +2: +1 for 0-based index, +1 for the header row.
                line: (idx as u64) + 2,
                source: e,
            })
        });
    header_error.into_iter().chain(rows)
}

/// CSV writer wrapper. Holds an open `csv::Writer<W>` and exposes
/// `write_row()` for streaming. Drop it (or call `flush()`) to flush the
/// underlying writer.
pub struct BatchWriter<W: Write> {
    inner: csv::Writer<W>,
    headers_written: bool,
}

impl<W: Write> BatchWriter<W> {
    /// Create a writer with comma delimiter. The header line is emitted
    /// on the first `write_row()` call (so empty output produces no file
    /// content — useful for HTTP streaming where the client cancels
    /// before any rows arrive).
    pub fn new(w: W) -> Self {
        Self::with_delimiter(w, b',')
    }

    pub fn with_delimiter(w: W, delim: u8) -> Self {
        Self {
            inner: csv::WriterBuilder::new()
                .delimiter(delim)
                .has_headers(true)
                .from_writer(w),
            headers_written: false,
        }
    }

    /// A writer continuing a CSV whose header is already written (a
    /// resumed batch job, v3 Phase 11.6).
    pub fn continuing(w: W) -> Self {
        Self {
            inner: csv::WriterBuilder::new().has_headers(false).from_writer(w),
            headers_written: true,
        }
    }

    /// Write one row. The csv crate writes the header on the first
    /// `serialize()` call because `has_headers(true)`.
    pub fn write_row(&mut self, row: &OutputRow) -> Result<(), BatchError> {
        self.inner.serialize(row)?;
        self.headers_written = true;
        Ok(())
    }

    /// Flush the underlying writer.
    pub fn flush(&mut self) -> Result<(), BatchError> {
        self.inner.flush()?;
        Ok(())
    }

    /// Consume the writer and return the inner sink. Use this when the
    /// caller wants to inspect/take ownership of the buffer (e.g. for HTTP
    /// streaming).
    pub fn into_inner(self) -> Result<W, BatchError> {
        self.inner
            .into_inner()
            .map_err(|e| BatchError::Io(std::io::Error::other(e.to_string())))
    }
}

/// Peek the first line of `r` (up to `\n`) into a `Vec<u8>`, returning
/// the bytes alongside a reader that emits both the peeked line and the
/// rest. Lets the CSV reader downstream see the same bytes the sniffer
/// just inspected.
pub fn peek_first_line<R: BufRead>(mut r: R) -> std::io::Result<(Vec<u8>, R)> {
    let mut first = Vec::with_capacity(256);
    r.read_until(b'\n', &mut first)?;
    Ok((first, r))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn collect_rows(s: &str, delim: u8) -> Result<Vec<InputRow>, BatchError> {
        let cur = Cursor::new(s.as_bytes().to_vec());
        read_rows(cur, delim).collect()
    }

    #[test]
    fn reads_comma_csv() {
        let csv = "id,street,house_number,postal_code\n\
                   1,Avenue Louise,32,1050\n\
                   2,Rue Belliard,71,1040\n";
        let rows = collect_rows(csv, b',').unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, "1");
        assert_eq!(rows[0].street, "Avenue Louise");
        assert_eq!(rows[0].house_number.as_deref(), Some("32"));
        assert_eq!(rows[1].id, "2");
    }

    #[test]
    fn reads_semicolon_csv() {
        let csv = "id;street;house_number;postal_code\n\
                   1;Avenue Louise;32;1050\n";
        let rows = collect_rows(csv, b';').unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].postal_code.as_deref(), Some("1050"));
    }

    #[test]
    fn empty_optional_columns_become_none() {
        let csv = "id,street,house_number,postal_code\n\
                   1,Grand-Place,,1000\n\
                   2,Rue Sans Numero,,\n";
        let rows = collect_rows(csv, b',').unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].house_number, None);
        assert_eq!(rows[0].postal_code.as_deref(), Some("1000"));
        assert_eq!(rows[1].house_number, None);
        assert_eq!(rows[1].postal_code, None);
    }

    #[test]
    fn sniff_picks_semicolon_when_dominant() {
        assert_eq!(sniff_delimiter(b"id;a;b;c\n1;2;3;4\n"), b';');
        assert_eq!(sniff_delimiter(b"id,a,b,c\n1,2,3,4\n"), b',');
        // Tie or empty defaults to comma.
        assert_eq!(sniff_delimiter(b""), b',');
        assert_eq!(sniff_delimiter(b"id\n"), b',');
    }

    #[test]
    fn writer_emits_header_and_rows_streaming() {
        let mut buf: Vec<u8> = Vec::new();
        {
            let mut w = BatchWriter::new(&mut buf);
            w.write_row(&OutputRow {
                id: "1".into(),
                street: "Avenue Louise".into(),
                house_number: Some("32".into()),
                postal_code: Some("1050".into()),
                matched_street: Some("Avenue Louise".into()),
                matched_house_number: Some("32".into()),
                matched_postal_code: Some("1050".into()),
                lat: Some(50.83),
                lng: Some(4.36),
                confidence: "exact".into(),
                fallback_reason: None,
                house_number_delta: Some(0),
                street_match_score: Some(12.5),
                building_group_id: Some("ns|s|32".into()),
                best_id: Some("ns/a/1".into()),
                address: None,
                label: Some("Avenue Louise 32, 1050 Ixelles".into()),
                position_source: Some("measured".into()),
                ambiguous: false,
                status: None,
                corrections: None,
                score_gap: None,
                id_status: None,
                successor_best_id: None,
                alternatives: None,
            })
            .unwrap();
            w.flush().unwrap();
        }
        let s = String::from_utf8(buf).unwrap();
        assert!(
            s.starts_with("id,street,house_number,postal_code,matched_street"),
            "header order changed: {s}"
        );
        assert!(s.contains(",exact,"));
        assert!(s.contains(",50.83,"));
        assert!(
            s.lines()
                .next()
                .unwrap()
                .ends_with(",best_id,address,label,position_source,ambiguous"),
            "{s}"
        );
        assert!(
            s.contains(",,\"Avenue Louise 32, 1050 Ixelles\",measured,false"),
            "{s}"
        );
    }

    #[test]
    fn writer_empty_output_emits_no_header() {
        // Per docstring: nothing written means nothing emitted. csv 1.3
        // emits the header on the first row write, so this is automatic.
        let mut buf: Vec<u8> = Vec::new();
        {
            let mut w = BatchWriter::new(&mut buf);
            w.flush().unwrap();
        }
        assert!(
            buf.is_empty(),
            "expected zero bytes, got {} bytes",
            buf.len()
        );
    }

    // --- degraded-input cases (the docs promise specific behaviour here,
    //     these tests pin the contract so the docs and code stay in sync).

    #[test]
    fn header_needs_street_or_address() {
        let csv = "id,house_number,postal_code\n\
                   1,32,1050\n";
        let rows: Vec<_> = read_rows(Cursor::new(csv.as_bytes().to_vec()), b',').collect();
        assert_eq!(rows.len(), 1);
        assert!(matches!(rows[0], Err(BatchError::MissingColumn)));
        // `id` is still required per row.
        let err = collect_rows("street\nAvenue Louise\n", b',').unwrap_err();
        assert!(matches!(err, BatchError::Read { line: 2, .. }), "{err:?}");
    }

    #[test]
    fn reads_free_text_rows() {
        let csv = "id,address\n\
                   1,\"Rue de la Loi 16, 1000 Bruxelles\"\n\
                   2,\n";
        let rows = collect_rows(csv, b',').unwrap();
        assert_eq!(
            rows[0].address.as_deref(),
            Some("Rue de la Loi 16, 1000 Bruxelles")
        );
        assert_eq!(rows[0].street, "");
        assert_eq!(rows[1].address, None);
    }

    #[test]
    fn output_formats() {
        assert_eq!("CSV".parse::<OutputFormat>(), Ok(OutputFormat::Csv));
        assert_eq!("geojson".parse::<OutputFormat>(), Ok(OutputFormat::GeoJson));
        assert_eq!("json".parse::<OutputFormat>(), Ok(OutputFormat::GeoJson));
        assert_eq!(
            "xml".parse::<OutputFormat>(),
            Err("unknown format 'xml' (expected csv or geojson)".into())
        );
        assert_eq!(OutputFormat::default(), OutputFormat::Csv);
        assert!(OutputFormat::Csv.content_type().starts_with("text/csv"));
        assert_eq!(OutputFormat::GeoJson.content_type(), "application/geo+json");
    }

    #[test]
    fn geojson_has_one_feature_per_row() {
        let found = OutputRow {
            id: "1".into(),
            street: "Avenue Louise".into(),
            house_number: None,
            postal_code: None,
            matched_street: None,
            matched_house_number: None,
            matched_postal_code: None,
            lat: Some(50.83),
            lng: Some(4.36),
            confidence: "exact".into(),
            fallback_reason: None,
            house_number_delta: None,
            street_match_score: None,
            building_group_id: None,
            best_id: None,
            address: None,
            label: None,
            position_source: None,
            ambiguous: true,
            status: None,
            corrections: None,
            score_gap: None,
            id_status: None,
            successor_best_id: None,
            alternatives: None,
        };
        let missing = OutputRow {
            id: "2".into(),
            lat: None,
            lng: None,
            confidence: "not_found".into(),
            ..found.clone()
        };
        let mut buf = Vec::new();
        write_geojson(&[found, missing], &mut buf).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&buf).unwrap();
        assert_eq!(v["type"], "FeatureCollection");
        let fs = v["features"].as_array().unwrap();
        assert_eq!(fs.len(), 2);
        assert_eq!(fs[0]["geometry"]["coordinates"][0], 4.36);
        assert_eq!(fs[0]["properties"]["ambiguous"], true);
        assert!(fs[1]["geometry"].is_null());
        assert_eq!(fs[1]["properties"]["confidence"], "not_found");

        // A sink continuing an output (a resumed job) writes the rest of
        // the same bytes, in both formats.
        let rows = [
            OutputRow {
                id: "3".into(),
                ..fs_row(&v, 0)
            },
            OutputRow {
                id: "4".into(),
                ..fs_row(&v, 0)
            },
        ];
        for format in [OutputFormat::Csv, OutputFormat::GeoJson] {
            let mut whole = OutputSink::new(format, Vec::new(), 0);
            for r in &rows {
                whole.write_row(r).unwrap();
            }
            let whole = whole.finish().unwrap();
            let mut head = OutputSink::new(format, Vec::new(), 0);
            head.write_row(&rows[0]).unwrap();
            head.flush().unwrap();
            let mut out = match head {
                OutputSink::Csv(w) => (*w).into_inner().unwrap(),
                OutputSink::GeoJson { w, .. } => w,
            };
            let mut tail = OutputSink::new(format, Vec::new(), 1);
            tail.write_row(&rows[1]).unwrap();
            out.extend(tail.finish().unwrap());
            assert_eq!(
                String::from_utf8(out).unwrap(),
                String::from_utf8(whole).unwrap()
            );
        }
        // Empty outputs.
        let csv = OutputSink::new(OutputFormat::Csv, Vec::new(), 0);
        assert!(csv.finish().unwrap().is_empty());
        let geo = OutputSink::new(OutputFormat::GeoJson, Vec::new(), 0);
        assert_eq!(
            geo.finish().unwrap(),
            br#"{"type":"FeatureCollection","features":[]}"#
        );
    }

    /// An `OutputRow` with the `idx`-th feature's id and coordinates.
    fn fs_row(v: &serde_json::Value, idx: usize) -> OutputRow {
        let f = &v["features"][idx];
        OutputRow {
            id: f["properties"]["id"].as_str().unwrap().into(),
            street: "Avenue Louise".into(),
            house_number: None,
            postal_code: None,
            matched_street: None,
            matched_house_number: None,
            matched_postal_code: None,
            lat: f["geometry"]["coordinates"][1].as_f64(),
            lng: f["geometry"]["coordinates"][0].as_f64(),
            confidence: "exact".into(),
            fallback_reason: None,
            house_number_delta: None,
            street_match_score: None,
            building_group_id: None,
            best_id: None,
            address: None,
            label: None,
            position_source: None,
            ambiguous: false,
            status: None,
            corrections: None,
            score_gap: None,
            id_status: None,
            successor_best_id: None,
            alternatives: None,
        }
    }

    #[test]
    fn too_few_columns_in_data_row_is_tolerated_via_flexible() {
        // We enable `flexible(true)` so an Excel export that ate trailing
        // commas still parses. Verify that a 2-column row against a
        // 4-column header gets the optional columns set to None.
        let csv = "id,street,house_number,postal_code\n\
                   1,Avenue Louise\n";
        let rows = collect_rows(csv, b',').unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].street, "Avenue Louise");
        assert_eq!(rows[0].house_number, None);
        assert_eq!(rows[0].postal_code, None);
    }

    #[test]
    fn second_row_error_carries_correct_line_number() {
        // Line numbers in `BatchError::Read` must point at the offending
        // *data* row, accounting for the header. With one good row first
        // and a malformed second row, the error line is 3.
        //
        // csv 1.4 tolerates unterminated quotes and column-count mismatches
        // (the latter because we set `flexible(true)`), so the cleanest
        // remaining failure mode is invalid UTF-8 in a field — serde's
        // `String` deserializer rejects it via the csv crate's utf-8
        // bridge.
        let mut bytes: Vec<u8> = b"id,street,house_number,postal_code\n\
                                   1,Avenue Louise,32,1050\n\
                                   2,"
        .to_vec();
        bytes.extend_from_slice(&[0xFF, 0xFE]); // invalid UTF-8 in street
        bytes.extend_from_slice(b",71,1040\n");
        let cur = Cursor::new(bytes);
        let mut iter = read_rows(cur, b',');
        let first = iter.next().unwrap();
        assert!(first.is_ok(), "first row should parse");
        let second = iter.next().unwrap();
        match second {
            Err(BatchError::Read { line, .. }) => assert_eq!(line, 3),
            other => panic!("expected error on line 3, got {other:?}"),
        }
    }

    #[test]
    fn utf8_accents_in_street_round_trip() {
        // Adresses belges courantes : st. Étienne, Liège, …
        // Verify accents survive the read path verbatim.
        let csv = "id,street,house_number,postal_code\n\
                   1,Place Saint-Étienne,1,1040\n\
                   2,Liège-Centre,5,4000\n";
        let rows = collect_rows(csv, b',').unwrap();
        assert_eq!(rows[0].street, "Place Saint-Étienne");
        assert_eq!(rows[1].street, "Liège-Centre");
    }
}
