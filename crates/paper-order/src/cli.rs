use std::path::PathBuf;

use jiff::{Timestamp, civil::Weekday, tz::TimeZone};

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Status,
    Buy(u32),
    Sell(u32),
}

#[derive(Debug, PartialEq, Eq)]
pub struct Args {
    pub command: Command,
    pub config: PathBuf,
    pub journal: PathBuf,
}

pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Args, String> {
    let mut args = args.into_iter();
    let verb = args.next().ok_or("missing command")?;
    if !matches!(verb.as_str(), "status" | "buy" | "sell") {
        return Err("expected status, buy, or sell".into());
    }
    let mut config = None;
    let mut journal = None;
    let mut shares = None;
    while let Some(option) = args.next() {
        match option.as_str() {
            "--config" | "--journal" => {
                let path = if option == "--config" {
                    &mut config
                } else {
                    &mut journal
                };
                if path.is_some() {
                    return Err(format!("duplicate option: {option}"));
                }
                *path = Some(PathBuf::from(
                    args.next()
                        .ok_or_else(|| format!("missing value for {option}"))?,
                ));
            }
            "--shares" => {
                if shares.is_some() {
                    return Err("duplicate option: --shares".into());
                }
                let value = args.next().ok_or("missing value for --shares")?;
                let value = value.parse::<u32>().map_err(|_| "invalid share count")?;
                if value == 0 {
                    return Err("share count must be positive".into());
                }
                shares = Some(value);
            }
            _ => return Err(format!("unknown argument: {option}")),
        }
    }
    let config = config.ok_or("missing --config")?;
    let journal = journal.ok_or("missing --journal")?;
    let command = match (verb.as_str(), shares) {
        ("status", None) => Command::Status,
        ("status", Some(_)) => return Err("--shares is only valid for buy or sell".into()),
        ("buy", Some(shares)) => Command::Buy(shares),
        ("sell", Some(shares)) => Command::Sell(shares),
        (_, None) => return Err("missing --shares".into()),
        _ => return Err("expected status, buy, or sell".into()),
    };
    Ok(Args {
        command,
        config,
        journal,
    })
}

pub fn market_hours(now: Timestamp) -> Result<(), String> {
    let local = now.to_zoned(TimeZone::get("America/New_York").map_err(|e| e.to_string())?);
    // shortcut: exchange holidays are not modeled; upgrade when holiday data is available.
    if matches!(local.weekday(), Weekday::Saturday | Weekday::Sunday)
        || (local.hour(), local.minute()) < (9, 35)
        || (local.hour(), local.minute()) > (15, 55)
    {
        return Err("outside market hours".into());
    }
    Ok(())
}

pub fn paper_account(accounts: &[String]) -> Result<&str, String> {
    match accounts {
        [account] if account.starts_with('D') => Ok(account),
        _ => Err("refused: not a paper account".into()),
    }
}

pub fn order_id(intent: &guard::Intent) -> Result<String, String> {
    if intent.strategy().contains('-') || intent.symbol().contains('-') {
        return Err("strategy and symbol must not contain '-'".into());
    }
    Ok(intent.key().replace(':', "-"))
}

