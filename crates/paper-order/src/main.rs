mod baseline;
mod broker;
mod cli;
mod errors;
mod execution;

use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use guard::{Account, Intent, Journal, Limits, Side};
use nautilus_common::live::get_runtime;
use time::OffsetDateTime;

struct OrderSession {
    node: nautilus_live::node::LiveNode,
    journal: Arc<Mutex<Journal>>,
}

impl Drop for OrderSession {
    fn drop(&mut self) {
        self.node.dispose();
    }
}

fn terminal_result(
    outcome: Option<(String, u8)>,
    node_result: anyhow::Result<()>,
    account: Option<&str>,
) -> anyhow::Result<(String, u8, Option<String>)> {
    match (outcome, node_result) {
        (Some((line, code)), Ok(())) => Ok((line, code, None)),
        (Some((line, _)), Err(error)) => {
            Ok((line, 1, Some(errors::mask(&format!("{error:#}"), account))))
        }
        (None, Err(error)) => Err(error),
        (None, Ok(())) => anyhow::bail!("node stopped without result"),
    }
}

fn main() -> std::process::ExitCode {
    let mut account = None;
    match run(&mut account) {
        Ok((line, code)) => {
            println!("{line}");
            std::process::ExitCode::from(code)
        }
        Err(error) => {
            let (line, diagnostic) = errors::refusal(&error, account.as_deref());
            println!("{line}");
            eprintln!("{diagnostic}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run(account: &mut Option<String>) -> anyhow::Result<(String, u8)> {
    let args = cli::parse(std::env::args().skip(1)).map_err(anyhow::Error::msg)?;
    anyhow::ensure!(
        ["IBAPI_RECORDING_DIR", "IBAPI_RAW_CAPTURE_DIR"]
            .iter()
            .all(|key| std::env::var_os(key).is_none()),
        "IBAPI recording environment must be unset"
    );
    let today = OffsetDateTime::now_utc().date();
    let _limits = Limits::load(&args.config, today)?;
    let journal = Journal::open(&args.journal)?;
    if args.command != cli::Command::Status {
        cli::market_hours(jiff::Timestamp::now()).map_err(anyhow::Error::msg)?;
    }
    get_runtime().block_on(async move {
        let (shared, raw_account) = broker::connect().await?;
        *account = Some(raw_account.clone());
        if args.command == cli::Command::Status {
            let result = broker::snapshot(&shared).await;
            shared.disconnect().await;
            let snapshot = result?;
            let net = journal.net_shares("demo", "SPY")?;
            let pnl = snapshot.daily_pnl(
                &args.baseline,
                OffsetDateTime::now_utc().date(),
                &journal,
                &raw_account,
            );
            return Ok((snapshot.status_line(net, pnl), 0));
        }
        broker::require_writable(&shared).await?;
        let mut session = OrderSession {
            node: broker::build_node(&raw_account)?,
            journal: Arc::new(Mutex::new(journal)),
        };
        let handle = session.node.handle();
        let result = Arc::new(Mutex::new(None));
        let (sender, receiver) = mpsc::channel();
        session.node.add_strategy(execution::DemoStrategy::new(
            receiver,
            Arc::clone(&result),
            handle.clone(),
            raw_account.clone(),
        )?)?;
        let task_result = Arc::clone(&result);
        let task_journal = Arc::clone(&session.journal);
        let task_account = raw_account.clone();
        let task = get_runtime().spawn(async move {
            let work = async {
                tokio::time::timeout(Duration::from_secs(90), async {
                    while !handle.is_running() {
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                })
                .await
                .map_err(|_| anyhow::anyhow!("node readiness timeout"))?;
                let snapshot = broker::snapshot(&shared).await?;
                let (net, daily_pnl) = {
                    let journal = task_journal
                        .lock()
                        .map_err(|_| anyhow::anyhow!("journal lock unavailable"))?;
                    let net = journal.net_shares("demo", "SPY")?;
                    let pnl = snapshot.daily_pnl(
                        &args.baseline,
                        OffsetDateTime::now_utc().date(),
                        &journal,
                        &task_account,
                    );
                    (net, pnl)
                };
                let pnl = cli::gate(snapshot.position, net, snapshot.open_orders, daily_pnl)
                    .map_err(anyhow::Error::msg)?;
                let (side, shares) = match args.command {
                    cli::Command::Buy(shares) => (Side::Buy, shares),
                    cli::Command::Sell(shares) => (Side::Sell, shares),
                    cli::Command::Status => anyhow::bail!("invalid order command"),
                };
                let current_day = OffsetDateTime::now_utc().date();
                let limits = Limits::load(&args.config, current_day)?;
                cli::market_hours(jiff::Timestamp::now()).map_err(anyhow::Error::msg)?;
                let intent = Intent::new(current_day, "demo", "SPY", side, shares)?;
                let recorded = {
                    let mut journal = task_journal
                        .lock()
                        .map_err(|_| anyhow::anyhow!("journal lock unavailable"))?;
                    let approved = guard::check(
                        &limits,
                        &Account {
                            position: snapshot.position,
                            daily_pnl_usd: pnl,
                        },
                        journal.orders_on(current_day),
                        &intent,
                    )?;
                    journal.record(approved)?
                };
                sender
                    .send(recorded)
                    .map_err(|_| anyhow::anyhow!("order receiver unavailable"))?;
                Ok::<(), anyhow::Error>(())
            }
            .await;
            match work {
                Ok(()) => (),
                Err(error) => {
                    let (line, diagnostic) = errors::refusal(&error, Some(&task_account));
                    eprintln!("{diagnostic}");
                    if let Ok(mut slot) = task_result.lock() {
                        *slot = Some((line, 1));
                    }
                    handle.stop();
                }
            }
        });
        let node_result = session.node.run().await;
        drop(session);
        task.abort();
        let _ = task.await;
        let outcome = result
            .lock()
            .map_err(|_| anyhow::anyhow!("result unavailable"))?
            .take();
        let (line, code, diagnostic) = terminal_result(outcome, node_result, Some(&raw_account))?;
        if let Some(diagnostic) = diagnostic {
            eprintln!("{diagnostic}");
        }
        Ok((line, code))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_line_survives_node_error_and_exit_is_nonzero() {
        for (line, code) in [
            ("filled side=buy shares=1 position=1", 0),
            ("pending client_order_id=example", 1),
        ] {
            let (actual, exit, stderr) = terminal_result(
                Some((line.into(), code)),
                Err(anyhow::anyhow!("disconnect failed for DU1234567").context("shutdown failed")),
                Some("DU1234567"),
            )
            .unwrap();
            assert_eq!(actual, line);
            assert_eq!(exit, 1);
            assert_eq!(
                stderr.as_deref(),
                Some("shutdown failed: disconnect failed for <account>")
            );
        }
        assert!(terminal_result(None, Err(anyhow::anyhow!("startup failed")), None).is_err());
        assert_eq!(
            terminal_result(Some(("filled".into(), 0)), Ok(()), None).unwrap(),
            ("filled".into(), 0, None)
        );
    }

    fn session() -> (OrderSession, std::path::PathBuf) {
        let directory =
            std::env::temp_dir().join(format!("mikiri-session-{}", nautilus_core::UUID4::new()));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("journal.jsonl");
        let node = broker::build_node("DU1234567").unwrap();
        let journal = Arc::new(Mutex::new(Journal::open(path).unwrap()));
        (OrderSession { node, journal }, directory)
    }

    #[test]
    fn journal_stays_locked_after_preflight_until_node_disposal() {
        for preflight_failed in [false, true] {
            let (session, directory) = session();
            let handle = session.node.handle();
            let worker_journal = Arc::clone(&session.journal);
            let worker_result = if preflight_failed {
                Err("preflight failed")
            } else {
                Ok(())
            };
            drop(worker_journal);
            assert_eq!(worker_result.is_err(), preflight_failed);
            assert!(matches!(
                Journal::open(directory.join("journal.jsonl")),
                Err(guard::JournalError::Locked)
            ));
            assert_eq!(handle.state(), nautilus_live::node::NodeState::Idle);
            drop(session);
            assert_eq!(handle.state(), nautilus_live::node::NodeState::Stopped);
            drop(Journal::open(directory.join("journal.jsonl")).unwrap());
            std::fs::remove_dir_all(directory).unwrap();
        }
    }

    #[test]
    fn failed_strategy_registration_disposes_node_before_unlocking_journal() {
        let (mut session, directory) = session();
        let handle = session.node.handle();
        let result = (|| -> anyhow::Result<()> {
            for _ in 0..2 {
                let (_sender, receiver) = mpsc::channel();
                let strategy = execution::DemoStrategy::new(
                    receiver,
                    Arc::new(Mutex::new(None)),
                    handle.clone(),
                    "DU1234567".into(),
                )?;
                session.node.add_strategy(strategy)?;
            }
            Ok(())
        })();
        assert!(result.is_err());
        assert!(matches!(
            Journal::open(directory.join("journal.jsonl")),
            Err(guard::JournalError::Locked)
        ));
        drop(session);
        assert_eq!(handle.state(), nautilus_live::node::NodeState::Stopped);
        drop(Journal::open(directory.join("journal.jsonl")).unwrap());
        std::fs::remove_dir_all(directory).unwrap();
    }
}
