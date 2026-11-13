use super::*;
use serde_json::json;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "mikiri-h005-boundary-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&p).unwrap();
        Self(p)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn rate_csv() -> String {
    let mut s="DATAFLOW,REF_AREA,FREQ,MEASURE,UNIT_MEASURE,ACTIVITY,ADJUSTMENT,TRANSFORMATION,TIME_HORIZ,METHODOLOGY,TIME_PERIOD,OBS_VALUE,OBS_STATUS,UNIT_MULT,DECIMALS,BASE_PER\n".to_string();
    for area in AREAS {
        s.push_str(&format!("OECD.SDD.STES:DSD_STES@DF_FINMARK(4.0),{area},M,IR3TIB,PA,_Z,_Z,_Z,_Z,N,2007-12,-1.2,A,0,2,\n"));
    }
    s
}
#[test]
fn fred_requires_exact_header_increasing_dates_and_positive_finite_spot() {
    let dir = Scratch::new();
    let p = dir.0.join("fx.csv");
    for body in [
        "date,DEXUSEU\n2008-01-01,1\n",
        "observation_date,DEXUSEU\n2008-01-01,1\n2008-01-01,2\n",
        "observation_date,DEXUSEU\n2008-01-02,1\n2008-01-01,2\n",
        "observation_date,DEXUSEU\n2008-01-01,0\n",
        "observation_date,DEXUSEU\n2008-01-01,NaN\n",
        "observation_date,DEXUSEU\n,1\n",
        "observation_date,DEXUSEU\n2008-01-01,.\n",
    ] {
        fs::write(&p, body).unwrap();
        assert!(
            fred(&p, "DEXUSEU", "fx", Stage::Dev, false).is_err(),
            "{body}"
        );
    }
    fs::write(
        &p,
        "observation_date,DEXUSEU\n2008-01-01,1.2\n2008-01-02,\n",
    )
    .unwrap();
    let rows = fred(&p, "DEXUSEU", "fx", Stage::Dev, false).unwrap();
    assert_eq!(rows[1].value, None);
}
#[test]
fn oecd_validates_source_units_areas_duplicate_pairs_and_cutoff_before_values() {
    let dir = Scratch::new();
    let p = dir.0.join("rates.csv");
    let valid = rate_csv();
    fs::write(&p, &valid).unwrap();
    assert_eq!(oecd(&p, Stage::Dev).unwrap().len(), 10);
    for bad in [
        valid.replace(",M,", ",D,"),
        valid.replace(",IR3TIB,", ",OTHER,"),
        valid.replace(",PA,", ",USD,"),
        valid.replace(",_Z,", ",WRONG,"),
        valid.replace(",N,", ",X,"),
        valid.replace("USA", "OTHER"),
        valid.replace("USA", "EA20"),
        valid.replace(",-1.2,", ",inf,"),
        valid
            .lines()
            .filter(|s| !s.contains(",GBR,"))
            .collect::<Vec<_>>()
            .join("\n"),
    ] {
        fs::write(&p, bad).unwrap();
        assert!(oecd(&p, Stage::Dev).is_err());
    }
    let future = format!(
        "{valid}OECD.SDD.STES:DSD_STES@DF_FINMARK(4.0),GBR,M,IR3TIB,PA,_Z,_Z,_Z,_Z,N,2024-10,invalid,A,0,2,\n"
    );
    fs::write(&p, future).unwrap();
    assert_eq!(oecd(&p, Stage::Dev).unwrap().len(), 10);
    assert!(oecd(&p, Stage::Holdout).is_err());
}
#[test]
fn load_discards_all_future_values_and_ignores_missing_vintages() {
    let dir = Scratch::new();
    for series in FX {
        fs::write(
            dir.0.join(format!("{series}.csv")),
            format!("observation_date,{series}\n2008-01-02,1.2\n2024-11-01,-1\n"),
        )
        .unwrap();
    }
    let rates = dir.0.join("rates.csv");
    fs::write(&rates, rate_csv()).unwrap();
    let spy = dir.0.join("spy.json");
    fs::write(&spy,r#"[{"date":"2008-01-02T00:00:00.000Z","close":100.0,"divCash":0.0,"splitFactor":1.0},{"date":"2024-11-01T00:00:00.000Z","close":-1.0,"divCash":-1.0,"splitFactor":0.0}]"#).unwrap();
    let paths = InputPaths {
        fx_dir: &dir.0,
        rates: &rates,
        spy: &spy,
        first_release_dir: Some(&dir.0),
    };
    let a = serde_json::to_vec(&load(&paths, Stage::Dev).unwrap()).unwrap();
    for series in FX {
        fs::write(
            dir.0.join(format!("{series}.csv")),
            format!("observation_date,{series}\n2008-01-02,1.2\n2024-11-01,garbage\n"),
        )
        .unwrap();
    }
    let b = serde_json::to_vec(&load(&paths, Stage::Dev).unwrap()).unwrap();
    assert_eq!(a, b);
    assert!(load(&paths, Stage::Holdout).is_err());
}

fn authorized(dir: &Scratch) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
    for id in FX {
        fs::write(dir.0.join(format!("{id}.csv")), b"source bytes").unwrap();
    }
    let rates = dir.0.join("OECD.csv");
    let spy = dir.0.join("SPY.json");
    fs::write(&rates, b"rates").unwrap();
    fs::write(&spy, b"spy").unwrap();
    let out = dir.0.join("dev");
    fs::create_dir(&out).unwrap();
    for name in OUTPUTS {
        fs::write(
            out.join(name),
            if name == "summary.json" {
                b"{\"stage\":\"dev\"}".as_slice()
            } else {
                b"synthetic result".as_slice()
            },
        )
        .unwrap();
    }
    let manifest = OUTPUTS
        .iter()
        .map(|name| format!("{}  {name}\n", digest(&out.join(name)).unwrap()))
        .collect::<String>();
    fs::write(out.join("SHA256SUMS"), manifest).unwrap();
    let hash = digest(&out.join("SHA256SUMS")).unwrap();
    let envelope = out.join("verdict.json");
    fs::write(&envelope,serde_json::to_vec(&json!({"match":true,"python_summary":{"stage":"dev"},"rust_summary":{"stage":"dev"},"results_sha256":hash})).unwrap()).unwrap();
    let paths = InputPaths {
        fx_dir: &dir.0,
        rates: &rates,
        spy: &spy,
        first_release_dir: None,
    };
    let records = [
        json!({"hypothesis":"H005","kind":"run","stage":"dev","data_sha256":input_hashes(&paths).unwrap(),"results_sha256":hash}),
        json!({"hypothesis":"H005","kind":"verdict","stage":"dev","result":"pass","gates":{"A":true,"B":true,"C":true},"results_sha256":hash}),
        json!({"hypothesis":"H005","kind":"holdout_open","stage":"holdout","reason":"synthetic"}),
    ];
    let ledger = dir.0.join("ledger.jsonl");
    fs::write(
        &ledger,
        records.iter().map(|v| format!("{v}\n")).collect::<String>(),
    )
    .unwrap();
    (rates, spy, envelope, ledger)
}

