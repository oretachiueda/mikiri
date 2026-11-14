#![allow(clippy::unwrap_used)]

use super::*;
use proptest::prelude::*;
use tempfile::tempdir;
use time::macros::date;

const DAY: Date = date!(2026 - 11 - 11);
const CONFIG: &str = "expires_on = \"2026-11-30\"\nsymbols = [\"SPY\"]\nmax_position_shares = 10\nmax_daily_orders = 4\nmax_daily_loss_usd = 50.0\n";

fn limits() -> Limits {
    Limits(toml::from_str(CONFIG).unwrap())
}
fn intent(side: Side, shares: u32) -> Intent {
    Intent::new(DAY, "demo", "SPY", side, shares).unwrap()
}
fn account(position: i64, pnl: f64) -> Account {
    Account {
        position,
        daily_pnl_usd: pnl,
    }
}
fn approved() -> Approved {
    check(&limits(), &account(0, 0.0), 0, &intent(Side::Buy, 1)).unwrap()
}
fn load(text: &str, today: Date) -> Result<Limits, ConfigError> {
    let dir = tempdir().unwrap();
    let path = dir.path().join("limits.toml");
    fs::write(&path, text).unwrap();
    Limits::load(path, today)
}

#[test]
fn config_valid_dates_and_limits() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("limits.toml");
    assert!(matches!(
        Limits::load(&path, DAY),
        Err(ConfigError::Missing)
    ));
    assert!(matches!(
        Limits::load(dir.path(), DAY),
        Err(ConfigError::Io(_))
    ));
    fs::write(&path, CONFIG).unwrap();
    assert!(Limits::load(&path, DAY).is_ok());
    fs::create_dir(dir.path().join("KILL")).unwrap();
    assert!(matches!(Limits::load(&path, DAY), Err(ConfigError::Killed)));
    assert!(load(CONFIG, date!(2026 - 11 - 30)).is_ok());
    assert!(load(CONFIG, date!(2026 - 10 - 30)).is_ok());
    assert!(matches!(
        load(CONFIG, date!(2026 - 12 - 01)),
        Err(ConfigError::Expired)
    ));
    assert!(matches!(
        load(CONFIG, date!(2026 - 10 - 29)),
        Err(ConfigError::TooFar)
    ));
    assert!(matches!(load("[", DAY), Err(ConfigError::Parse(_))));
    assert!(matches!(
        load(&(CONFIG.to_owned() + "unknown = 1\n"), DAY),
        Err(ConfigError::Schema(_))
    ));
    assert!(matches!(
        load("symbols = []", DAY),
        Err(ConfigError::Schema(_))
    ));
    for replacement in ["[]", "[\"\"]", "[\"S PY\"]", "[\"S:PY\"]"] {
        assert!(matches!(
            load(&CONFIG.replace("[\"SPY\"]", replacement), DAY),
            Err(ConfigError::Symbols)
        ));
    }
    for (old, replacements) in [
        ("max_position_shares = 10", vec!["max_position_shares = 0"]),
        ("max_daily_orders = 4", vec!["max_daily_orders = 0"]),
        (
            "max_daily_loss_usd = 50.0",
            vec![
                "max_daily_loss_usd = 0.0",
                "max_daily_loss_usd = -1.0",
                "max_daily_loss_usd = nan",
                "max_daily_loss_usd = inf",
                "max_daily_loss_usd = -inf",
            ],
        ),
    ] {
        for replacement in replacements {
            assert!(
                matches!(
                    load(&CONFIG.replace(old, replacement), DAY),
                    Err(ConfigError::InvalidLimit)
                ),
                "{replacement}"
            );
        }
    }
    assert!(
        load(
            &CONFIG.replace("max_daily_orders = 4", "max_daily_orders = -1"),
            DAY
        )
        .is_err()
    );
    assert!(load(&CONFIG.replace("2026-11-30", "invalid"), DAY).is_err());
    assert!(
        load(
            &CONFIG.replace("2026-11-30", "9999-12-31"),
            date!(9999 - 12 - 31)
        )
        .is_ok()
    );
}

