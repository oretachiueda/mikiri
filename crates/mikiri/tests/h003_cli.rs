#[path = "common/h003.rs"]
mod common;
use std::{fs, process::Command};

#[test]
fn cli_real_format_and_fail_closed() {
    let root = std::env::temp_dir().join(format!("mikiri-h003-cli-{}", std::process::id()));
    fs::create_dir(&root).unwrap();
    let run = root.join("results/run");
    for ticker in 0..3 {
        fs::write(
            root.join(format!("{ticker}.json")),
            serde_json::to_vec(&common::fixture(ticker)).unwrap(),
        )
        .unwrap();
    }
    let invoke = |out: &str| {
        Command::new(env!("CARGO_BIN_EXE_h003-compute"))
            .current_dir(&root)
            .args([
                "--spy", "0.json", "--spxl", "1.json", "--bil", "2.json", "--out", out,
            ])
            .output()
            .unwrap()
    };
    let result = invoke("results/run");
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let stdout = String::from_utf8(result.stdout).unwrap();
    assert_eq!(stdout.lines().count(), 1);
    assert_eq!(stdout.matches("sha256=").count(), 3);
    assert!(!stdout.contains("gate") && !stdout.contains("psr"));
    assert!(run.join("days.parquet").exists());
    assert!(!invoke("results/run").status.success());
    assert!(!invoke("outside").status.success());
    assert!(!root.join("outside").exists());
    for (name, value) in [
        ("close", serde_json::json!(0.0)),
        ("close", serde_json::Value::Null),
        ("splitFactor", serde_json::json!(0.0)),
        ("divCash", serde_json::json!(-1.0)),
    ] {
        let mut rows = common::fixture(1);
        rows[600][name] = value;
        fs::write(root.join("1.json"), serde_json::to_vec(&rows).unwrap()).unwrap();
        assert!(!invoke("results/fail").status.success());
        assert!(!root.join("results/fail").exists());
    }
    fs::remove_dir_all(root).unwrap();
}
