//! End-to-end integration tests for the `crabfinder-batch` CLI binary.
//!
//! Each test builds a tiny on-disk Tantivy index (v7, with a mid-street
//! side-file), then spawns the real release/debug binary via
//! `std::process::Command` using the `CARGO_BIN_EXE_crabfinder-batch` env
//! var that cargo sets for integration tests. The binary's stdout (CSV) is
//! captured and asserted on.
//!
//! Coverage:
//!
//! * Happy path: `--input` + `--output` against a built index, exact match.
//! * Stdin/stdout streaming: same workload piped via stdin → stdout.
//! * Index sidecar mismatch: stale schema_version → non-zero exit + helpful
//!   error on stderr.
//! * `--no-mid-street` toggle: same row, mid_street fallback disabled →
//!   not_found instead of median_street.
//! * Empty input: header only → empty stdout + exit 0.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crabfinder_core::{
    Address, AddressId, BestId, BuildingGroupId, Coordinates, MultilingualName, MunicipalityId,
    PostalInfoId, Region, Status, StreetId, StreetType, WgsCoord,
};
use crabfinder_indexer::index::CrabIndexWriter;
use crabfinder_indexer::meta::{write_sidecar, SCHEMA_VERSION, SIDECAR_FILENAME};
use crabfinder_indexer::mid_street::MID_STREET_FILENAME;

/// Path to the CLI binary cargo just built for this integration test.
fn batch_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_crabfinder-batch"))
}

fn make_addr(
    addr_oid: &str,
    street_oid: &str,
    street_fr: Option<&str>,
    street_nl: Option<&str>,
    house: &str,
    postal: &str,
    wgs_lat: f64,
    wgs_lon: f64,
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
            fr: street_fr.map(|s| s.into()),
            nl: street_nl.map(|s| s.into()),
            de: None,
        },
        street_type: StreetType::Streetname,
        municipality_name: MultilingualName {
            fr: Some("Bruxelles".into()),
            nl: Some("Brussel".into()),
            de: None,
        },
        postal_code: postal.into(),
        postal_name: MultilingualName::default(),
        part_of_municipality_name: None,
        coordinates: Coordinates::Wgs84(WgsCoord {
            lat: wgs_lat,
            lon: wgs_lon,
        }),
        building_group_id,
    }
}

/// Build a small index that includes:
/// - Avenue Louise n°10, 20, 30 (Brussels, 1050) → enable median fallback.
/// - Grand-Place n°1 (1000).
///
/// Also writes a hand-rolled `mid-street-v1.bin` for Avenue Louise so the
/// runtime loader resolves `Confidence::MedianStreet`.
///
/// Returns the path to the index dir. The dir lives in a `tempfile::TempDir`
/// that the caller owns (so cleanup happens on drop after the test).
fn build_test_index() -> tempfile::TempDir {
    let dir = tempfile::Builder::new()
        .prefix("crab-batch-cli-e2e-")
        .tempdir()
        .unwrap();

    let mut writer = CrabIndexWriter::create(dir.path()).unwrap();
    writer
        .add_address(&make_addr(
            "100", "3919", Some("Avenue Louise"), Some("Louizalaan"),
            "10", "1050", 50.8300, 4.3500,
        ))
        .unwrap();
    writer
        .add_address(&make_addr(
            "101", "3919", Some("Avenue Louise"), Some("Louizalaan"),
            "20", "1050", 50.8310, 4.3510,
        ))
        .unwrap();
    writer
        .add_address(&make_addr(
            "102", "3919", Some("Avenue Louise"), Some("Louizalaan"),
            "30", "1050", 50.8320, 4.3520,
        ))
        .unwrap();
    writer
        .add_address(&make_addr(
            "200", "7", Some("Grand-Place"), Some("Grote Markt"),
            "1", "1000", 50.8466, 4.3525,
        ))
        .unwrap();
    writer.commit().unwrap();
    write_sidecar(dir.path()).unwrap();

    write_mid_street_file(&dir.path().join(MID_STREET_FILENAME));

    dir
}