#[test]
fn optional_vintages_allow_missing_but_reject_malformed_observations() {
    let dir = Scratch::new();
    for series in FX {
        fs::write(
            dir.0.join(format!("{series}.csv")),
            format!("observation_date,{series}\n2008-01-02,1.2\n"),
        )
        .unwrap();
    }
    let rates = dir.0.join("rates.csv");
    fs::write(&rates, rate_csv()).unwrap();
    let spy = dir.0.join("spy.json");
    fs::write(
        &spy,
        r#"[{"date":"2008-01-02T00:00:00.000Z","close":100.0,"divCash":0.0,"splitFactor":1.0}]"#,
    )
    .unwrap();
    let vintages = dir.0.join("vintages");
    let paths = InputPaths {
        fx_dir: &dir.0,
        rates: &rates,
        spy: &spy,
        first_release_dir: Some(&vintages),
    };
    assert!(load(&paths, Stage::Dev).is_ok());
    fs::create_dir(&vintages).unwrap();
    let path = vintages.join("DEXUSEU.csv");
    fs::write(&path, "observation_date,DEXUSEU\n2008-01-02,\n").unwrap();
    assert!(load(&paths, Stage::Dev).is_ok());
    for invalid in [
        "wrong_header\n2008-01-02,1\n",
        "observation_date,DEXUSEU\n2008-01-02,0\n",
        "observation_date,DEXUSEU\n2008-01-02,1\n2008-01-02,2\n",
    ] {
        fs::write(&path, invalid).unwrap();
        assert!(
            load(&paths, Stage::Dev).is_err(),
            "malformed optional data was silently discarded"
        );
    }
}

