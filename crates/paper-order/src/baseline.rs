use std::{
    fs::{self, File, OpenOptions},
    io::{ErrorKind, Write},
    path::Path,
};

use anyhow::{anyhow, ensure};
use nautilus_core::UUID4;
use serde::{Deserialize, Serialize};
use time::Date;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Baseline {
    day: Date,
    equity_usd: f64,
}

// shortcut: baseline is the first equity seen each UTC day, so losses before the first run are not counted, and deposits or withdrawals distort it; run status right after the open and set KILL on transfer days; revisit before real trading
pub fn pnl(
    path: &Path,
    today: Date,
    current_equity: f64,
    _journal: &guard::Journal,
) -> anyhow::Result<f64> {
    ensure!(current_equity.is_finite(), "current equity is invalid");
    let baseline = match fs::read(path) {
        Ok(bytes) => {
            let baseline: Baseline = serde_json::from_slice(&bytes)
                .map_err(|_| anyhow!("equity baseline is invalid"))?;
            ensure!(
                baseline.equity_usd.is_finite(),
                "equity baseline is invalid"
            );
            ensure!(baseline.day <= today, "equity baseline is in the future");
            Some(baseline)
        }
        Err(error) if error.kind() == ErrorKind::NotFound => None,
        Err(_) => return Err(anyhow!("cannot read equity baseline")),
    };
    if let Some(baseline) = baseline
        && baseline.day == today
    {
        let pnl = current_equity - baseline.equity_usd;
        ensure!(pnl.is_finite(), "equity change is invalid");
        return Ok(pnl);
    }
    persist(
        path,
        &Baseline {
            day: today,
            equity_usd: current_equity,
        },
    )?;
    Ok(0.0)
}

fn persist(path: &Path, baseline: &Baseline) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let temporary = parent.join(format!(".paper-order-baseline-{}.tmp", UUID4::new()));
    let bytes =
        serde_json::to_vec(baseline).map_err(|_| anyhow!("cannot encode equity baseline"))?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temporary)
        .map_err(|_| anyhow!("cannot create equity baseline temporary file"))?;
    let result = (|| {
        file.write_all(&bytes)
            .map_err(|_| anyhow!("cannot write equity baseline"))?;
        file.sync_all()
            .map_err(|_| anyhow!("cannot sync equity baseline"))?;
        fs::rename(&temporary, path).map_err(|_| anyhow!("cannot replace equity baseline"))?;
        File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|_| anyhow!("cannot sync equity baseline directory"))
    })();
    drop(file);
    match fs::remove_file(&temporary) {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(_) => return Err(anyhow!("cannot clean equity baseline temporary file")),
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use time::macros::date;

    struct Fixture {
        directory: PathBuf,
        journal: guard::Journal,
    }

    impl Fixture {
        fn new() -> Self {
            let directory = std::env::temp_dir().join(format!("mikiri-baseline-{}", UUID4::new()));
            fs::create_dir(&directory).unwrap();
            let journal = guard::Journal::open(directory.join("journal")).unwrap();
            Self { directory, journal }
        }

        fn path(&self) -> PathBuf {
            self.directory.join("baseline.json")
        }

        fn pnl(&self, day: Date, equity: f64) -> anyhow::Result<f64> {
            pnl(&self.path(), day, equity, &self.journal)
        }

        fn assert_no_temporary_files(&self) {
            let mut names: Vec<_> = fs::read_dir(&self.directory)
                .unwrap()
                .map(|entry| entry.unwrap().file_name().into_string().unwrap())
                .collect();
            names.sort();
            assert_eq!(names, ["baseline.json", "journal"]);
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.directory);
        }
    }

    #[test]
    fn creates_private_baseline_and_uses_same_day_delta() {
        let fixture = Fixture::new();
        assert_eq!(fixture.pnl(date!(2026 - 11 - 12), 1000.0).unwrap(), 0.0);
        let initial = fs::read(fixture.path()).unwrap();
        let json: serde_json::Value = serde_json::from_slice(&initial).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"day": "2026-11-12", "equity_usd": 1000.0})
        );
        assert_eq!(fixture.pnl(date!(2026 - 11 - 12), 975.0).unwrap(), -25.0);
        assert_eq!(fs::read(fixture.path()).unwrap(), initial);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(fixture.path()).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        fixture.assert_no_temporary_files();
    }

    #[test]
    fn older_day_is_replaced_and_future_day_is_preserved() {
        let fixture = Fixture::new();
        fixture.pnl(date!(2026 - 11 - 11), 1000.0).unwrap();
        assert_eq!(fixture.pnl(date!(2026 - 11 - 12), 950.0).unwrap(), 0.0);
        assert_eq!(fixture.pnl(date!(2026 - 11 - 12), 945.0).unwrap(), -5.0);
        let before = fs::read(fixture.path()).unwrap();
        assert_eq!(
            fixture
                .pnl(date!(2026 - 11 - 11), 900.0)
                .unwrap_err()
                .to_string(),
            "equity baseline is in the future"
        );
        assert_eq!(fs::read(fixture.path()).unwrap(), before);
        fixture.assert_no_temporary_files();
    }

    #[test]
    fn invalid_baselines_are_never_overwritten_or_echoed() {
        let fixture = Fixture::new();
        for bytes in [
            "private-invalid-json",
            r#"{"day":"2026-11-12","equity_usd":1000,"extra":1}"#,
            r#"{"day":"2026-11-12","equity_usd":null}"#,
            r#"{"day":"2026-11-12","equity_usd":1e999}"#,
            r#"{"day":"invalid","equity_usd":1000}"#,
        ] {
            fs::write(fixture.path(), bytes).unwrap();
            assert_eq!(
                fixture
                    .pnl(date!(2026 - 11 - 12), 950.0)
                    .unwrap_err()
                    .to_string(),
                "equity baseline is invalid"
            );
            assert_eq!(fs::read(fixture.path()).unwrap(), bytes.as_bytes());
            fixture.assert_no_temporary_files();
        }
    }

    #[test]
    fn invalid_current_equity_and_overflow_never_change_baseline() {
        let fixture = Fixture::new();
        for equity in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(fixture.pnl(date!(2026 - 11 - 12), equity).is_err());
            assert!(!fixture.path().exists());
        }
        fixture.pnl(date!(2026 - 11 - 12), -f64::MAX).unwrap();
        let before = fs::read(fixture.path()).unwrap();
        assert_eq!(
            fixture
                .pnl(date!(2026 - 11 - 12), f64::MAX)
                .unwrap_err()
                .to_string(),
            "equity change is invalid"
        );
        assert_eq!(fs::read(fixture.path()).unwrap(), before);
        fixture.assert_no_temporary_files();
    }

    #[test]
    fn failed_replacement_cleans_temporary_file() {
        let fixture = Fixture::new();
        fs::create_dir(fixture.path()).unwrap();
        assert_eq!(
            persist(
                &fixture.path(),
                &Baseline {
                    day: date!(2026 - 11 - 12),
                    equity_usd: 1000.0
                }
            )
            .unwrap_err()
            .to_string(),
            "cannot replace equity baseline"
        );
        assert!(fixture.path().is_dir());
        fixture.assert_no_temporary_files();
    }
}
