use std::{fs, io::Write, path::PathBuf, process::Command};

#[test]
fn failed_clock_prints_both_checks_before_refusal() {
    let id = format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let root = std::env::temp_dir().join(format!("h006-clock-{id}"));
    fs::create_dir(&root).unwrap();
    for year in 2007..=2024 {
        let path = root.join(format!("HISTDATA_COM_ASCII_EURUSD_M1_{year}.zip"));
        let mut zip = zip::ZipWriter::new(fs::File::create(path).unwrap());
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        zip.start_file(format!("DAT_ASCII_EURUSD_M1_{year}.csv"), options)
            .unwrap();
        if year == 2007 {
            zip.write_all(b"20070107 170000;1;1;1;1;0\n20070108 082900;1;1;1;1;0\n20070108 083000;1.1;1.1;1.1;1.1;0\n20070108 083100;1.101;1.101;1.101;1.101;0\n20070701 170000;1;1;1;1;0\n20070702 082900;1;1;1;1;0\n20070702 083000;1.001;1.001;1.001;1.001;0\n20070702 083100;1.1;1.1;1.1;1.1;0\n").unwrap();
        }
        zip.finish().unwrap();
    }
    let output = PathBuf::from(format!("results/h006-clock-fail-{id}"));
    assert!(!output.exists());
    let result = Command::new(env!("CARGO_BIN_EXE_h006-compute"))
        .args([
            "--histdata-dir",
            root.to_str().unwrap(),
            "--spy",
            "unused.json",
            "--stage",
            "dev",
            "--out",
            output.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(!result.status.success());
    let stdout = String::from_utf8(result.stdout).unwrap();
    let line = stdout
        .lines()
        .find_map(|s| s.strip_prefix("clock_check "))
        .unwrap();
    let clock: serde_json::Value = serde_json::from_str(line).unwrap();
    assert_eq!(clock.as_array().unwrap().len(), 2);
    assert_eq!(clock[0]["top"][0][0], 509);
    assert_eq!(clock[1]["top"][0][0], 510);
    assert!(
        String::from_utf8(result.stderr)
            .unwrap()
            .contains("clock check failed")
    );
    assert!(!output.exists());
    fs::remove_dir_all(root).unwrap();
}
