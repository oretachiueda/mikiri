//! H006 intraday calculations.

/// SplitMix64 with the preregistered seed.
pub fn splitmix64(n: u64) -> u64 {
    let mut v = (n ^ 20_261_115).wrapping_add(0x9e3779b97f4a7c15);
    v = (v ^ (v >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    v = (v ^ (v >> 27)).wrapping_mul(0x94d049bb133111eb);
    v ^ (v >> 31)
}

#[cfg(test)]
mod tests {
    use super::splitmix64;

    #[test]
    fn splitmix_reference_values() {
        for (n, expected) in [
            (0, 13866863570256181660),
            (1, 8826110057919892546),
            (2, 17342297256639978221),
            (4294967296, 5572763319858028600),
            (4290672337505, 11747482445946130720),
        ] {
            assert_eq!(splitmix64(n), expected);
        }
    }
}

pub mod daily;
pub mod input;
pub mod random;
pub mod reports;
pub mod segments;

use crate::h001::{checked_output_path, publish_outputs};
use arrow_array::{Array, RecordBatch, StringArray};
use arrow_schema::{Field, Schema};
use parquet::arrow::ArrowWriter;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    error::Error,
    fs::{self, File},
    io::{self, Write},
    path::Path,
    sync::Arc,
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;
const OUTPUTS: [&str; 5] = [
    "accounts.parquet",
    "days.parquet",
    "random.parquet",
    "removed_bars.parquet",
    "summary.json",
];

pub struct InputPaths<'a> {
    pub histdata_dir: &'a Path,
    pub spy: &'a Path,
}
fn digest(path: &Path) -> Result<String> {
    let mut hash = Sha256::new();
    io::copy(&mut File::open(path)?, &mut hash)?;
    Ok(format!("{:x}", hash.finalize()))
}
fn all_hashes(inputs: &InputPaths<'_>) -> Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    for year in 2007..=2025 {
        let name = format!("HISTDATA_COM_ASCII_EURUSD_M1_{year}.zip");
        out.insert(name.clone(), digest(&inputs.histdata_dir.join(name))?);
    }
    for month in 1..=10 {
        let name = format!("HISTDATA_COM_ASCII_EURUSD_M1_2026{month:02}.zip");
        out.insert(name.clone(), digest(&inputs.histdata_dir.join(name))?);
    }
    out.insert("SPY.json".into(), digest(inputs.spy)?);
    Ok(out)
}
fn equivalent(a: &Value, b: &Value, key: &str) -> bool {
    match (a, b) {
        (Value::Object(a), Value::Object(b)) => {
            a.len() == b.len()
                && a.iter()
                    .all(|(k, v)| b.get(k).is_some_and(|w| equivalent(v, w, k)))
        }
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(x, y)| equivalent(x, y, key))
        }
        (Value::Number(a), Value::Number(b)) => match (a.as_f64(), b.as_f64()) {
            (Some(_), Some(_)) if a.is_i64() && b.is_i64() => a == b,
            (Some(x), Some(y)) => {
                x.is_finite()
                    && y.is_finite()
                    && (x - y).abs()
                        <= if key == "psr" || key == "percentile" {
                            1e-9
                        } else {
                            1e-10 * x.abs().max(y.abs()).max(1.0)
                        }
            }
            _ => false,
        },
        _ => a == b,
    }
}
pub fn require_holdout(
    inputs: &InputPaths<'_>,
    envelope: Option<&Path>,
    ledger: Option<&Path>,
) -> Result<()> {
    let envelope = envelope.ok_or("holdout requires --dev-match")?;
    let matched: Value = serde_json::from_slice(&fs::read(envelope)?)?;
    if matched["match"].as_bool() != Some(true)
        || matched["rust_summary"]["stage"] != "dev"
        || matched["python_summary"]["stage"] != "dev"
    {
        return Err("dev comparison must match".into());
    }
    let dir = envelope
        .parent()
        .ok_or("dev match needs result directory")?;
    let mut expected = String::new();
    for name in OUTPUTS {
        expected.push_str(&format!("{}  {name}\n", digest(&dir.join(name))?));
    }
    if fs::read_to_string(dir.join("SHA256SUMS"))? != expected {
        return Err("dev result manifest mismatch".into());
    }
    let result_hash = digest(&dir.join("SHA256SUMS"))?;
    let summary: Value = serde_json::from_slice(&fs::read(dir.join("summary.json"))?)?;
    if matched["results_sha256"] != result_hash
        || matched["rust_summary"] != summary
        || !equivalent(&summary, &matched["python_summary"], "")
    {
        return Err("dev envelope results mismatch".into());
    }
    let ledger = fs::read_to_string(ledger.ok_or("holdout requires --ledger")?)?;
    let records: Vec<Value> = ledger
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<std::result::Result<Vec<_>, _>>()?
        .into_iter()
        .filter(|v| v["hypothesis"] == "H006")
        .collect();
    if records
        .iter()
        .any(|r| r["kind"] == "run" && r["stage"] == "holdout")
    {
        return Err("H006 holdout already used".into());
    }
    let (run_i, run) = records
        .iter()
        .enumerate()
        .rev()
        .find(|(_, r)| r["kind"] == "run" && r["stage"] == "dev")
        .ok_or("missing H006 dev run")?;
    if run["results_sha256"] != result_hash {
        return Err("dev run result hash mismatch".into());
    }
    let (verdict_i, verdict) = records
        .iter()
        .enumerate()
        .skip(run_i + 1)
        .rev()
        .find(|(_, r)| r["kind"] == "verdict" && r["stage"] == "dev")
        .ok_or("missing H006 dev verdict")?;
    if verdict["result"] != "pass"
        || verdict["results_sha256"] != result_hash
        || ["A", "B", "C", "D"]
            .iter()
            .any(|k| verdict["gates"][*k].as_bool() != Some(true))
    {
        return Err("H006 dev ledger gates or results mismatch".into());
    }
    if !records
        .iter()
        .skip(verdict_i + 1)
        .any(|r| r["kind"] == "holdout_open" && r["stage"] == "holdout")
    {
        return Err("missing H006 holdout_open".into());
    }
    if run["data_sha256"] != serde_json::to_value(all_hashes(inputs)?)? {
        return Err("dev run input hashes mismatch".into());
    }
    Ok(())
}
fn write_removed(path: &Path, removed: &[jiff::civil::DateTime]) -> Result<()> {
    let arr = StringArray::from(removed.iter().map(ToString::to_string).collect::<Vec<_>>());
    let schema = Arc::new(Schema::new(vec![Field::new(
        "timestamp",
        arr.data_type().clone(),
        false,
    )]));
    let batch = RecordBatch::try_new(schema.clone(), vec![Arc::new(arr)])?;
    let mut writer = ArrowWriter::try_new(File::create(path)?, schema, None)?;
    writer.write(&batch)?;
    writer.close()?;
    Ok(())
}
pub fn run(
    inputs: &InputPaths<'_>,
    out: &Path,
    stage: input::Stage,
    dev_match: Option<&Path>,
    ledger: Option<&Path>,
) -> Result<String> {
    checked_output_path(out)?;
    if stage == input::Stage::Holdout {
        require_holdout(inputs, dev_match, ledger)?;
    }
    let loaded = input::read(inputs.histdata_dir, stage)?;
    let raw_count = loaded.bars.len();
    let (bars, removed) = input::clean(&loaded.bars);
    drop(loaded.bars);
    let (clock, valid_clock) = input::clock_check(&bars);
    println!("clock_check {}", serde_json::to_string(&clock)?);
    io::stdout().flush()?;
    if !valid_clock {
        return Err("clock check failed".into());
    }
    let boundary = input::days(&bars)?;
    let calculated = daily::compute(
        &boundary,
        &bars,
        daily::Mode::Stop,
        false,
        daily::Order::Conservative,
        0.4,
        0.3,
    )?;
    let spy = segments::read_spy(inputs.spy, stage)?;
    let judgment = segments::compute(&calculated, &spy, stage)?;
    let mut hashes = loaded.hashes;
    hashes.insert("SPY.json".into(), digest(inputs.spy)?);
    publish_outputs(out, |tmp| {
        daily::write_days(&tmp.join("days.parquet"), &calculated)?;
        segments::write_accounts(&tmp.join("accounts.parquet"), &judgment.accounts)?;
        write_removed(&tmp.join("removed_bars.parquet"), &removed)?;
        let random = random::compute(&calculated, &bars, &tmp.join("random.parquet"))?;
        let summary = reports::assemble(&reports::SummaryInput {
            stage,
            input_hashes: &hashes,
            raw_bar_count: raw_count,
            duplicate_lines_removed: &loaded.duplicate_lines_removed,
            removed: &removed,
            clock: &clock,
            bars: &bars,
            boundaries: &boundary,
            daily: &calculated,
            spy: &spy,
            judgment: &judgment,
            random: &random,
        })?;
        reports::write_summary(&tmp.join("summary.json"), &summary)?;
        let mut manifest = String::new();
        for name in OUTPUTS {
            manifest.push_str(&format!("{}  {name}\n", digest(&tmp.join(name))?));
        }
        fs::write(tmp.join("SHA256SUMS"), manifest)?;
        Ok(())
    })?;
    Ok(format!(
        "{} stage={} results_sha256={}",
        out.display(),
        if stage == input::Stage::Dev {
            "dev"
        } else {
            "holdout"
        },
        digest(&out.join("SHA256SUMS"))?
    ))
}