#[test]
fn holdout_checks_each_authorization_binding() {
    for case in [
        "valid",
        "missing-envelope",
        "missing-ledger",
        "no-match",
        "no-run",
        "no-verdict",
        "gate-a",
        "gate-b",
        "gate-c",
        "result-hash",
        "input-hash",
        "missing-open",
        "used",
        "tampered-result",
        "envelope-result",
        "python-summary",
    ] {
        let dir = Scratch::new();
        let (rates, spy, envelope, ledger) = authorized(&dir);
        let mut records = fs::read_to_string(&ledger)
            .unwrap()
            .lines()
            .map(|s| serde_json::from_str::<Value>(s).unwrap())
            .collect::<Vec<_>>();
        match case {
            "no-match" => {
                let mut v: Value = serde_json::from_slice(&fs::read(&envelope).unwrap()).unwrap();
                v["match"] = json!(false);
                fs::write(&envelope, v.to_string()).unwrap();
            }
            "no-run" => {
                records.remove(0);
            }
            "no-verdict" => {
                records.remove(1);
            }
            "gate-a" => records[1]["gates"]["A"] = json!(false),
            "gate-b" => records[1]["gates"]["B"] = json!(false),
            "gate-c" => records[1]["gates"]["C"] = json!(false),
            "result-hash" => records[1]["results_sha256"] = json!("0".repeat(64)),
            "input-hash" => {
                fs::write(&spy, b"changed input").unwrap();
            }
            "missing-open" => {
                records.pop();
            }
            "used" => records.push(json!({"hypothesis":"H005","kind":"run","stage":"holdout"})),
            "tampered-result" => {
                fs::write(envelope.parent().unwrap().join("days.parquet"), b"changed").unwrap();
            }
            "envelope-result" => {
                let mut v: Value = serde_json::from_slice(&fs::read(&envelope).unwrap()).unwrap();
                v["results_sha256"] = json!("0".repeat(64));
                fs::write(&envelope, v.to_string()).unwrap();
            }
            "python-summary" => {
                let mut v: Value = serde_json::from_slice(&fs::read(&envelope).unwrap()).unwrap();
                v["python_summary"]["main"] = json!({"W":-987654321.0});
                fs::write(&envelope, v.to_string()).unwrap();
            }
            _ => {}
        }
        fs::write(
            &ledger,
            records.iter().map(|v| format!("{v}\n")).collect::<String>(),
        )
        .unwrap();
        let paths = InputPaths {
            fx_dir: &dir.0,
            rates: &rates,
            spy: &spy,
            first_release_dir: None,
        };
        let result = require_holdout(
            &paths,
            if case == "missing-envelope" {
                None
            } else {
                Some(&envelope)
            },
            if case == "missing-ledger" {
                None
            } else {
                Some(&ledger)
            },
        );
        assert_eq!(result.is_ok(), case == "valid", "{case}: {result:?}");
    }
}

#[test]
fn envelope_comparison_respects_numeric_tolerances_and_exact_signals() {
    assert!(equivalent(
        &json!({"W": 1.0, "psr_main":0.99, "signal":1}),
        &json!({"W": 1.0+5e-11, "psr_main":0.99+5e-10, "signal":1}),
        ""
    ));
    assert!(!equivalent(
        &json!({"W": 1.0}),
        &json!({"W": -987654321.0}),
        ""
    ));
    assert!(!equivalent(
        &json!({"signal":1}),
        &json!({"signal":0.99999999999}),
        ""
    ));
}
