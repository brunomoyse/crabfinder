//! CSV reader + writer for batch geocoding.
//!
//! Schema (per `docs/design-batch-geocoding.md` §3.2):
//! ```text
//! Input:   id, street, house_number?, postal_code?
//! Output:  id, street, house_number, postal_code,
//!          matched_street, matched_house_number, matched_postal_code,
//!          lat, lng,
//!          confidence, fallback_reason, house_number_delta, street_match_score,
//!          building_group_id, best_id
//! ```
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
#[derive(Debug, Clone, Deserialize)]
pub struct InputRow {
    pub id: String,
    pub street: String,
    #[serde(default, deserialize_with = "empty_string_as_none")]
    pub house_number: Option<String>,
    #[serde(default, deserialize_with = "empty_string_as_none")]
    pub postal_code: Option<String>,
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
    if semis > commas {
        b';'
    } else {
        b','
    }
}

/// Read CSV rows from `r`. The caller is responsible for delimiter detection
/// (use [`sniff_delimiter`] when needed). Returns an iterator that yields
/// one `Result<InputRow>` per data row; the iterator is lazy so 100k+ row
/// files don't all sit in memory.
///
/// `flexible(true)` lets the csv parser tolerate trailing-comma rows where
/// optional columns are absent — Excel exports of 2-column files into a
/// 4-column template would otherwise fail.
pub fn read_rows<R: Read + 'static>(
    r: R,
    delim: u8,
) -> impl Iterator<Item = Result<InputRow, BatchError>> {
    let reader = csv::ReaderBuilder::new()
        .delimiter(delim)
        .has_headers(true)
        .flexible(true)
        .trim(csv::Trim::All)
        .from_reader(r);
    reader.into_deserialize::<InputRow>().enumerate().map(|(idx, res)| {
        res.map_err(|e| BatchError::Read {
            // +2: +1 for 0-based index, +1 for the header row.
            line: (idx as u64) + 2,
            source: e,
        })
    })
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
    #[allow(dead_code)] // wired up by Lot E
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
        assert!(buf.is_empty(), "expected zero bytes, got {} bytes", buf.len());
    }

    // --- degraded-input cases (the docs promise specific behaviour here,
    //     these tests pin the contract so the docs and code stay in sync).

    #[test]
    fn missing_required_column_in_header_errors_on_first_row() {
        // `street` is required. If the header omits it, deserialization
        // fails on the first data row with a structured error pointing at
        // line 2 (line 1 = header).
        let csv = "id,house_number,postal_code\n\
                   1,32,1050\n";
        let err = collect_rows(csv, b',').unwrap_err();
        match err {
            BatchError::Read { line, .. } => assert_eq!(line, 2),
            other => panic!("expected Read{{line:2}}, got {other:?}"),
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
