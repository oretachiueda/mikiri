mod broker;
mod cli;
mod errors;
mod execution;

use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use guard::{Account, Intent, Journal, Limits, Side};
use nautilus_common::live::get_runtime;
use time::OffsetDateTime;

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
            let pnl = snapshot.daily_pnl_usd.map_or_else(|| "unknown".into(), |v| format!("{v:.2}"));
            return Ok((format!("status account=paper position={} journal_net={net} open_orders={} match={} daily_pnl_usd={pnl}", snapshot.position, snapshot.open_orders, snapshot.position == net), 0));
        }
        broker::require_writable(&shared).await?;
        let mut node = broker::build_node(&raw_account)?;
        let handle = node.handle();
        let result = Arc::new(Mutex::new(None));
        let (sender, receiver) = mpsc::channel();
        node.add_strategy(execution::DemoStrategy::new(
                receiver,
                Arc::clone(&result),
                handle.clone(),
        )?)?;
        let task_result = Arc::clone(&result);
        let task = get_runtime().spawn(async move {
            let work = async {
                tokio::time::timeout(Duration::from_secs(90), async {
                    while !handle.is_running() {
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                }).await.map_err(|_| anyhow::anyhow!("node readiness timeout"))?;
                let snapshot = broker::snapshot(&shared).await?;
                let net = journal.net_shares("demo", "SPY")?;
                let pnl = cli::gate(snapshot.position, net, snapshot.open_orders, snapshot.daily_pnl_usd).map_err(anyhow::Error::msg)?;
                let (side, shares) = match args.command {
                    cli::Command::Buy(shares) => (Side::Buy, shares),
                    cli::Command::Sell(shares) => (Side::Sell, shares),
                    cli::Command::Status => anyhow::bail!("invalid order command"),
                };
                let current_day = OffsetDateTime::now_utc().date();
                let limits = Limits::load(&args.config, current_day)?;
                cli::market_hours(jiff::Timestamp::now()).map_err(anyhow::Error::msg)?;
                let intent = Intent::new(current_day, "demo", "SPY", side, shares)?;
                let approved = guard::check(&limits, &Account { position: snapshot.position, daily_pnl_usd: pnl }, journal.orders_on(current_day), &intent)?;
                let mut journal = journal;
                let recorded = journal.record(approved)?;
                sender.send(recorded).map_err(|_| anyhow::anyhow!("order receiver unavailable"))?;
                // Keep the journal lock until shutdown, including the terminal-event wait.
                while handle.is_running() {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                Ok::<(), anyhow::Error>(())
            }.await;
            match work {
                Ok(()) => (),
                Err(error) => {
                    let (line, diagnostic) = errors::refusal(&error, Some(&raw_account));
                    eprintln!("{diagnostic}");
                    if let Ok(mut slot) = task_result.lock() { *slot = Some((line, 1)); }
                    handle.stop();
                }
            }
        });
        let node_result = node.run().await;
        node.dispose();
        task.abort();
        let _ = task.await;
        let outcome = result.lock().map_err(|_| anyhow::anyhow!("result unavailable"))?.take();
        node_result?;
        outcome.ok_or_else(|| anyhow::anyhow!("node stopped without result"))
    })
}
