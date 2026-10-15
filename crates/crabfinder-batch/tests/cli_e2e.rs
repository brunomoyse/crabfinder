//! Tests for the `crabfinder-batch` CLI.
//!
//! Most cases call the library (`run` / `run_with_io`) in-process. A few
//! smoke tests spawn the real binary via `CARGO_BIN_EXE_crabfinder-batch`
//! to pin the argument parsing, stdin/stdout wiring and exit codes.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use crabfinder_batch::{Args, Summary, run, run_with_io};
use crabfinder_indexer::meta::{SCHEMA_VERSION, SIDECAR_FILENAME};
use crabfinder_test_support::{AddressBuilder, TestIndex, col, fixtures, parse_csv};

const HEADER: &str = "id,street,house_number,postal_code\n";

fn index() -> TestIndex {
    TestIndex::build_with_mid_street(&fixtures::batch_small())
}

fn batch_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_crabfinder-batch"))
}

/// Run in-process over an in-memory CSV; returns the summary and output.
fn run_csv(args: &Args, csv: &str) -> (Summary, String) {
    let mut out = Vec::new();
    let summary = run_with_io(args, csv.as_bytes(), &mut out).expect("batch run");
    (summary, String::from_utf8(out).unwrap())
}

#[test]
fn resolves_exact_matches() {
    let ti = index();
    let (summary, body) = run_csv(
        &Args::for_index(ti.path()),
        &format!("{HEADER}1,Avenue Louise,10,1050\n2,Grand-Place,1,1000\n"),
    );
    assert_eq!(summary, Summary { rows: 2 });
    let (h, rows) = parse_csv(&body);
    for r in &rows {
        assert_eq!(col(&h, r, "confidence"), "exact");
        assert!(!col(&h, r, "lat").is_empty() && !col(&h, r, "lng").is_empty());
    }
}

#[test]
fn default_path_does_median_fallback() {
    let ti = index();
    let (_, body) = run_csv(
        &Args::for_index(ti.path()),
        &format!("{HEADER}1,Avenue Louise,999,1050\n"),
    );
    let (h, rows) = parse_csv(&body);
    assert_eq!(col(&h, &rows[0], "confidence"), "median_street");
    let lat: f64 = col(&h, &rows[0], "lat").parse().unwrap();
    assert!((lat - 50.831).abs() < 1e-9, "median of 10/20/30 is n°20");
}

#[test]
fn no_mid_street_skips_street_level_fallbacks() {
    let ti = index();
    let mut args = Args::for_index(ti.path());
    args.no_mid_street = true;
    let (_, body) = run_csv(
        &args,
        &format!(
            "{HEADER}1,Avenue Louise,999,1050
2,Avenue Louise,10,1050
3,Avenue Louise,,1050
"
        ),
    );
    let (h, rows) = parse_csv(&body);
    assert_eq!(col(&h, &rows[0], "confidence"), "not_found");
    assert_eq!(col(&h, &rows[0], "lat"), "");
    assert_eq!(
        col(&h, &rows[1], "confidence"),
        "exact",
        "exact matches are unaffected"
    );
    assert_eq!(
        col(&h, &rows[2], "confidence"),
        "not_found",
        "no number → street-level only"
    );
}

#[test]
fn delimiter_is_sniffed_or_overridden() {
    let ti = index();
    let csv = "id;street;house_number;postal_code\n1;Avenue Louise;20;1050\n";
    let (_, body) = run_csv(&Args::for_index(ti.path()), csv);
    let (h, rows) = parse_csv(&body);
    assert_eq!(col(&h, &rows[0], "matched_house_number"), "20");

    let mut args = Args::for_index(ti.path());
    args.delimiter = Some(';');
    args.parallel = Some(1);
    let (summary, _) = run_csv(&args, csv);
    assert_eq!(summary.rows, 1);
}

#[test]
fn non_ascii_delimiter_is_rejected() {
    let ti = index();
    let mut args = Args::for_index(ti.path());
    args.delimiter = Some('§');
    let err = run_with_io(&args, HEADER.as_bytes(), Vec::new()).unwrap_err();
    assert!(err.to_string().contains("non-ASCII delimiter"));
}

#[test]
fn malformed_csv_is_an_error() {
    let ti = index();
    let err = run_with_io(
        &Args::for_index(ti.path()),
        "id,house_number\n1,10\n".as_bytes(),
        Vec::new(),
    )
    .unwrap_err();
    assert!(err.to_string().starts_with("csv:"), "{err}");
    assert!(
        err.to_string()
            .contains("neither a `street` nor an `address`"),
        "{err}"
    );
}

#[test]
fn empty_input_writes_nothing() {
    let ti = index();
    let (summary, body) = run_csv(&Args::for_index(ti.path()), HEADER);
    assert_eq!(summary.rows, 0);
    assert!(body.is_empty());
}

#[test]
fn file_in_file_out() {
    let ti = index();
    let work = tempfile::tempdir().unwrap();
    let input = work.path().join("in.csv");
    let output = work.path().join("out.csv");
    std::fs::write(&input, format!("{HEADER}1,Grand-Place,1,1000\n")).unwrap();
    let mut args = Args::for_index(ti.path());
    args.input = Some(input);
    args.output = Some(output.clone());
    assert_eq!(run(&args).unwrap().rows, 1);
    let (h, rows) = parse_csv(&std::fs::read_to_string(&output).unwrap());
    assert_eq!(col(&h, &rows[0], "confidence"), "exact");
}

#[test]
fn missing_input_file_is_an_error() {
    let ti = index();
    let mut args = Args::for_index(ti.path());
    args.input = Some(ti.path().join("nope.csv"));
    let err = run(&args).unwrap_err();
    assert!(format!("{err:#}").contains("reading input"));
}

