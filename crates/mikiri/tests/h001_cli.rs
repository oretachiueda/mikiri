use arrow_array::{BooleanArray, Date32Array, Float64Array, StringArray};
use arrow_schema::DataType;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{fs, path::PathBuf, process::Command};
use time::{Date, Month, Weekday};

struct Workspace(PathBuf);
impl Workspace {
    fn new(name: &str) -> Self {
        let directory = std::env::temp_dir().join(format!(
            "mikiri-h001-cli-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        fs::create_dir(&directory).expect("workspace");
        Self(directory)
    }
    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_h001-compute"));
        command.current_dir(&self.0);
        command
    }
}
impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn synthetic_json() -> Vec<Value> {
    let mut day = Date::from_calendar_date(2005, Month::November, 1).expect("start");
    let end = Date::from_calendar_date(2026, Month::October, 30).expect("end");
    let mut bars = Vec::new();
    while day <= end {
        if !matches!(day.weekday(), Weekday::Saturday | Weekday::Sunday) {
            bars.push(json!({
                "date": format!("{day}T00:00:00Z"),
                "close": 100.0,
                "adjClose": 3.0,
                "divCash": f64::from(day.ordinal()) / 100.0,
                "splitFactor": 1.0
            }));
        }
        day = day.next_day().expect("next day");
    }
    bars
}

#[test]
fn cli_writes_readable_outputs_and_hashes_with_october_30_final_bar() {
    let workspace = Workspace::new("success");
    let bars = synthetic_json();
    fs::write(
        workspace.0.join("bars.json"),
        serde_json::to_vec(&bars).expect("JSON"),
    )
    .expect("write bars");
    let output = workspace
        .command()
        .args(["--bars", "bars.json", "--out", "results/run"])
        .output()
        .expect("run CLI");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout).expect("UTF-8 stdout");
    assert_eq!(stdout.lines().count(), 1);
    let mut expected = Vec::new();
    for name in ["bars.parquet", "cycles.parquet", "summary.json"] {
        let path = format!("results/run/{name}");
        let hash = Sha256::digest(fs::read(workspace.0.join(&path)).expect("output bytes"));
        expected.push(format!("{path} sha256={hash:x}"));
    }
    assert_eq!(
        stdout,
        format!("{} main_n=250 recent_n=120\n", expected.join(" "))
    );
    for forbidden in ["gate", "psr", "mean"] {
        assert!(!stdout.contains(forbidden));
    }
    let summary: Value = serde_json::from_slice(
        &fs::read(workspace.0.join("results/run/summary.json")).expect("summary"),
    )
    .expect("summary JSON");
    assert_eq!(summary["main_n"], 250);
    assert_eq!(summary["recent_n"], 120);
    assert_eq!(summary["reference_n"], 1);
    for key in [
        "mean_d_main",
        "mean_d_recent",
        "psr_main",
        "mean_d_reference",
    ] {
        assert!(summary[key].as_f64().expect("numeric summary").is_finite());
    }
    assert_eq!(
        summary["gate_a"],
        summary["mean_d_main"].as_f64().expect("main mean") > 0.0
    );
    assert_eq!(
        summary["gate_b"],
        summary["psr_main"].as_f64().expect("PSR") >= 0.95
    );
    assert_eq!(
        summary["gate_c"],
        summary["mean_d_recent"].as_f64().expect("recent mean") > 0.0
    );

    let reader = ParquetRecordBatchReaderBuilder::try_new(
        fs::File::open(workspace.0.join("results/run/bars.parquet")).expect("bars parquet"),
    )
    .expect("bars reader");
    let fields: Vec<_> = reader
        .schema()
        .fields()
        .iter()
        .map(|f| (f.name().clone(), f.data_type().clone()))
        .collect();
    assert_eq!(
        fields,
        vec![
            ("date".into(), DataType::Date32),
            ("close".into(), DataType::Float64),
            ("div_cash".into(), DataType::Float64),
            ("split_factor".into(), DataType::Float64),
        ]
    );
    let row_count: usize = reader
        .build()
        .expect("bars batches")
        .map(|b| b.expect("bar batch").num_rows())
        .sum();
    assert_eq!(row_count, bars.len());
    let reader = ParquetRecordBatchReaderBuilder::try_new(
        fs::File::open(workspace.0.join("results/run/cycles.parquet")).expect("cycles parquet"),
    )
    .expect("cycles reader")
    .build()
    .expect("cycles batches");
    let (mut main, mut recent, mut reference, mut last_main, mut main_sum) = (0, 0, 0, None, 0.0);
    for batch in reader {
        let batch = batch.expect("cycle batch");
        let periods = batch
            .column_by_name("period")
            .expect("period")
            .as_any()
            .downcast_ref::<StringArray>()
            .expect("string period");
        let recents = batch
            .column_by_name("recent")
            .expect("recent")
            .as_any()
            .downcast_ref::<BooleanArray>()
            .expect("boolean recent");
        let dates = batch
            .column_by_name("plus1")
            .expect("plus1")
            .as_any()
            .downcast_ref::<Date32Array>()
            .expect("date plus1");
        let ds = batch
            .column_by_name("d")
            .expect("d")
            .as_any()
            .downcast_ref::<Float64Array>()
            .expect("float d");
        for i in 0..batch.num_rows() {
            match periods.value(i) {
                "main" => {
                    main += 1;
                    last_main = Some(dates.value(i));
                    main_sum += ds.value(i);
                }
                "reference" => reference += 1,
                other => panic!("unexpected period {other}"),
            }
            recent += usize::from(recents.value(i));
        }
    }
    assert_eq!((main, recent, reference), (250, 120, 1));
    let epoch = Date::from_calendar_date(1970, Month::January, 1).expect("epoch");
    let october = Date::from_calendar_date(2026, Month::October, 1).expect("October");
    assert_eq!(last_main, Some((october - epoch).whole_days() as i32));
    assert_eq!(
        (main_sum / 250.0).to_bits(),
        summary["mean_d_main"]
            .as_f64()
            .expect("main mean")
            .to_bits()
    );
}

#[test]
fn cli_rejects_invalid_coverage_paths_and_duplicate_arguments_without_outputs() {
    let workspace = Workspace::new("errors");
    fs::write(
        workspace.0.join("bars.json"),
        br#"[{"date":"2026-10-30T00:00:00Z","close":100,"divCash":0,"splitFactor":1}]"#,
    )
    .expect("write short bars");
    let output = workspace
        .command()
        .args(["--bars", "bars.json", "--out", "results/invalid"])
        .output()
        .expect("coverage CLI");
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("start on or before"));
    assert!(!workspace.0.join("results").exists());
    for out in [
        "/tmp/h001-unwanted",
        "other/run",
        "results/../outside",
        "results",
        "../results/run",
        "results/line\nbreak",
        "results/tab\tname",
        "results/carriage\rreturn",
    ] {
        let output = workspace
            .command()
            .args(["--bars", "bars.json", "--out", out])
            .output()
            .expect("path CLI");
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8_lossy(&output.stderr).contains("under results/"));
    }
    let output = workspace
        .command()
        .args([
            "--bars",
            "bars.json",
            "--bars",
            "bars.json",
            "--out",
            "results/duplicate",
        ])
        .output()
        .expect("duplicate CLI");
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("duplicate argument"));
    assert!(!workspace.0.join("results").exists());
}
