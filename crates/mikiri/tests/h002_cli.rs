use arrow_array::{
    ArrayRef, Date32Array, Float64Array, LargeStringArray, RecordBatch, StringArray,
};
use parquet::{
    arrow::{ArrowWriter, arrow_reader::ParquetRecordBatchReaderBuilder},
    basic::Compression,
    file::properties::WriterProperties,
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    path::PathBuf,
    process::{Command, Output},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use time::{Date, Month};

const MAIN_START: i32 = 1994 * 12;
const HOLDOUT_START: i32 = 2024 * 12 + 10;
const WRONG_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";

struct Workspace(PathBuf);

impl Workspace {
    fn new() -> Self {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let directory =
            std::env::temp_dir().join(format!("mikiri-h002-{}-{timestamp}", std::process::id()));
        fs::create_dir(&directory).expect("workspace");
        Self(directory)
    }

    fn input(&self, rows: &[Row], large: bool) -> String {
        self.input_with_compression(rows, large, Compression::UNCOMPRESSED)
    }

    fn input_with_compression(
        &self,
        rows: &[Row],
        large: bool,
        compression: Compression,
    ) -> String {
        let strings = |values: Vec<&str>| -> ArrayRef {
            if large {
                Arc::new(LargeStringArray::from(values))
            } else {
                Arc::new(StringArray::from(values))
            }
        };
        let batch = RecordBatch::try_from_iter([
            (
                "signalname",
                strings(rows.iter().map(|r| r.signal).collect()),
            ),
            ("port", strings(rows.iter().map(|r| r.port).collect())),
            (
                "date",
                Arc::new(Date32Array::from(
                    rows.iter().map(|r| r.day).collect::<Vec<_>>(),
                )),
            ),
            (
                "ret",
                Arc::new(Float64Array::from(
                    rows.iter().map(|r| r.ret).collect::<Vec<_>>(),
                )),
            ),
        ])
        .expect("batch");
        let path = self.0.join("ports.parquet");
        let properties = WriterProperties::builder()
            .set_compression(compression)
            .build();
        let mut writer = ArrowWriter::try_new(
            File::create(&path).expect("create"),
            batch.schema(),
            Some(properties),
        )
        .expect("writer");
        writer.write(&batch).expect("write");
        writer.close().expect("close");
        self.hash("ports.parquet")
    }

    fn bytes(&self, path: &str) -> Vec<u8> {
        fs::read(self.0.join(path)).expect("read artifact")
    }

    fn hash(&self, path: &str) -> String {
        format!("{:x}", Sha256::digest(self.bytes(path)))
    }

    fn command(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_h002-compute"))
            .current_dir(&self.0)
            .args(args)
            .output()
            .expect("execute H002")
    }

    fn run(&self, hash: &str, out: &str) -> Output {
        self.command(&["--ports", "ports.parquet", "--sha256", hash, "--out", out])
    }

    fn summary(&self, out: &str) -> Value {
        serde_json::from_slice(&self.bytes(&format!("{out}/summary.json"))).expect("summary")
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[derive(Clone)]
struct Row {
    signal: &'static str,
    port: &'static str,
    day: i32,
    month: i32,
    ret: f64,
}

fn rows(p05: impl Fn(i32) -> f64) -> Vec<Row> {
    let epoch = Date::from_calendar_date(1970, Month::January, 1)
        .expect("epoch")
        .to_julian_day();
    (1971 * 12 + 8..=2025 * 12 + 11)
        .flat_map(|month| {
            let calendar_month =
                Month::try_from(u8::try_from(month % 12 + 1).expect("month number"))
                    .expect("month");
            let day = Date::from_calendar_date(month / 12, calendar_month, 28)
                .expect("date")
                .to_julian_day()
                - epoch;
            let top = p05(month);
            ["01", "02", "03", "04", "05", "LS"]
                .into_iter()
                .zip([1.0, 2.0, 3.0, 4.0, top, top - 1.0])
                .map(move |(port, ret)| Row {
                    signal: "AnnouncementReturn",
                    port,
                    day,
                    month,
                    ret,
                })
        })
        .collect()
}

fn varying(top: f64, month: i32) -> f64 {
    top + if month % 2 == 0 { 0.125 } else { -0.125 }
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
}

fn assert_failure(output: &Output, message: &str) {
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert_eq!(String::from_utf8_lossy(&output.stderr).trim_end(), message);
}

fn check_invariance(original: &[Row], altered: &[Row], large: bool, compression: Compression) {
    let work = Workspace::new();
    let a = work.input(original, false);
    assert_success(&work.run(&a, "results/a"));
    let b = work.input_with_compression(altered, large, compression);
    assert_ne!(a, b);
    assert_success(&work.run(&b, "results/b"));
    assert_eq!(
        work.bytes("results/a/months.parquet"),
        work.bytes("results/b/months.parquet")
    );
    let without_hash = |run: &str, expected: &str| {
        let path = format!("{run}/summary.json");
        assert_eq!(work.summary(run)["input_sha256"], expected);
        let json = String::from_utf8(work.bytes(&path)).expect("summary text");
        json.lines()
            .filter(|line| !line.starts_with("  \"input_sha256\":"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert_eq!(without_hash("results/a", &a), without_hash("results/b", &b));
}

#[test]
fn holdout_changes_leave_outputs_invariant_except_input_hash() {
    let original = rows(|month| varying(6.0, month));
    let mut altered = original.clone();
    for row in altered.iter_mut().filter(|row| row.month >= HOLDOUT_START) {
        row.ret = if row.port == "LS" { 999.0 } else { -50.0 };
    }
    check_invariance(&original, &altered, false, Compression::UNCOMPRESSED);
}

#[test]
fn zstd_and_uncompressed_inputs_give_identical_results_except_input_hash() {
    let input = rows(|month| varying(6.0, month));
    check_invariance(&input, &input, false, Compression::ZSTD(Default::default()));
}

#[test]
fn rejects_hash_mismatch_without_writing_output() {
    let work = Workspace::new();
    work.input(&rows(|month| varying(6.0, month)), false);
    assert_failure(
        &work.run(WRONG_HASH, "results/run"),
        "input SHA-256 mismatch",
    );
    assert!(!work.0.join("results").exists());
}

#[test]
fn rejects_main_ls_mismatch_without_writing_output() {
    let work = Workspace::new();
    let mut input = rows(|month| varying(6.0, month));
    input
        .iter_mut()
        .find(|row| row.month == MAIN_START && row.port == "LS")
        .expect("LS")
        .ret = 0.0;
    let hash = work.input(&input, false);
    assert_failure(
        &work.run(&hash, "results/run"),
        "LS does not match 05 minus 01",
    );
    assert!(!work.0.join("results").exists());
}

#[test]
fn rejects_missing_main_month_or_port_without_writing_output() {
    let work = Workspace::new();
    for (whole_month, message) in [
        (true, "missing main month"),
        (false, "missing main portfolio"),
    ] {
        let mut input = rows(|month| varying(6.0, month));
        input.retain(|row| row.month != MAIN_START || (!whole_month && row.port != "02"));
        let hash = work.input(&input, false);
        assert_failure(&work.run(&hash, "results/run"), message);
        assert!(!work.0.join("results").exists());
    }
}

#[test]
fn excludes_only_the_incomplete_reference_month() {
    let work = Workspace::new();
    let mut input = rows(|month| varying(6.0, month));
    input.retain(|row| row.month != MAIN_START - 1 || !matches!(row.port, "02" | "04"));
    let hash = work.input(&input, false);
    assert_success(&work.run(&hash, "results/run"));
    let summary = work.summary("results/run");
    assert_eq!(summary["reference_n"], 267);
    assert_eq!(summary["reference_excluded"], 1);
}

#[test]
fn complete_history_writes_expected_counts_and_one_line_of_hashes() {
    let work = Workspace::new();
    let hash = work.input(&rows(|month| varying(6.0, month)), false);
    let output = work.run(&hash, "results/run");
    assert_success(&output);
    let stdout = String::from_utf8(output.stdout).expect("stdout");
    assert_eq!(
        stdout,
        format!(
            "results/run/months.parquet sha256={} results/run/summary.json sha256={} main_n=370 recent_n=120\n",
            work.hash("results/run/months.parquet"),
            work.hash("results/run/summary.json")
        )
    );
    assert!(
        ["gate", "psr", "mean"]
            .iter()
            .all(|label| !stdout.contains(label))
    );
    let summary = work.summary("results/run");
    for (field, expected) in [
        ("main_n", 370),
        ("recent_n", 120),
        ("reference_n", 268),
        ("reference_excluded", 0),
    ] {
        assert_eq!(summary[field], expected);
    }
    let file = File::open(work.0.join("results/run/months.parquet")).expect("output");
    let mut reader = ParquetRecordBatchReaderBuilder::try_new(file)
        .expect("builder")
        .build()
        .expect("reader");
    let batch = reader.next().expect("first batch").expect("batch");
    assert_eq!(batch.num_rows(), 638);
    let dates = batch
        .column_by_name("month_end")
        .expect("month_end")
        .as_any()
        .downcast_ref::<Date32Array>()
        .expect("Date32");
    let input = rows(|month| varying(6.0, month));
    assert_eq!(dates.value(0), input[0].day);
    assert_eq!(
        dates.value(637),
        input
            .iter()
            .rev()
            .find(|row| row.month < HOLDOUT_START)
            .expect("last used date")
            .day
    );
}

#[test]
fn hand_calculated_excess_and_annual_gate() {
    let work = Workspace::new();
    for (out, top, e, annual, gate) in [
        ("results/high", 6.0, 2.8, 33.6, true),
        ("results/low", 3.5, 0.8, 9.6, false),
    ] {
        let hash = work.input(&rows(|month| varying(top, month)), false);
        assert_success(&work.run(&hash, out));
        let summary = work.summary(out);
        for (key, expected) in [
            ("mean_e_main", e),
            ("mean_e_recent", e),
            ("annual_e_recent", annual),
        ] {
            assert!((summary[key].as_f64().expect("number") - expected).abs() <= 1e-12);
        }
        assert_eq!(summary["gate_d"], gate);
    }
}

#[test]
fn rejects_constant_main_excess_return_without_writing_output() {
    let work = Workspace::new();
    let hash = work.input(&rows(|_| 6.0), false);
    assert_failure(
        &work.run(&hash, "results/run"),
        "PSR variance must be positive and finite",
    );
    assert!(!work.0.join("results").exists());
}

#[test]
fn gate_b_rejects_psr_between_point_95_and_point_975() {
    let work = Workspace::new();
    let hash = work.input(
        &rows(|month| {
            let e = 0.094 + if month % 2 == 0 { 1.0 } else { -1.0 };
            (e + 2.0) * 1.25
        }),
        false,
    );
    assert_success(&work.run(&hash, "results/run"));
    let summary = work.summary("results/run");
    let psr = summary["psr_main"].as_f64().expect("PSR");
    assert!(psr > 0.95 && psr < 0.975);
    assert_eq!(summary["gate_b"], false);
}

#[test]
fn ignores_other_signals_before_validating_values() {
    let input = rows(|month| varying(6.0, month));
    let mut altered = input.clone();
    altered.push(Row {
        signal: "Other",
        port: "invalid",
        day: i32::MAX,
        month: MAIN_START,
        ret: f64::INFINITY,
    });
    check_invariance(&input, &altered, false, Compression::UNCOMPRESSED);
}

#[test]
fn rejects_duplicate_port_even_on_different_dates_in_the_month() {
    let work = Workspace::new();
    let mut input = rows(|month| varying(6.0, month));
    let mut duplicate = input[0].clone();
    duplicate.day -= 1;
    input.push(duplicate);
    let hash = work.input(&input, false);
    assert_failure(
        &work.run(&hash, "results/run"),
        "duplicate month and portfolio",
    );
    assert!(!work.0.join("results").exists());
}

#[test]
fn utf8_and_large_utf8_give_identical_results_except_input_hash() {
    let input = rows(|month| varying(6.0, month));
    check_invariance(&input, &input, true, Compression::UNCOMPRESSED);
}

#[test]
fn cli_rejects_invalid_paths_arguments_and_existing_output() {
    let work = Workspace::new();
    let hash = work.input(&rows(|month| varying(6.0, month)), false);
    for out in ["out", "results", "results/../out", "results/bad\npath"] {
        assert_failure(&work.run(&hash, out), "output path must be under results/");
    }
    for (args, error) in [
        (vec![], "missing --out"),
        (vec!["--out", "results/run"], "missing --ports"),
        (
            vec!["--out", "results/run", "--ports", "ports.parquet"],
            "missing --sha256",
        ),
        (vec!["--ports"], "missing argument value"),
        (vec!["--ports", "a", "--ports", "b"], "duplicate argument"),
        (vec!["--unknown"], "expected --ports, --sha256 or --out"),
    ] {
        assert_failure(&work.command(&args), error);
    }
    assert_failure(
        &work.run("bad", "results/run"),
        "--sha256 must be 64 lowercase hex characters",
    );
    assert!(!work.0.join("results").exists());
    assert_success(&work.run(&hash, "results/run"));
    let original = work.bytes("results/run/summary.json");
    assert_failure(
        &work.run(&hash, "results/run"),
        "output directory already exists",
    );
    assert_eq!(work.bytes("results/run/summary.json"), original);
}