#[test]
fn stale_index_is_rejected() {
    let ti = index();
    let stale = format!(
        r#"{{"schema_version": {}, "tokenizer_name": "crab", "built_at_unix_secs": 0}}"#,
        SCHEMA_VERSION + 1
    );
    std::fs::write(ti.path().join(SIDECAR_FILENAME), stale).unwrap();
    let err = run_with_io(&Args::for_index(ti.path()), HEADER.as_bytes(), Vec::new()).unwrap_err();
    assert!(err.to_string().contains("schema_version"), "{err}");
}

#[test]
fn box_column_and_slash_numbers_pick_the_unit() {
    let mut addrs = fixtures::batch_small();
    for (oid, b) in [("u1", "1"), ("u2", "2")] {
        addrs.push(
            AddressBuilder::new(oid, fixtures::LOUISE_OID, "10")
                .street(Some("Avenue Louise"), Some("Louizalaan"))
                .postal("1050")
                .box_no(b)
                .build(),
        );
    }
    let ti = TestIndex::build_with_mid_street(&addrs);
    let (_, body) = run_csv(
        &Args::for_index(ti.path()),
        "id,street,house_number,postal_code,box_number
1,Avenue Louise,10,1050,2
2,Avenue Louise,10/1,1050,
3,Avenue Louise,10,1050,
4,Avenue Louise,10A,1050,
",
    );
    let (h, rows) = parse_csv(&body);
    assert_eq!(rows.len(), 4);
    let best = |i: usize| col(&h, &rows[i], "best_id").to_string();
    assert!(best(0).contains("/u2/"), "{}", best(0));
    assert!(best(1).contains("/u1/"), "{}", best(1));
    assert!(
        best(2).contains("/100/"),
        "no box → base record: {}",
        best(2)
    );
    for r in &rows[..3] {
        assert_eq!(col(&h, r, "confidence"), "exact");
        assert_eq!(col(&h, r, "matched_house_number"), "10");
    }
    // 10A doesn't exist → nearest number on the street: n°10, delta 0.
    assert_eq!(col(&h, &rows[3], "confidence"), "nearest_same_side");
    assert_eq!(col(&h, &rows[3], "matched_house_number"), "10");
    assert_eq!(col(&h, &rows[3], "house_number_delta"), "0");
    // The input is echoed as given.
    assert_eq!(col(&h, &rows[1], "house_number"), "10/1");
}

// --- subprocess smoke tests -------------------------------------------------

#[test]
fn binary_stdin_to_stdout() {
    let ti = index();
    let mut child = Command::new(batch_bin())
        .arg("--index")
        .arg(ti.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn crabfinder-batch");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(format!("{HEADER}1,Avenue Louise,10,1050\n").as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let (h, rows) = parse_csv(&String::from_utf8(out.stdout).unwrap());
    assert_eq!(col(&h, &rows[0], "confidence"), "exact");
}

#[test]
fn binary_fails_on_stale_index() {
    let ti = index();
    std::fs::write(
        ti.path().join(SIDECAR_FILENAME),
        format!(
            r#"{{"schema_version": {}, "tokenizer_name": "crab", "built_at_unix_secs": 0}}"#,
            SCHEMA_VERSION + 1
        ),
    )
    .unwrap();
    let out = Command::new(batch_bin())
        .args(["--index"])
        .arg(ti.path())
        .args(["--input", "-", "--output", "-"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("schema_version"));
}

// --- Phase 6: free text, GeoJSON, lang ---------------------------------------

#[test]
fn free_text_input_and_label_language() {
    let ti = index();
    let mut args = Args::for_index(ti.path());
    args.lang = Some(crabfinder_core::Language::Nl);
    let (summary, body) = run_csv(
        &args,
        "id,address\n1,\"Grote Markt 1, 1000 Brussel\"\n2,\"Avenue Louise 20 1050\"\n",
    );
    assert_eq!(summary.rows, 2);
    let (h, rows) = parse_csv(&body);
    assert_eq!(col(&h, &rows[0], "confidence"), "exact");
    assert_eq!(col(&h, &rows[0], "label"), "Grote Markt 1, 1000 Brussel");
    assert_eq!(col(&h, &rows[1], "matched_house_number"), "20");
}

#[test]
fn geojson_output() {
    let ti = index();
    let mut args = Args::for_index(ti.path());
    args.format = crabfinder_service::batch::io::OutputFormat::GeoJson;
    let (_, body) = run_csv(&args, &format!("{HEADER}1,Avenue Louise,10,1050\n"));
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["features"][0]["properties"]["confidence"], "exact");
    assert_eq!(v["features"][0]["geometry"]["type"], "Point");
    let (_, body) = run_csv(&args, HEADER);
    assert_eq!(body, r#"{"type":"FeatureCollection","features":[]}"#);
}

#[test]
fn format_and_lang_flags_parse() {
    use clap::Parser;
    let a = Args::try_parse_from([
        "crabfinder-batch",
        "--index",
        "x",
        "--format",
        "GeoJSON",
        "--lang",
        "DE",
    ])
    .unwrap();
    assert_eq!(
        a.format,
        crabfinder_service::batch::io::OutputFormat::GeoJson
    );
    assert_eq!(a.lang, Some(crabfinder_core::Language::De));
    let e =
        Args::try_parse_from(["crabfinder-batch", "--index", "x", "--format", "xml"]).unwrap_err();
    assert!(e.to_string().contains("unknown format 'xml'"), "{e}");
    let e = Args::try_parse_from(["crabfinder-batch", "--index", "x", "--lang", "en"]).unwrap_err();
    assert!(e.to_string().contains("unknown lang 'en'"), "{e}");
}