#[test]
fn identifiers_and_keys() {
    for bad in ["", ":", "a:b", " a", "a ", "a\tb", "a\nb", "a\u{2003}b"] {
        assert_eq!(
            Intent::new(DAY, bad, "SPY", Side::Buy, 1),
            Err(IntentError::Strategy)
        );
        assert_eq!(
            Intent::new(DAY, "demo", bad, Side::Buy, 1),
            Err(IntentError::Symbol)
        );
    }
    assert_eq!(
        Intent::new(DAY, "demo", "SPY", Side::Buy, 0),
        Err(IntentError::Shares)
    );
    let buy = intent(Side::Buy, 1);
    assert_eq!(buy.key(), "2026-11-11:demo:SPY:buy");
    assert_eq!(buy.key(), intent(Side::Buy, 2).key());
    assert_eq!(intent(Side::Sell, 1).key(), "2026-11-11:demo:SPY:sell");
    assert_eq!(buy.day(), DAY);
    assert_eq!(buy.strategy(), "demo");
    assert_eq!(buy.symbol(), "SPY");
    assert_eq!(buy.side(), Side::Buy);
    assert_eq!(buy.shares(), 1);
    assert_eq!(intent(Side::Buy, 2).shares(), 2);
}

#[test]
fn every_rejection_and_boundary() {
    let l = limits();
    let buy = intent(Side::Buy, 1);
    let sell = intent(Side::Sell, 1);
    let reject = |a: Account, count, i: &Intent, expected| {
        assert_eq!(check(&l, &a, count, i).unwrap_err(), expected);
    };
    let other = Intent::new(DAY, "demo", "QQQ", Side::Buy, 1).unwrap();
    reject(account(0, 0.0), 0, &other, Reject::Symbol);
    for pnl in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        reject(account(0, pnl), 0, &buy, Reject::NonFinitePnl);
    }
    reject(account(0, 0.0), 4, &buy, Reject::OrderCap);
    reject(account(0, 0.0), 5, &buy, Reject::OrderCap);
    reject(account(i64::MAX, 0.0), 0, &buy, Reject::Overflow);
    reject(account(i64::MIN, 0.0), 0, &sell, Reject::Overflow);
    reject(account(0, 0.0), 0, &sell, Reject::Short);
    reject(account(10, 0.0), 0, &buy, Reject::PositionCap);
    reject(account(0, -50.0), 0, &buy, Reject::DailyLoss);
    reject(account(0, -51.0), 0, &buy, Reject::DailyLoss);
    assert!(check(&l, &account(9, -49.99), 3, &buy).is_ok());
    assert!(check(&l, &account(1, -50.0), 3, &sell).is_ok());
    assert!(check(&l, &account(-1, -50.0), 0, &buy).is_ok());
    let mut huge = limits();
    huge.0.max_position_shares = i64::MAX;
    assert!(check(&huge, &account(i64::MAX - 1, 0.0), 0, &buy).is_ok());
    assert_eq!(
        check(&l, &account(i64::MIN, 0.0), 0, &buy).unwrap_err(),
        Reject::Short
    );
}

#[test]
fn journal_restart_lock_and_count() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("journal.jsonl");
    let mut journal = Journal::open(&path).unwrap();
    assert_eq!(journal.orders_on(DAY), 0);
    assert!(matches!(Journal::open(&path), Err(JournalError::Locked)));
    let recorded = journal.record(approved()).unwrap();
    assert_eq!(recorded.key(), "2026-11-11:demo:SPY:buy");
    assert_eq!(recorded.intent(), &intent(Side::Buy, 1));
    assert_eq!(journal.orders_on(DAY), 1);
    assert_eq!(journal.orders_on(DAY.next_day().unwrap()), 0);
    assert!(matches!(
        journal.record(approved()),
        Err(JournalError::Duplicate)
    ));
    drop(journal);
    let mut journal = Journal::open(&path).unwrap();
    assert_eq!(journal.orders_on(DAY), 1);
    assert!(matches!(
        journal.record(approved()),
        Err(JournalError::Duplicate)
    ));
    let text = fs::read_to_string(&path).unwrap();
    assert_eq!(text.lines().count(), 1);
    let value: serde_json::Value = serde_json::from_str(text.trim()).unwrap();
    assert_eq!(value.as_object().unwrap().len(), 7);
    assert_eq!(value["day"], "2026-11-11");
    assert_eq!(value["shares"], 1);
    assert!(value["recorded_at"].as_str().unwrap().ends_with('Z'));
    let sell = check(&limits(), &account(1, 0.0), 1, &intent(Side::Sell, 1)).unwrap();
    journal.record(sell).unwrap();
    assert_eq!(journal.orders_on(DAY), 2);
    drop(journal);
    assert_eq!(Journal::open(&path).unwrap().orders_on(DAY), 2);
    assert!(matches!(
        Journal::open(dir.path().join("absent/file")),
        Err(JournalError::Io(_))
    ));
    assert!(matches!(
        Journal::open(dir.path()),
        Err(JournalError::Io(_))
    ));
}