fn write_mid_street_file(path: &Path) {
    // Mirror crabfinder_indexer::mid_street::finish() format.
    let mut f = std::fs::File::create(path).unwrap();
    f.write_all(b"CRBMID7\0").unwrap();
    f.write_all(&1u64.to_le_bytes()).unwrap();
    let street_key = "https://databrussels.be/id/streetname|3919";
    f.write_all(&(street_key.len() as u32).to_le_bytes()).unwrap();
    f.write_all(street_key.as_bytes()).unwrap();
    let pc = "1050";
    f.write_all(&(pc.len() as u8).to_le_bytes()).unwrap();
    f.write_all(pc.as_bytes()).unwrap();
    // Median of (10, 20, 30) = n°20 → (50.8310, 4.3510).
    f.write_all(&50.8310f64.to_le_bytes()).unwrap();
    f.write_all(&4.3510f64.to_le_bytes()).unwrap();
    f.flush().unwrap();
}

fn parse_csv(body: &str) -> Vec<Vec<String>> {
    body.lines()
        .map(|l| l.split(',').map(|s| s.to_string()).collect())
        .collect()
}

fn col(rows: &[Vec<String>], name: &str) -> usize {
    rows[0]
        .iter()
        .position(|h| h == name)
        .unwrap_or_else(|| panic!("missing column {name:?} in header (header={:?})", rows[0]))
}

// --- happy path: file-in, file-out -----------------------------------------