#[cfg(test)]
mod cli_tests {
    use super::*;
    use serde_json::json;
    use std::{
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Fixture {
        root: PathBuf,
        zips: PathBuf,
        spy: PathBuf,
        dev: PathBuf,
        ledger: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "mikiri-h006-cli-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&root).unwrap();
            let zips = root.join("zips");
            fs::create_dir(&zips).unwrap();
            for year in 2007..=2025 {
                fs::write(
                    zips.join(format!("HISTDATA_COM_ASCII_EURUSD_M1_{year}.zip")),
                    b"synthetic",
                )
                .unwrap();
            }
            for month in 1..=10 {
                fs::write(
                    zips.join(format!("HISTDATA_COM_ASCII_EURUSD_M1_2026{month:02}.zip")),
                    b"synthetic",
                )
                .unwrap();
            }
            let spy = root.join("SPY.json");
            fs::write(&spy, b"spy").unwrap();
            let dev = root.join("dev");
            fs::create_dir(&dev).unwrap();
            for name in OUTPUTS {
                fs::write(
                    dev.join(name),
                    if name == "summary.json" {
                        b"{\"stage\":\"dev\"}".as_slice()
                    } else {
                        b"synthetic result"
                    },
                )
                .unwrap();
            }
            let manifest = OUTPUTS
                .iter()
                .map(|name| format!("{}  {name}\n", digest(&dev.join(name)).unwrap()))
                .collect::<String>();
            fs::write(dev.join("SHA256SUMS"), manifest).unwrap();
            let hash = digest(&dev.join("SHA256SUMS")).unwrap();
            fs::write(dev.join("verdict.json"),json!({"match":true,"rust_summary":{"stage":"dev"},"python_summary":{"stage":"dev"},"results_sha256":hash}).to_string()).unwrap();
            let ledger = root.join("ledger.jsonl");
            let inputs = InputPaths {
                histdata_dir: &zips,
                spy: &spy,
            };
            let rows = [
                json!({"hypothesis":"H006","kind":"run","stage":"dev","data_sha256":all_hashes(&inputs).unwrap(),"results_sha256":hash}),
                json!({"hypothesis":"H006","kind":"verdict","stage":"dev","result":"pass","gates":{"A":true,"B":true,"C":true,"D":true},"results_sha256":hash}),
                json!({"hypothesis":"H006","kind":"holdout_open","stage":"holdout"}),
            ];
            fs::write(
                &ledger,
                rows.iter().map(|r| format!("{r}\n")).collect::<String>(),
            )
            .unwrap();
            Self {
                root,
                zips,
                spy,
                dev,
                ledger,
            }
        }
        fn check(&self) -> Result<()> {
            require_holdout(
                &InputPaths {
                    histdata_dir: &self.zips,
                    spy: &self.spy,
                },
                Some(&self.dev.join("verdict.json")),
                Some(&self.ledger),
            )
        }
        fn change_ledger(&self, f: impl FnOnce(&mut Vec<Value>)) {
            let text = fs::read_to_string(&self.ledger).unwrap();
            let mut rows: Vec<Value> = text
                .lines()
                .map(|l| serde_json::from_str(l).unwrap())
                .collect();
            f(&mut rows);
            fs::write(
                &self.ledger,
                rows.iter().map(|v| format!("{v}\n")).collect::<String>(),
            )
            .unwrap();
        }
        fn change_match(&self, f: impl FnOnce(&mut Value)) {
            let p = self.dev.join("verdict.json");
            let mut v: Value = serde_json::from_slice(&fs::read(&p).unwrap()).unwrap();
            f(&mut v);
            fs::write(p, v.to_string()).unwrap();
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
    #[test]
    fn authorized_holdout_gate() {
        assert!(Fixture::new().check().is_ok());
    }
    #[test]
    fn refusal_without_match() {
        let f = Fixture::new();
        f.change_match(|v| v["match"] = json!(false));
        assert!(f.check().is_err());
    }
    #[test]
    fn refusal_missing_dev_run() {
        let f = Fixture::new();
        f.change_ledger(|v| {
            v.remove(0);
        });
        assert!(f.check().is_err());
    }
    #[test]
    fn refusal_missing_dev_verdict() {
        let f = Fixture::new();
        f.change_ledger(|v| {
            v.remove(1);
        });
        assert!(f.check().is_err());
    }
    fn refuse_gate(gate: &str) {
        let f = Fixture::new();
        f.change_ledger(|v| v[1]["gates"][gate] = json!(false));
        assert!(f.check().is_err());
    }
    #[test]
    fn refusal_gate_a() {
        refuse_gate("A");
    }
    #[test]
    fn refusal_gate_b() {
        refuse_gate("B");
    }
    #[test]
    fn refusal_gate_c() {
        refuse_gate("C");
    }
    #[test]
    fn refusal_gate_d() {
        refuse_gate("D");
    }
    #[test]
    fn refusal_verdict_hash_mismatch() {
        let f = Fixture::new();
        f.change_ledger(|v| v[1]["results_sha256"] = json!("0".repeat(64)));
        assert!(f.check().is_err());
    }
    #[test]
    fn refusal_run_result_hash_mismatch() {
        let f = Fixture::new();
        f.change_ledger(|v| v[0]["results_sha256"] = json!("0".repeat(64)));
        assert!(f.check().is_err());
    }
    #[test]
    fn refusal_zip_hash_mismatch() {
        let f = Fixture::new();
        fs::write(
            f.zips.join("HISTDATA_COM_ASCII_EURUSD_M1_202610.zip"),
            "tampered",
        )
        .unwrap();
        assert!(f.check().is_err());
    }
    #[test]
    fn refusal_input_hash_mismatch() {
        let f = Fixture::new();
        fs::write(&f.spy, "tampered").unwrap();
        assert!(f.check().is_err());
    }
    #[test]
    fn refusal_missing_holdout_open() {
        let f = Fixture::new();
        f.change_ledger(|v| {
            v.pop();
        });
        assert!(f.check().is_err());
    }
    #[test]
    fn refusal_checks_authorization_before_opening_holdout_files() {
        let f = Fixture::new();
        f.change_ledger(|v| {
            v.pop();
        });
        fs::remove_file(f.zips.join("HISTDATA_COM_ASCII_EURUSD_M1_202610.zip")).unwrap();
        assert_eq!(
            f.check().unwrap_err().to_string(),
            "missing H006 holdout_open"
        );
    }
    #[test]
    fn refusal_prior_holdout_run() {
        let f = Fixture::new();
        f.change_ledger(|v| v.push(json!({"hypothesis":"H006","kind":"run","stage":"holdout"})));
        assert!(f.check().is_err());
    }
    #[test]
    fn refusal_tampered_dev_output() {
        let f = Fixture::new();
        fs::write(f.dev.join("days.parquet"), "tampered").unwrap();
        assert!(f.check().is_err());
    }
    #[test]
    fn refusal_verdict_result_not_pass() {
        let f = Fixture::new();
        f.change_ledger(|v| v[1]["result"] = json!("reject"));
        assert!(f.check().is_err());
    }
    #[test]
    fn refusal_envelope_rust_stage_not_dev() {
        let f = Fixture::new();
        f.change_match(|v| v["rust_summary"]["stage"] = json!("holdout"));
        assert!(f.check().is_err());
    }
    #[test]
    fn refusal_tampered_envelope() {
        let f = Fixture::new();
        f.change_match(|v| v["python_summary"]["stage"] = json!("holdout"));
        assert!(f.check().is_err());
    }
    #[test]
    fn refusal_missing_arguments() {
        let f = Fixture::new();
        let input = InputPaths {
            histdata_dir: &f.zips,
            spy: &f.spy,
        };
        assert!(require_holdout(&input, None, Some(&f.ledger)).is_err());
        assert!(require_holdout(&input, Some(&f.dev.join("verdict.json")), None).is_err());
    }
    #[test]
    fn authorized_synthetic_holdout_completes() {
        use jiff::civil::{Weekday, date};
        use std::io::Write;
        let f = Fixture::new();
        let mut dates = std::collections::BTreeSet::new();
        let mut d = date(2007, 1, 1);
        while d <= date(2007, 5, 31) {
            if d.weekday() != Weekday::Saturday {
                dates.insert(d);
            }
            d = d.tomorrow().unwrap();
        }
        for date in [
            date(2007, 7, 1),
            date(2007, 7, 2),
            date(2008, 1, 2),
            date(2019, 11, 1),
            date(2020, 7, 5),
            date(2020, 7, 6),
            date(2025, 1, 5),
            date(2025, 1, 6),
            date(2025, 7, 6),
            date(2025, 7, 7),
        ] {
            dates.insert(date);
        }
        let mut contents: std::collections::BTreeMap<String, String> =
            std::collections::BTreeMap::new();
        for (i, d) in dates.into_iter().enumerate() {
            let id = if d.year() == 2026 {
                format!("2026{:02}", d.month())
            } else {
                format!("{}", d.year())
            };
            let output = contents.entry(id).or_default();
            let stamp = format!("{:04}{:02}{:02}", d.year(), d.month(), d.day());
            if d.weekday() == Weekday::Sunday {
                output.push_str(&format!(
                    "{stamp} 170000;1.100000;1.100010;1.099990;1.100000;0\n"
                ));
                continue;
            }
            let x = (i as f64 * 0.41).sin() * 0.002;
            let y = (i as f64 * 0.73).cos() * 0.001;
            let (eu, exit) = input::boundary_minutes(d).unwrap();
            let points = [
                (eu, 1.1),
                (480, 1.1 + x),
                (509, 1.1 + x),
                (510, 1.1 + x),
                (511, 1.102 + x),
                (512, 1.10195 + x),
                (exit, 1.1 + x + y),
            ];
            for (minute, p) in points {
                output.push_str(&format!(
                    "{stamp} {:02}{:02}00;{p:.8};{:.8};{:.8};{p:.8};0\n",
                    minute / 60,
                    minute % 60,
                    p + 0.000001,
                    p - 0.000001
                ));
            }
        }
        let mut ids: Vec<String> = (2007..=2025).map(|year| year.to_string()).collect();
        ids.extend((1..=10).map(|month| format!("2026{month:02}")));
        for id in ids {
            let path = f
                .zips
                .join(format!("HISTDATA_COM_ASCII_EURUSD_M1_{id}.zip"));
            let mut zip = zip::ZipWriter::new(File::create(path).unwrap());
            let opt = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            zip.start_file(format!("DAT_ASCII_EURUSD_M1_{id}.csv"), opt)
                .unwrap();
            zip.write_all(contents.get(&id).map_or("", String::as_str).as_bytes())
                .unwrap();
            zip.finish().unwrap();
        }
        let dates = [
            "2007-07-02",
            "2008-01-02",
            "2019-11-01",
            "2020-07-06",
            "2025-01-06",
            "2025-07-07",
        ];
        let spy:Vec<_>=dates.iter().enumerate().map(|(i,d)|json!({"date":format!("{d}T00:00:00Z"),"close":100.0+i as f64,"divCash":0.0,"splitFactor":1.0})).collect();
        fs::write(&f.spy, serde_json::to_vec(&spy).unwrap()).unwrap();
        let hashes = all_hashes(&InputPaths {
            histdata_dir: &f.zips,
            spy: &f.spy,
        })
        .unwrap();
        f.change_ledger(|rows| rows[0]["data_sha256"] = json!(hashes));
        let out = PathBuf::from(format!(
            "results/h006-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        assert!(!out.exists());
        let result = run(
            &InputPaths {
                histdata_dir: &f.zips,
                spy: &f.spy,
            },
            &out,
            input::Stage::Holdout,
            Some(&f.dev.join("verdict.json")),
            Some(&f.ledger),
        );
        if let Err(error) = &result {
            eprintln!("holdout failed: {error}");
        }
        assert!(result.is_ok());
        let summary: Value =
            serde_json::from_slice(&fs::read(out.join("summary.json")).unwrap()).unwrap();
        assert_eq!(summary["stage"], "holdout");
        assert!(summary["gates"]["H"].is_boolean());
        for name in OUTPUTS {
            assert!(out.join(name).is_file());
        }
        assert!(out.join("SHA256SUMS").is_file());
        fs::remove_dir_all(out).unwrap();
    }
}