#[test]
fn journal_corruption_fails_closed() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("journal");
    let mut journal = Journal::open(&path).unwrap();
    journal.record(approved()).unwrap();
    drop(journal);
    let valid = fs::read_to_string(&path).unwrap();
    let mutations = [
        "{\n".to_owned(),
        "\n".to_owned(),
        valid.trim_end().to_owned(),
        format!("{valid}{{"),
        format!("{valid}{valid}"),
        valid.replace("demo:SPY:buy", "other:SPY:buy"),
        valid.replace("\"shares\":1", "\"shares\":0"),
        valid.replace("\"strategy\":\"demo\"", "\"strategy\":\"bad:name\""),
        valid.replace("\"symbol\":\"SPY\"", "\"symbol\":\"\""),
        valid.replace("Z\"", "+01:00\""),
        valid.replace("\"side\":\"buy\"", "\"side\":\"invalid\""),
    ];
    for bad in mutations {
        fs::write(&path, &bad).unwrap();
        assert!(
            matches!(Journal::open(&path), Err(JournalError::Corrupt)),
            "{bad}"
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), bad);
    }
}

#[cfg(unix)]
#[test]
fn kill_switch_dangling_symlink_and_invalid_utf8() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("limits");
    fs::write(&path, [0xff]).unwrap();
    assert!(matches!(Limits::load(&path, DAY), Err(ConfigError::Io(_))));
    std::os::unix::fs::symlink("absent", dir.path().join("KILL")).unwrap();
    assert!(matches!(Limits::load(&path, DAY), Err(ConfigError::Killed)));
}

#[test]
fn failed_write_poison_prevents_retry() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("journal");
    fs::write(&path, "").unwrap();
    let mut j = Journal::open(&path).unwrap();
    j.file = File::open(&path).unwrap();
    assert!(matches!(j.record(approved()), Err(JournalError::Io(_))));
    assert!(matches!(j.record(approved()), Err(JournalError::Poisoned)));
    assert_eq!(j.orders_on(DAY), 0);
    assert_eq!(fs::read_to_string(path).unwrap(), "");
}

proptest! {
    #[test]
    fn approval_implies_all_rules(
        max in 1i64..=i64::MAX, cap in 1u64..=u64::MAX,
        loss in 0.001f64..1e12, position in any::<i64>(),
        pnl in any::<f64>(), orders in any::<u64>(), shares in 1u32..=u32::MAX,
        buy in any::<bool>(), allowed in any::<bool>(),
    ) {
        let l = Limits(Config { expires_on: DAY, symbols: vec!["SPY".into()], max_position_shares: max, max_daily_orders: cap, max_daily_loss_usd: loss });
        let i = Intent::new(DAY, "demo", if allowed {"SPY"} else {"QQQ"}, if buy {Side::Buy} else {Side::Sell}, shares).unwrap();
        let new = if buy { position.checked_add(i64::from(shares)) } else { position.checked_sub(i64::from(shares)) };
        let expected = allowed && pnl.is_finite() && orders < cap && new.is_some_and(|n| n >= 0 && n <= max && (pnl > -loss || n.unsigned_abs() < position.unsigned_abs()));
        prop_assert_eq!(check(&l, &account(position, pnl), orders, &i).is_ok(), expected);
    }

    #[test]
    fn single_violated_rule_rejects(which in 0u8..5, shares in 1u32..1_000_000, pnl in 0f64..1000.0) {
        let mut l = limits();
        l.0.max_position_shares = i64::from(shares) + 1;
        let mut a = account(0, pnl);
        let mut orders = 0;
        let mut i = intent(Side::Buy, shares);
        match which {
            0 => l.0.symbols.clear(),
            1 => a.daily_pnl_usd = f64::NAN,
            2 => orders = l.0.max_daily_orders,
            3 => i = intent(Side::Sell, shares),
            _ => a.daily_pnl_usd = -l.0.max_daily_loss_usd,
        }
        prop_assert!(check(&l, &a, orders, &i).is_err());
    }
}

#[cfg(unix)]
#[test]
fn failed_fsync_never_returns_recorded() {
    use std::{os::fd::OwnedFd, os::unix::net::UnixStream};
    let dir = tempdir().unwrap();
    let mut j = Journal::open(dir.path().join("journal")).unwrap();
    let (writer, _reader) = UnixStream::pair().unwrap();
    j.file = File::from(OwnedFd::from(writer));
    assert!(matches!(j.record(approved()), Err(JournalError::Io(_))));
    assert!(matches!(j.record(approved()), Err(JournalError::Poisoned)));
    assert_eq!(j.orders_on(DAY), 0);
}

