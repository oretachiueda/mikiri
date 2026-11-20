use std::{
    fs,
    path::PathBuf,
    process::{Command, Output},
    time::{SystemTime, UNIX_EPOCH},
};

struct Fixture {
    dir: PathBuf,
    bars: PathBuf,
    log: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "mikiri-recorder-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        fs::create_dir(&dir).expect("create directory");
        Self {
            bars: dir.join("bars.json"),
            log: dir.join("log.jsonl"),
            dir,
        }
    }
    fn bars(&self, rows: &[(&str, f64)]) {
        let rows: Vec<_> = rows.iter().map(|(date, close)| serde_json::json!({"date": format!("{date}T00:00:00Z"), "close": close, "divCash": 0, "splitFactor": 1})).collect();
        fs::write(&self.bars, serde_json::to_vec(&rows).expect("serialize")).expect("write bars");
    }
    fn record(&self, ticker: &str, today: &str) -> Output {
        Command::new(env!("CARGO_BIN_EXE_paper-recorder"))
            .args(["record", "--bars"])
            .arg(&self.bars)
            .arg("--log")
            .arg(&self.log)
            .args(["--ticker", ticker, "--today", today])
            .output()
            .expect("run record")
    }
    fn verify(&self) -> Output {
        Command::new(env!("CARGO_BIN_EXE_paper-recorder"))
            .arg("verify")
            .arg("--log")
            .arg(&self.log)
            .output()
            .expect("run verify")
    }
    fn alert(&self, ticker: &str, today: &str, code: i32) {
        let before = fs::read(&self.log).expect("read log");
        let result = self.record(ticker, today);
        assert_eq!(
            result.status.code(),
            Some(code),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(fs::read(&self.log).expect("read log"), before);
        assert!(result.stdout.is_empty());
        let stderr = String::from_utf8_lossy(&result.stderr).to_lowercase();
        for forbidden in ["value", "pnl", "return", "cumulative", "sharpe"] {
            assert!(!stderr.contains(forbidden));
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn records_and_rejects_alerts_without_writing() {
    let f = Fixture::new();
    f.bars(&[
        ("2024-01-01", 10.0),
        ("2024-01-02", 11.0),
        ("2024-01-03", 12.0),
    ]);
    let first = f.record("TEST", "2024-01-03");
    assert!(first.status.success());
    assert!(String::from_utf8_lossy(&first.stdout).contains("appended=3"));
    let original = fs::read(&f.log).expect("log");
    let repeat = f.record("TEST", "2024-01-03");
    assert!(repeat.status.success());
    assert!(String::from_utf8_lossy(&repeat.stdout).contains("appended=0"));
    assert_eq!(fs::read(&f.log).expect("log"), original);
    f.bars(&[
        ("2024-01-01", 10.0),
        ("2024-01-02", 11.0),
        ("2024-01-03", 12.0),
        ("2024-01-04", 13.0),
    ]);
    let append = f.record("TEST", "2024-01-04");
    assert!(append.status.success());
    assert!(String::from_utf8_lossy(&append.stdout).contains("appended=1"));
    let verify = f.verify();
    assert!(verify.status.success());
    assert!(String::from_utf8_lossy(&verify.stdout).contains("lines=4"));
    let log = fs::read_to_string(&f.log).expect("log");
    for line in log.lines() {
        let entry: serde_json::Value = serde_json::from_str(line).expect("JSON");
        let keys = entry
            .as_object()
            .expect("object")
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>();
        assert_eq!(
            keys,
            [
                "bars_sha256",
                "close",
                "date",
                "hash",
                "prev",
                "qty",
                "seq",
                "ticker"
            ]
        );
    }
    for output in [&first, &repeat, &append, &verify] {
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
        .to_lowercase();
        for forbidden in ["value", "pnl", "return", "cumulative", "sharpe"] {
            assert!(!text.contains(forbidden));
        }
    }
    f.bars(&[("2024-01-02", 99.0), ("2024-01-04", 13.0)]);
    f.alert("TEST", "2024-01-04", 2);
    f.bars(&[("2024-01-04", 13.0), ("2024-01-09", 14.0)]);
    f.alert("TEST", "2024-01-09", 2);
    f.bars(&[("2024-01-04", 13.0)]);
    f.alert("TEST", "2024-01-09", 2);
    f.alert("OTHER", "2024-01-04", 2);
    let mut tampered = log.clone().into_bytes();
    let pos = tampered.iter().position(|b| *b == b'1').expect("digit");
    tampered[pos] = b'2';
    fs::write(&f.log, tampered).expect("tamper");
    assert_eq!(f.verify().status.code(), Some(3));
    f.alert("TEST", "2024-01-04", 3);
    let lines: Vec<_> = log.lines().collect();
    fs::write(
        &f.log,
        format!("{}\n{}\n{}\n", lines[0], lines[2], lines[3]),
    )
    .expect("delete line");
    assert_eq!(f.verify().status.code(), Some(3));
    f.alert("TEST", "2024-01-04", 3);
}

#[test]
fn missing_log_is_an_error_for_verify_but_record_creates_it() {
    let f = Fixture::new();
    let result = f.verify();
    assert_eq!(result.status.code(), Some(1));
    assert!(!f.log.exists());
    f.bars(&[("2024-01-02", 10.0)]);
    assert!(f.record("TEST", "2024-01-02").status.success());
    assert!(f.verify().status.success());
}

#[test]
fn fixed_line_hash_pins_number_format() {
    let f = Fixture::new();
    fs::write(&f.bars, b"[{\"date\":\"2024-01-02T00:00:00Z\",\"close\":1.2300000000000002,\"divCash\":0,\"splitFactor\":1}]").expect("fixture");
    assert!(f.record("TEST", "2024-01-02").status.success());
    let entry: serde_json::Value =
        serde_json::from_slice(&fs::read(&f.log).expect("log")).expect("JSON");
    assert_eq!(
        entry["hash"],
        "a90014459e7eaeec8c1499042bbe968668fb7b660c785040ea9d674fd3a8b3bc"
    );
}

#[test]
fn gaps_follow_exchange_sessions() {
    let f = Fixture::new();
    f.bars(&[("2025-01-17", 10.0), ("2025-01-21", 11.0)]);
    assert!(f.record("TEST", "2025-01-21").status.success());
    f.bars(&[("2025-01-23", 12.0)]);
    let result = f.record("TEST", "2025-01-23");
    assert_eq!(result.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&result.stderr).contains("bar gap"));
}