pub fn gate(
    position: i64,
    journal_net: i64,
    open_orders: usize,
    pnl: Option<f64>,
) -> Result<f64, String> {
    if position != journal_net {
        return Err("refused: position mismatch".into());
    }
    if open_orders != 0 {
        return Err("refused: open order".into());
    }
    let pnl = pnl.ok_or("refused: daily P&L unknown")?;
    if !pnl.is_finite() {
        return Err("refused: daily P&L invalid".into());
    }
    Ok(pnl)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Terminal {
    Filled,
    Rejected,
    Canceled,
    Timeout,
}

pub fn terminal_exit(value: Terminal) -> u8 {
    match value {
        Terminal::Filled => 0,
        Terminal::Rejected | Terminal::Canceled | Terminal::Timeout => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use guard::Side;
    use proptest::prelude::*;

    fn instant(s: &str) -> Timestamp {
        s.parse().expect("valid test instant")
    }

    fn order_intent(strategy: &str, symbol: &str) -> guard::Intent {
        guard::Intent::new(
            time::macros::date!(2026 - 11 - 12),
            strategy,
            symbol,
            Side::Buy,
            1,
        )
        .expect("valid order intent")
    }

    #[test]
    fn parses_commands_options_and_rejects_bad_arguments() {
        assert_eq!(
            parse(
                [
                    "buy",
                    "--config",
                    "cfg",
                    "--journal",
                    "jrnl",
                    "--shares",
                    "2"
                ]
                .map(str::to_string)
            ),
            Ok(Args {
                command: Command::Buy(2),
                config: "cfg".into(),
                journal: "jrnl".into()
            })
        );
        assert!(
            parse(
                [
                    "sell",
                    "--config",
                    "cfg",
                    "--journal",
                    "jrnl",
                    "--shares",
                    "0"
                ]
                .map(str::to_string)
            )
            .is_err()
        );
        assert!(
            parse(["buy", "--config", "cfg", "--journal", "jrnl"].map(str::to_string)).is_err()
        );
        assert!(
            parse(["status", "--config", "cfg", "--journal", "jrnl"].map(str::to_string)).is_ok()
        );
        assert!(
            parse(
                [
                    "status",
                    "--config",
                    "cfg",
                    "--journal",
                    "jrnl",
                    "--shares",
                    "1"
                ]
                .map(str::to_string)
            )
            .is_err()
        );
        assert!(parse(["status"].map(str::to_string)).is_err());
        assert!(parse(["status", "--config", "cfg"].map(str::to_string)).is_err());
        assert!(
            parse(
                ["status", "--config", "a", "--config", "b", "--journal", "j"].map(str::to_string)
            )
            .is_err()
        );
        assert!(
            parse(
                [
                    "status",
                    "--config",
                    "c",
                    "--journal",
                    "a",
                    "--journal",
                    "b"
                ]
                .map(str::to_string)
            )
            .is_err()
        );
        assert!(
            parse(
                ["status", "--config", "c", "--journal", "j", "--other", "x"].map(str::to_string)
            )
            .is_err()
        );
    }

    #[test]
    fn accounts_gates_and_terminal_codes() {
        assert_eq!(paper_account(&["DU123".into()]), Ok("DU123"));
        assert!(paper_account(&["U123".into()]).is_err());
        assert!(paper_account(&[]).is_err());
        assert!(paper_account(&["D1".into(), "D2".into()]).is_err());
        assert_eq!(
            order_id(&order_intent("demo", "SPY")),
            Ok("2026-11-12-demo-SPY-buy".into())
        );
        assert!(order_id(&order_intent("bad-strategy", "SPY")).is_err());
        assert!(order_id(&order_intent("demo", "bad-symbol")).is_err());
        assert_eq!(gate(2, 2, 0, Some(-1.0)), Ok(-1.0));
        assert!(gate(2, 1, 0, Some(0.0)).is_err());
        assert!(gate(2, 2, 1, Some(0.0)).is_err());
        assert!(gate(2, 2, 0, None).is_err());
        assert_eq!(
            [
                Terminal::Filled,
                Terminal::Rejected,
                Terminal::Canceled,
                Terminal::Timeout
            ]
            .map(terminal_exit),
            [0, 1, 1, 1]
        );
    }

    #[test]
    fn market_hours_include_boundaries_and_dst_dates() {
        assert!(market_hours(instant("2026-03-09T13:35:00Z")).is_ok());
        assert!(market_hours(instant("2026-11-02T14:35:00Z")).is_ok());
        assert!(market_hours(instant("2026-03-09T13:34:00Z")).is_err());
        assert!(market_hours(instant("2026-03-09T19:55:00Z")).is_ok());
        assert!(market_hours(instant("2026-03-09T19:56:00Z")).is_err());
        assert!(market_hours(instant("2026-03-09T20:56:00Z")).is_err());
        assert!(market_hours(instant("2026-03-07T15:00:00Z")).is_err());
        assert!(market_hours(instant("2026-03-08T14:00:00Z")).is_err());
        assert!(market_hours(instant("2026-11-01T14:00:00Z")).is_err());
    }

    proptest! {
        #[test]
        fn id_mapping_is_injective_for_allowed_intents(
            strategy_a in "[A-Za-z0-9]+", strategy_b in "[A-Za-z0-9]+",
            symbol_a in "[A-Za-z0-9]+", symbol_b in "[A-Za-z0-9]+"
        ) {
            let intent_a = order_intent(&strategy_a, &symbol_a);
            let intent_b = order_intent(&strategy_b, &symbol_b);
            let id_a = order_id(&intent_a).unwrap();
            prop_assert_eq!(&id_a, &intent_a.key().replace(':', "-"));
            if strategy_a != strategy_b || symbol_a != symbol_b {
                prop_assert_ne!(id_a, order_id(&intent_b).unwrap());
            }
        }
    }
}