#[test]
fn negative_integer_limits_fail_closed() {
    for (old, new) in [
        ("max_daily_orders = 4", "max_daily_orders = -1"),
        ("max_position_shares = 10", "max_position_shares = -1"),
    ] {
        assert!(load(&CONFIG.replace(old, new), DAY).is_err());
    }
}

#[test]
fn inaccessible_kill_metadata_fails_closed() {
    let dir = tempdir().unwrap();
    let parent = dir.path().join("file");
    fs::write(&parent, "not a directory").unwrap();
    assert!(matches!(
        Limits::load(parent.join("limits"), DAY),
        Err(ConfigError::Io(_))
    ));
}

#[test]
fn invalid_utf8_journal_is_corrupt() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("journal");
    fs::write(&path, [0xff, b'\n']).unwrap();
    assert!(matches!(Journal::open(path), Err(JournalError::Corrupt)));
}

#[cfg(target_os = "macos")]
#[test]
fn unreadable_kill_metadata_blocks_readable_config() {
    let dir = tempdir().unwrap();
    let config = dir.path().join("limits.toml");
    let kill = dir.path().join("KILL");
    fs::write(&config, CONFIG).unwrap();
    fs::write(&kill, "").unwrap();
    let status = std::process::Command::new("chmod")
        .args(["+a", "everyone deny readattr"])
        .arg(&kill)
        .status()
        .unwrap();
    assert!(status.success());
    assert!(fs::symlink_metadata(&kill).is_err());
    assert!(fs::read_to_string(&config).is_ok());
    assert!(matches!(
        Limits::load(&config, DAY),
        Err(ConfigError::Io(_))
    ));
}

#[test]
fn journal_net_shares_filters_and_survives_reopen() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("journal.jsonl");
    let mut journal = Journal::open(&path).unwrap();
    assert_eq!(journal.net_shares("demo", "SPY").unwrap(), 0);
    for (day, strategy, symbol, side, shares) in [
        (DAY, "demo", "SPY", Side::Buy, 4),
        (DAY.next_day().unwrap(), "demo", "SPY", Side::Sell, 1),
        (DAY, "other", "SPY", Side::Buy, 8),
        (DAY, "demo", "QQQ", Side::Buy, 7),
    ] {
        let intent = Intent::new(day, strategy, symbol, side, shares).unwrap();
        journal.record(Approved(intent)).unwrap();
    }
    assert_eq!(journal.net_shares("demo", "SPY").unwrap(), 3);
    assert_eq!(journal.net_shares("other", "SPY").unwrap(), 8);
    assert_eq!(journal.net_shares("demo", "QQQ").unwrap(), 7);
    assert_eq!(journal.net_shares("missing", "SPY").unwrap(), 0);
    drop(journal);
    let journal = Journal::open(path).unwrap();
    assert_eq!(journal.net_shares("demo", "SPY").unwrap(), 3);
}

#[test]
fn journal_share_sum_rejects_positive_and_negative_overflow() {
    for deltas in [[i64::MAX, 1], [i64::MIN, -1]] {
        assert!(matches!(sum_shares(deltas), Err(JournalError::Overflow)));
    }
    assert_eq!(sum_shares([i64::MAX, -1]).unwrap(), i64::MAX - 1);
    assert_eq!(sum_shares([i64::MIN, 1]).unwrap(), i64::MIN + 1);
}

#[test]
fn non_fill_outcomes_reconcile_without_releasing_keys() {
    for outcome in ["denied", "rejected"] {
        let dir = tempdir().unwrap();
        let path = dir.path().join("journal");
        let mut journal = Journal::open(&path).unwrap();
        let recorded = journal.record(approved()).unwrap();
        assert_eq!(journal.net_shares("demo", "SPY").unwrap(), 1);
        let original = fs::read_to_string(&path).unwrap();
        drop(journal);
        let terminal = format!(
            "{{\"key\":\"{}\",\"outcome\":\"{outcome}\",\"recorded_at\":\"2026-11-11T16:00:00Z\"}}\n",
            recorded.key()
        );
        fs::write(&path, format!("{original}{terminal}")).unwrap();
        let mut journal = Journal::open(&path).unwrap();
        assert_eq!(journal.net_shares("demo", "SPY").unwrap(), 0);
        assert_eq!(journal.orders_on(DAY), 1);
        assert!(matches!(
            journal.record(approved()),
            Err(JournalError::Duplicate)
        ));
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            format!("{original}{terminal}")
        );
    }
}
