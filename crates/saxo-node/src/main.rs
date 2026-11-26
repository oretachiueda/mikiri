mod strategy;
use nautilus_common::{enums::Environment, live::get_runtime, logging::logger::LoggerConfig};
use nautilus_live::{config::RoutingConfig, node::LiveNode};
use nautilus_model::identifiers::TraderId;
use saxo::{
    auth::{AppKey, Pkce, RedirectUri, TokenManager, wait_for_code},
    config::{SaxoDataClientConfig, SaxoExecutionClientConfig},
    env::Env,
    factories::{SaxoDataClientFactory, SaxoExecutionClientFactory},
};
use std::{
    collections::HashMap,
    fmt,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicI32, Ordering},
    },
    time::Duration,
};
use tokio::{net::TcpListener, sync::Mutex, time::Instant};

struct AccountKey(String);
impl fmt::Debug for AccountKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AccountKey(..)")
    }
}
impl AccountKey {
    fn from_file(path: &Path) -> anyhow::Result<Self> {
        use std::io::Read;
        let mut file = std::fs::File::open(path)?;
        anyhow::ensure!(
            file.metadata()?.permissions().mode() & 0o7777 == 0o600,
            "invalid account key permissions"
        );
        let mut value = String::new();
        file.read_to_string(&mut value)?;
        let value = value.trim();
        anyhow::ensure!(
            !value.is_empty() && !value.chars().any(char::is_whitespace),
            "invalid account key"
        );
        Ok(Self(value.to_owned()))
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Command {
    Check,
    SimRoundtrip,
}
struct Args {
    command: Command,
    out: PathBuf,
    app: PathBuf,
    account: PathBuf,
    kill: PathBuf,
    redirect: String,
    timeout: u64,
}
fn parse() -> anyhow::Result<Args> {
    let mut it = std::env::args().skip(1);
    let command = match it.next().as_deref() {
        Some("check") => Command::Check,
        Some("sim-roundtrip") => Command::SimRoundtrip,
        _ => anyhow::bail!("expected check or sim-roundtrip"),
    };
    let mut opts = HashMap::new();
    while let Some(opt) = it.next() {
        anyhow::ensure!(
            matches!(
                opt.as_str(),
                "--env"
                    | "--app-key-file"
                    | "--account-key-file"
                    | "--kill-file"
                    | "--redirect"
                    | "--timeout-secs"
                    | "--out"
            ),
            "unknown option"
        );
        let value = it
            .next()
            .ok_or_else(|| anyhow::anyhow!("missing option value"))?;
        anyhow::ensure!(opts.insert(opt, value).is_none(), "duplicate option");
    }
    anyhow::ensure!(
        opts.get("--env").map(String::as_str) == Some("sim"),
        "only sim is allowed"
    );
    let required = |name: &str| {
        opts.get(name)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("missing {name}"))
    };
    let redirect = required("--redirect")?;
    RedirectUri::new(&redirect)?;
    let timeout = opts
        .get("--timeout-secs")
        .map_or(Ok(180), |s| s.parse::<u64>())?;
    anyhow::ensure!((1..=600).contains(&timeout), "invalid timeout");
    Ok(Args {
        command,
        out: opts
            .get("--out")
            .map_or_else(|| "live/saxo-sim/".into(), PathBuf::from),
        app: required("--app-key-file")?.into(),
        account: required("--account-key-file")?.into(),
        kill: required("--kill-file")?.into(),
        redirect,
        timeout,
    })
}
fn main() {
    let args = match parse() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    if args.kill.exists() {
        eprintln!("kill file present");
        std::process::exit(4);
    }
    let app = match AppKey::from_file(&args.app) {
        Ok(v) => v,
        Err(_) => {
            eprintln!("invalid app key file");
            std::process::exit(2);
        }
    };
    let account = match AccountKey::from_file(&args.account) {
        Ok(v) => v,
        Err(_) => {
            eprintln!("invalid account key file");
            std::process::exit(2);
        }
    };
    let code = get_runtime().block_on(run(args, app, account));
    if let Err((code, message)) = code {
        eprintln!("{message}");
        std::process::exit(code);
    }
}
async fn run(args: Args, app: AppKey, account: AccountKey) -> Result<(), (i32, String)> {
    let deadline = Instant::now() + Duration::from_secs(args.timeout);
    let kill = async {
        loop {
            if args.kill.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    };
    let login = tokio::select! {
        result = login(&args, app) => result?,
        _ = tokio::time::sleep_until(deadline) => return Err((5, "timeout".into())),
        _ = kill => return Err((4, "kill file present".into())),
    };
    check(&args, account, login, deadline).await
}
async fn login(args: &Args, app: AppKey) -> Result<TokenManager, (i32, String)> {
    let login = async {
        let redirect = RedirectUri::new(&args.redirect)?;
        let authority = args
            .redirect
            .strip_prefix("http://localhost:")
            .ok_or_else(|| anyhow::anyhow!("invalid redirect"))?;
        let (port, path) = authority
            .split_once('/')
            .ok_or_else(|| anyhow::anyhow!("invalid redirect"))?;
        let port: u16 = port.parse()?;
        let listener = TcpListener::bind(("127.0.0.1", port)).await?;
        let pkce = Pkce::new();
        let url = pkce.authorize_url(Env::Sim, &app, &redirect)?;
        // shortcut: macOS open only, upgrade when the executor moves to the server (SSH tunnel login)
        let status = std::process::Command::new("open")
            .arg(url)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()?;
        anyhow::ensure!(status.success(), "browser failed");
        println!("opened Saxo SIM login in the default browser");
        let code = wait_for_code(
            listener,
            &format!("/{path}"),
            pkce.state(),
            Instant::now() + Duration::from_secs(300),
        )
        .await?;
        Ok::<_, anyhow::Error>(TokenManager::exchange(Env::Sim, &app, pkce, &redirect, code).await?)
    }
    .await
    .map_err(|e| (3, format!("login failed: {e:#}")))?;
    Ok(login)
}
async fn check(
    args: &Args,
    account: AccountKey,
    login: TokenManager,
    deadline: Instant,
) -> Result<(), (i32, String)> {
    let tokens = Arc::new(Mutex::new(login));
    let routing = RoutingConfig::builder().default(true).build();
    let mut node = LiveNode::builder(TraderId::from("SAXO-CHECK"), Environment::Live)
        .and_then(|b| {
            b.with_name("SAXO-CHECK".to_owned())
                .with_reconciliation(true)
                .with_logging(LoggerConfig::from_spec("stdout=Warn;fileout=Off")?)
                .add_data_client_with_routing(
                    None,
                    Box::new(SaxoDataClientFactory),
                    Box::new(SaxoDataClientConfig::new(Env::Sim, tokens.clone())),
                    routing.clone(),
                )?
                .add_exec_client_with_routing(
                    None,
                    Box::new(SaxoExecutionClientFactory),
                    Box::new(SaxoExecutionClientConfig::new(
                        Env::Sim,
                        account.0,
                        tokens.clone(),
                    )),
                    routing,
                )?
                .build()
        })
        .map_err(|e| (1, format!("node setup failed: {e:#}")))?;
    let handle = node.handle();
    let strategy_result = Arc::new(std::sync::Mutex::new(None));
    let mut fallback = None;
    if args.command == Command::SimRoundtrip {
        let (strategy, file) = strategy::Roundtrip::new(
            handle.clone(),
            strategy_result.clone(),
            args.kill.clone(),
            deadline,
            args.out.clone(),
        )
        .map_err(|e| (1, format!("strategy setup failed: {e:#}")))?;
        fallback = Some(file);
        node.add_strategy(strategy)
            .map_err(|e| (1, format!("strategy registration failed: {e:#}")))?;
    }
    let reason = Arc::new(AtomicI32::new(0));
    let monitor_reason = reason.clone();
    let connected = Arc::new(AtomicBool::new(false));
    let monitor_connected = connected.clone();
    let kill = args.kill.clone();
    let is_check = args.command == Command::Check;
    let balance = Arc::new(Mutex::new(None::<String>));
    let balance_result = balance.clone();
    let balance_connected = connected.clone();
    let balance_task = get_runtime().spawn(async move {
        if !is_check {
            return;
        }
        while !balance_connected.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let Ok(http) = saxo::http::SaxoHttp::new(Env::Sim) else {
            return;
        };
        let Ok(reply) = http
            .get(&mut *tokens.lock().await, "/port/v1/balances/me")
            .await
        else {
            return;
        };
        if reply.status != 200 {
            return;
        }
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(&reply.body) else {
            return;
        };
        if let Some(currency) = value
            .get("Currency")
            .and_then(|v| v.as_str())
            .filter(|v| v.len() == 3 && v.bytes().all(|b| b.is_ascii_uppercase()))
        {
            *balance_result.lock().await = Some(currency.to_owned());
        }
    });
    let monitor = get_runtime().spawn(async move {
        let stop_at = Instant::now() + Duration::from_secs(20);
        loop {
            if handle.is_running() {
                monitor_connected.store(true, Ordering::SeqCst);
            }
            if (is_check || !handle.is_running()) && kill.exists() {
                monitor_reason.store(4, Ordering::SeqCst);
                break;
            }
            if (is_check || !handle.is_running()) && Instant::now() >= deadline {
                monitor_reason.store(5, Ordering::SeqCst);
                break;
            }
            if is_check && Instant::now() >= stop_at {
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        handle.stop();
    });
    let result = node.run().await;
    monitor.abort();
    balance_task.abort();
    let currency = balance.lock().await.clone();
    if args.command == Command::SimRoundtrip {
        use std::io::Write;
        let code = *strategy_result
            .lock()
            .map_err(|_| (1, "strategy result unavailable".to_owned()))?;
        if code.is_none() {
            let reason_code = reason.load(Ordering::SeqCst);
            let code = if reason_code == 4 || reason_code == 5 {
                reason_code
            } else {
                1
            };
            let cache = node.kernel().cache();
            let cache = cache.borrow();
            let instrument = nautilus_model::identifiers::InstrumentId::from("USD/JPY.SAXO");
            let orders = cache.orders_open_count(None, Some(&instrument), None, None, None);
            let net = cache
                .positions_open(None, Some(&instrument), None, None, None)
                .iter()
                .map(|p| p.signed_qty)
                .reduce(|a, b| a + b)
                .map_or_else(|| "0".to_owned(), |n| n.to_string());
            if let Some(mut file) = fallback {
                let row = serde_json::json!({"ts": time::OffsetDateTime::now_utc().format(&time::format_description::well_known::Rfc3339).map_err(|e| (1, format!("timestamp failed: {e}")))?, "kind":"final", "client_order_id":null, "venue_order_id":null, "side":null, "qty":null, "price":null, "trigger_price":null, "note":format!("state=startup_failed open_orders={orders} position_net={net}")});
                serde_json::to_writer(&mut file, &row)
                    .map_err(|e| (1, format!("final record failed: {e}")))?;
                writeln!(file)
                    .and_then(|()| file.flush())
                    .map_err(|e| (1, format!("final record failed: {e}")))?;
            }
            if let Err(e) = result {
                return Err((code, format!("node failed: {e:#}")));
            }
            return Err((code, "roundtrip stopped without final state".to_owned()));
        }
        result.map_err(|e| (1, format!("node failed: {e:#}")))?;

        let code = *strategy_result
            .lock()
            .map_err(|_| (1, "strategy result unavailable".to_owned()))?;
        return match code {
            Some(0) => Ok(()),
            Some(code) => Err((code, "roundtrip stopped".to_owned())),
            None => Err((1, "roundtrip stopped without final state".to_owned())),
        };
    }
    result.map_err(|e| (1, format!("node failed: {e:#}")))?;
    let cache = node.kernel().cache();
    let cache = cache.borrow();
    let orders = cache.orders_open_count(None, None, None, None, None);
    let positions = cache.positions_open(None, None, None, None, None);
    let net = positions
        .iter()
        .map(|p| p.signed_qty)
        .reduce(|a, b| a + b)
        .map_or_else(|| "0".to_owned(), |n| n.to_string());
    println!(
        "connection={} open_orders={orders} position_net={net} balance_currency={}",
        if connected.load(Ordering::SeqCst) {
            "connected"
        } else {
            "not_confirmed"
        },
        currency.as_deref().unwrap_or("unknown")
    );
    match reason.load(Ordering::SeqCst) {
        4 => Err((4, "kill file present".into())),
        5 => Err((5, "timeout".into())),
        _ if !connected.load(Ordering::SeqCst) => Err((1, "connection not confirmed".into())),
        _ if currency.is_none() => Err((1, "balance currency unavailable".into())),
        _ => Ok(()),
    }
}
