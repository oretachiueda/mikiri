use guard::{Account, Intent, Journal, JournalError, Limits, Side, check};
use std::fs;
use time::macros::date;

#[test]
#[allow(clippy::unwrap_used)]
fn public_api_records_before_submission_and_blocks_restarts() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("limits.toml");
    let journal_path = dir.path().join("orders.jsonl");
    let today = date!(2026 - 11 - 11);
    fs::write(&config, "expires_on = \"2026-11-30\"\nsymbols = [\"SPY\"]\nmax_position_shares = 1\nmax_daily_orders = 4\nmax_daily_loss_usd = 50.0\n").unwrap();
    let limits = Limits::load(&config, today).unwrap();
    let mut journal = Journal::open(&journal_path).unwrap();
    let intent = Intent::new(today, "demo", "SPY", Side::Buy, 1).unwrap();
    let account = Account {
        position: 0,
        daily_pnl_usd: -3.2,
    };
    let approved = check(&limits, &account, journal.orders_on(today), &intent).unwrap();
    let recorded = journal.record(approved).unwrap();
    assert_eq!(recorded.intent(), &intent);
    let persisted = fs::read_to_string(&journal_path).unwrap();
    let row: serde_json::Value = serde_json::from_str(persisted.trim()).unwrap();
    assert_eq!(row["key"], recorded.key());
    println!(
        "Synthetic durable journal before submission: {}",
        persisted.trim()
    );
    drop(journal);
    let mut journal = Journal::open(&journal_path).unwrap();
    let retry = check(&limits, &account, journal.orders_on(today), &intent).unwrap();
    assert!(matches!(
        journal.record(retry),
        Err(JournalError::Duplicate)
    ));
    assert_eq!(fs::read_to_string(&journal_path).unwrap(), persisted);
    fs::write(dir.path().join("KILL"), "").unwrap();
    assert!(matches!(
        Limits::load(&config, today),
        Err(guard::ConfigError::Killed)
    ));
    println!("Restart: Duplicate; journal unchanged (1 line). Next config reload: Killed.");
}

#[test]
#[allow(clippy::unwrap_used)]
fn public_api_non_fill_outcomes_reconcile_but_do_not_retry() {
    for outcome in [
        guard::NonFillOutcome::Denied,
        guard::NonFillOutcome::Rejected,
    ] {
        for side in [Side::Buy, Side::Sell] {
            let dir = tempfile::tempdir().unwrap();
            let config = dir.path().join("limits.toml");
            let path = dir.path().join("journal");
            let today = date!(2026 - 11 - 11);
            fs::write(&config, "expires_on = \"2026-11-30\"\nsymbols = [\"SPY\"]\nmax_position_shares = 2\nmax_daily_orders = 4\nmax_daily_loss_usd = 50.0\n").unwrap();
            let limits = Limits::load(&config, today).unwrap();
            let mut journal = Journal::open(&path).unwrap();
            let initial =
                Intent::new(today.previous_day().unwrap(), "demo", "SPY", Side::Buy, 1).unwrap();
            let approved = check(
                &limits,
                &Account {
                    position: 0,
                    daily_pnl_usd: 0.0,
                },
                0,
                &initial,
            )
            .unwrap();
            journal.record(approved).unwrap();
            let intent = Intent::new(today, "demo", "SPY", side, 1).unwrap();
            let account = Account {
                position: 1,
                daily_pnl_usd: 0.0,
            };
            let approved = check(&limits, &account, 0, &intent).unwrap();
            let recorded = journal.record(approved).unwrap();
            journal.record_non_fill(&recorded, outcome).unwrap();
            drop(journal);
            let mut journal = Journal::open(&path).unwrap();
            assert_eq!(journal.net_shares("demo", "SPY").unwrap(), account.position);
            assert_eq!(journal.orders_on(today), 1);
            let retry = check(&limits, &account, journal.orders_on(today), &intent).unwrap();
            assert!(matches!(
                journal.record(retry),
                Err(JournalError::Duplicate)
            ));
            let persisted = fs::read_to_string(&path).unwrap();
            assert_eq!(persisted.lines().count(), 3);
            println!(
                "Synthetic {side:?} {outcome:?}: restart position=1 journal_net=1 match=true orders_today=1 retry=Duplicate lines=3"
            );
        }
    }
}