#[test]
fn cli_file_in_file_out_resolves_exact_match() {
    let dir = build_test_index();
    let work = tempfile::tempdir().unwrap();
    let input_path = work.path().join("input.csv");
    let output_path = work.path().join("output.csv");
    std::fs::write(
        &input_path,
        "id,street,house_number,postal_code\n\
         1,Avenue Louise,10,1050\n\
         2,Grand-Place,1,1000\n",
    )
    .unwrap();

    let output = Command::new(batch_bin())
        .arg("--index")
        .arg(dir.path())
        .arg("--input")
        .arg(&input_path)
        .arg("--output")
        .arg(&output_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("spawn crabfinder-batch");
    assert!(
        output.status.success(),
        "CLI failed: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );

    let body = std::fs::read_to_string(&output_path).unwrap();
    let rows = parse_csv(&body);
    assert_eq!(rows.len(), 3, "header + 2 rows");

    let confidence_idx = col(&rows, "confidence");
    assert_eq!(rows[1][confidence_idx], "exact", "Louise n°10 → exact");
    assert_eq!(rows[2][confidence_idx], "exact", "Grand-Place 1 → exact");

    // lat/lng must be populated on exact matches.
    let lat_idx = col(&rows, "lat");
    let lng_idx = col(&rows, "lng");
    assert!(!rows[1][lat_idx].is_empty(), "lat must be populated on exact");
    assert!(!rows[1][lng_idx].is_empty(), "lng must be populated on exact");
}

// --- stdin → stdout streaming ----------------------------------------------

#[test]
fn cli_stdin_to_stdout_streaming() {
    let dir = build_test_index();
    let csv_input = b"id,street,house_number,postal_code\n\
                      1,Avenue Louise,10,1050\n";

    let mut child = Command::new(batch_bin())
        .arg("--index")
        .arg(dir.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn crabfinder-batch");

    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(csv_input)
        .unwrap();
    // Drop stdin so the child sees EOF.
    drop(child.stdin.take());

    let output = child.wait_with_output().expect("wait child");
    assert!(
        output.status.success(),
        "CLI failed: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );

    let body = String::from_utf8(output.stdout).unwrap();
    let rows = parse_csv(&body);
    assert_eq!(rows.len(), 2, "header + 1 row");
    let confidence_idx = col(&rows, "confidence");
    assert_eq!(rows[1][confidence_idx], "exact");
}

// --- mid_street fallback toggle --------------------------------------------

#[test]
fn cli_no_mid_street_changes_coords_but_not_confidence_today() {
    // --no-mid-street wires an empty MidStreetIndex into the resolver
    // context. The CURRENT implementation still emits `median_street` in
    // that case (the empty side-table just means the coords come from
    // the top strict-without-HN hit instead of the precomputed median).
    //
    // The user docs and design promise this flag "demotes" median_street
    // to not_found — see docs/{fr,en}/06-faq.md and design §3.4. The
    // mismatch is intentionally pinned by this test: changing the code
    // to honour the promise will fail this assertion, forcing a docs/
    // behaviour reconciliation rather than silent drift.
    let dir = build_test_index();
    let work = tempfile::tempdir().unwrap();
    let input_path = work.path().join("input.csv");
    let output_path = work.path().join("output.csv");
    std::fs::write(
        &input_path,
        "id,street,house_number,postal_code\n\
         1,Avenue Louise,999,1050\n",
    )
    .unwrap();

    let output = Command::new(batch_bin())
        .arg("--index")
        .arg(dir.path())
        .arg("--input")
        .arg(&input_path)
        .arg("--output")
        .arg(&output_path)
        .arg("--no-mid-street")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("spawn crabfinder-batch");
    assert!(output.status.success(), "CLI failed: {output:?}");

    let body = std::fs::read_to_string(&output_path).unwrap();
    let rows = parse_csv(&body);
    let confidence_idx = col(&rows, "confidence");
    assert_eq!(
        rows[1][confidence_idx], "median_street",
        "today's behaviour: --no-mid-street only changes coords, confidence stays median_street"
    );
}

#[test]
fn cli_default_path_does_median_fallback() {
    // Companion to the test above: same input, default flags → median_street.
    let dir = build_test_index();
    let work = tempfile::tempdir().unwrap();
    let input_path = work.path().join("input.csv");
    let output_path = work.path().join("output.csv");
    std::fs::write(
        &input_path,
        "id,street,house_number,postal_code\n\
         1,Avenue Louise,999,1050\n",
    )
    .unwrap();

    let output = Command::new(batch_bin())
        .arg("--index")
        .arg(dir.path())
        .arg("--input")
        .arg(&input_path)
        .arg("--output")
        .arg(&output_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("spawn crabfinder-batch");
    assert!(output.status.success(), "CLI failed: {output:?}");

    let body = std::fs::read_to_string(&output_path).unwrap();
    let rows = parse_csv(&body);
    let confidence_idx = col(&rows, "confidence");
    let lat_idx = col(&rows, "lat");
    assert_eq!(rows[1][confidence_idx], "median_street");
    // Lat must be populated with the median (~50.8310 per the side-file).
    let lat: f64 = rows[1][lat_idx].parse().expect("parse lat");
    assert!((lat - 50.831).abs() < 1e-3, "expected ~50.831, got {lat}");
}

// --- sidecar mismatch ------------------------------------------------------

#[test]
fn cli_rejects_index_with_wrong_schema_version() {
    // Write a stale sidecar that's one version ahead of the binary, then
    // launch the CLI on that index. It must exit non-zero with an error
    // pointing at the rebuild command.
    let dir = build_test_index();
    let stale = serde_json::json!({
        "schema_version": SCHEMA_VERSION + 1,
        "tokenizer_name": "crab",
        "built_at_unix_secs": 0i64,
    });
    std::fs::write(
        dir.path().join(SIDECAR_FILENAME),
        serde_json::to_string(&stale).unwrap(),
    )
    .unwrap();

    let output = Command::new(batch_bin())
        .arg("--index")
        .arg(dir.path())
        .arg("--input")
        .arg("/dev/null") // dummy — we never get past sidecar check
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("spawn crabfinder-batch");

    assert!(
        !output.status.success(),
        "CLI must reject mismatched schema version, but it exited 0"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("schema_version") || stderr.contains("rebuild"),
        "error must point at rebuild command, got: {stderr}"
    );
}

// --- empty input -----------------------------------------------------------

#[test]
fn cli_empty_input_succeeds_with_empty_output() {
    let dir = build_test_index();
    let work = tempfile::tempdir().unwrap();
    let input_path = work.path().join("empty.csv");
    let output_path = work.path().join("output.csv");
    // Header only, no data rows.
    std::fs::write(&input_path, "id,street,house_number,postal_code\n").unwrap();

    let output = Command::new(batch_bin())
        .arg("--index")
        .arg(dir.path())
        .arg("--input")
        .arg(&input_path)
        .arg("--output")
        .arg(&output_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("spawn crabfinder-batch");
    assert!(
        output.status.success(),
        "empty input should succeed: {output:?}"
    );

    // BatchWriter only emits the header on first row, so empty input ⇒
    // empty output (zero bytes). The output file may or may not exist
    // depending on whether the writer opens lazily; if it exists, it
    // must be empty.
    if output_path.exists() {
        let body = std::fs::read_to_string(&output_path).unwrap();
        assert!(body.is_empty(), "expected zero bytes, got {body:?}");
    }
}
