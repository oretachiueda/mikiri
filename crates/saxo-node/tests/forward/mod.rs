use super::*;
use forward::{
    journal::{Action, CostStatus, Environment as LedgerEnv, Journal, Row, Status},
    plan::Plan,
};
use saxo::{
    http::TEST_BASE,
    test_support::{market_data, serve},
};
use serde_json::{Value, json};
use std::{collections::HashSet, fs, os::unix::fs::PermissionsExt};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

#[derive(Default)]
struct Exchange {
    orders: Vec<Value>,
    history: Vec<Value>,
    requests: Vec<(String, String)>,
    net: i64,
    trigger_stop: bool,
    trigger_on_poll: bool,
}

impl Exchange {
    fn activity(&mut self, index: usize, status: &str) {
        let order = &self.orders[index];
        let mut row = order.clone();
        row["ActivityTime"] = json!(
            OffsetDateTime::now_utc()
                .format(&Rfc3339)
                .expect("timestamp")
        );
        row["LogId"] = json!((10_000 + self.history.len()).to_string());
        row["Status"] = json!(status);
        if status == "FinalFill" {
            let units = order["Amount"].as_i64().expect("units");
            let buying = order["BuySell"] == "Buy";
            let price = if buying { 131.427 } else { 131.359 };
            self.net += if buying { units } else { -units };
            row["FilledAmount"] = json!(units);
            row["FillAmount"] = json!(units);
            row["AveragePrice"] = json!(price);
            row["ExecutionPrice"] = json!(price);
        }
        self.history.push(row);
    }

    fn place(&mut self, mut order: Value) -> String {
        let index = self.orders.len();
        let id = (70 + index).to_string();
        order["OrderId"] = json!(id);
        order["OrderTime"] = json!(
            OffsetDateTime::now_utc()
                .format(&Rfc3339)
                .expect("timestamp")
        );
        order["OpenOrderType"] = order["OrderType"].clone();
        order["Price"] = order["OrderPrice"].clone();
        self.orders.push(order);
        self.activity(index, "Placed");
        id
    }

    fn respond(&mut self, method: &str, path: &str, mut request: Value) -> Value {
        let route = path.split('?').next().expect("route");
        match (method, route) {
            ("GET", "/port/v1/positions/me") => {
                if self.net == 0 {
                    json!({"Data": []})
                } else {
                    json!({"Data": [{"PositionBase": {
                        "Uic": 42, "AssetType": "FxSpot", "Amount": self.net,
                        "OpenPrice": 131.359, "IsForceOpen": false
                    }}]})
                }
            }
            ("GET", "/port/v1/orders/me") => {
                let open: Vec<_> = self
                    .orders
                    .iter()
                    .filter(|o| {
                        self.history
                            .iter()
                            .rev()
                            .find(|r| r["OrderId"] == o["OrderId"])
                            .is_some_and(|r| {
                                matches!(r["Status"].as_str(), Some("Placed" | "Changed"))
                            })
                    })
                    .collect();
                json!({"Data": open})
            }
            ("GET", "/cs/v1/audit/orderactivities") => {
                let id = path
                    .split(['?', '&'])
                    .find_map(|part| part.strip_prefix("OrderId="));
                if let Some(id) = id
                    && self.trigger_on_poll
                    && let Some(index) = self
                        .orders
                        .iter()
                        .position(|o| o["OrderId"] == id && o["OrderType"] == "Stop")
                {
                    self.trigger_on_poll = false;
                    self.activity(index, "FinalFill");
                }
                let rows: Vec<_> = self
                    .history
                    .iter()
                    .filter(|row| id.is_none_or(|id| row["OrderId"] == id))
                    .collect();
                json!({"Data": rows})
            }
            ("POST", "/trade/v2/orders") => {
                assert_eq!(request["OrderType"], "Market");
                self.requests.push((
                    method.into(),
                    request["ExternalReference"]
                        .as_str()
                        .expect("reference")
                        .into(),
                ));
                let children = request["Orders"].take();
                let id = self.place(request);
                self.activity(self.orders.len() - 1, "FinalFill");
                if let Some(orders) = children.as_array() {
                    assert_eq!(orders.len(), 1);
                    assert_eq!(orders[0]["OrderType"], "Stop");
                    let stop = self.place(orders[0].clone());
                    return json!({"OrderId": id, "Orders": [{"OrderId": stop}]});
                }
                json!({"OrderId": id})
            }
            ("PATCH", "/trade/v2/orders") => {
                let index = self
                    .orders
                    .iter()
                    .position(|o| o["OrderId"] == request["OrderId"])
                    .expect("modify target");
                assert_eq!(self.orders[index]["OrderType"], "Stop");
                assert_ne!(self.orders[index]["Price"], request["OrderPrice"]);
                self.requests.push((
                    method.into(),
                    self.orders[index]["ExternalReference"]
                        .as_str()
                        .expect("reference")
                        .into(),
                ));
                self.orders[index]["Price"] = request["OrderPrice"].clone();
                self.orders[index]["OrderTime"] = json!(
                    OffsetDateTime::now_utc()
                        .format(&Rfc3339)
                        .expect("timestamp")
                );
                self.activity(index, "Changed");
                if self.trigger_stop {
                    self.activity(index, "FinalFill");
                }
                json!({})
            }
            ("DELETE", _) => {
                let id = route
                    .strip_prefix("/trade/v2/orders/")
                    .expect("cancel route");
                let index = self
                    .orders
                    .iter()
                    .position(|o| o["OrderId"] == id)
                    .expect("cancel target");
                assert_eq!(self.orders[index]["OrderType"], "Stop");
                self.requests.push((
                    method.into(),
                    self.orders[index]["ExternalReference"]
                        .as_str()
                        .expect("reference")
                        .into(),
                ));
                self.activity(index, "Cancelled");
                json!({})
            }
            _ => {
                let mut body = market_data(method, path)
                    .unwrap_or_else(|| panic!("unexpected {method} {path}"));
                if route == "/trade/v1/infoprices" {
                    body["Quote"]["Bid"] = json!(131.377);
                    body["Quote"]["Ask"] = json!(131.402);
                }
                body
            }
        }
    }
}

fn plan_text(start: OffsetDateTime, stop_only: bool, exit_delay: i64) -> String {
    // Offset the synthetic day so a run across UTC midnight has the same windows.
    let noon = time::macros::time!(12:00);
    let offset =
        i32::try_from(i64::from(noon.hour()) * 3600 - start.unix_timestamp().rem_euclid(86_400))
            .expect("offset");
    let start = start.to_offset(time::UtcOffset::from_whole_seconds(offset).expect("offset"));
    let clock = |seconds| {
        (start + time::Duration::seconds(seconds))
            .time()
            .format(&time::macros::format_description!(
                "[hour]:[minute]:[second]"
            ))
            .expect("time")
    };
    let date = start.date();
    let mut text = format!(
        r#"
instrument_id = "USD/JPY.SAXO"
price_increment_milli = 1
utc_offset_seconds = {offset}
base_capital_milli = 220000000
halt_threshold_milli = 100000
event_dates = ["{date}"]
expires = "{date}"

[blackout]
start = "01:11:11"
end = "01:12:12"

[sizing]
unit_cap = 1000
absolute_cap = 1000
multiplier = 1
lot_size = 1000
minimum_units = 1000
"#
    );
    if !stop_only {
        text.push_str(&format!(
            r#"
[[legs]]
name = "A"
direction = "buy"
max_spread_milli = 50
entry = {{ start = "{}", end = "{}" }}
exit = {{ start = "{}", end = "{}" }}
stop = {{ enabled = false, width_ppm = 0 }}
order_limits = {{ entry = 1, stop = 0, exit = 2, cancel = 1 }}
"#,
            clock(4),
            clock(8),
            clock(9),
            clock(14)
        ));
    }
    let begin = if stop_only { 4 } else { 14 };
    text.push_str(&format!(
        r#"
[[legs]]
name = "B"
direction = "sell"
max_spread_milli = 50
entry = {{ start = "{}", end = "{}" }}
exit = {{ start = "{}", end = "{}" }}
stop = {{ enabled = true, width_ppm = 12345 }}
order_limits = {{ entry = 1, stop = 1, exit = 2, cancel = 1 }}
"#,
        clock(begin),
        clock(begin + 4),
        clock(begin + exit_delay),
        clock(begin + exit_delay + 5)
    ));
    text
}

struct FakeBase(tokio::task::JoinHandle<()>);
impl Drop for FakeBase {
    fn drop(&mut self) {
        self.0.abort();
        *TEST_BASE.lock().expect("test base") = None;
    }
}
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn run_fake(args: &Args) {
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                get_runtime().block_on(async {
                    let deadline = Instant::now() + Duration::from_secs(45);
                    tokio::time::timeout(
                        Duration::from_secs(55),
                        check(
                            args,
                            AccountKey("TestAccountKey1==".into()),
                            TokenManager::for_tests("synthetic"),
                            deadline,
                        ),
                    )
                    .await
                    .expect("forward timeout")
                    .expect("forward result");
                });
            })
            .join()
            .expect("node thread");
    });
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Scenario {
    Day,
    Stop,
    RestartStop,
}

fn scenario(case: Scenario, sample: bool) {
    let trigger_stop = case != Scenario::Day;
    let restart_open = case == Scenario::RestartStop;
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    get_runtime().block_on(async {
        let dir = tempfile::tempdir().expect("workspace");
        let path = dir.path().join("plan.toml");
        let ledger = dir.path().join("ledger.jsonl");
        let exchange = Arc::new(std::sync::Mutex::new(Exchange {
            trigger_stop: case == Scenario::Stop,
            ..Exchange::default()
        }));
        let server_exchange = exchange.clone();
        let (base, handle) = serve(move |method, path, body| {
            server_exchange
                .lock()
                .expect("exchange")
                .respond(method, path, body)
        })
        .await;
        *TEST_BASE.lock().expect("test base") = Some(base);
        let _base = FakeBase(handle);
        let start = OffsetDateTime::now_utc();
        fs::write(
            &path,
            plan_text(start, trigger_stop, if restart_open { 34 } else { 8 }),
        )
        .expect("write plan");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("permissions");
        let plan = Plan::read(&path, OffsetDateTime::now_utc()).expect("read plan");
        let mut args = Args {
            command: Command::Forward,
            out: dir.path().into(),
            app: dir.path().join("unused-app"),
            account: dir.path().join("unused-account"),
            kill: dir.path().join("kill"),
            redirect: "http://localhost:34567/cb".into(),
            timeout: 45,
            plan: Some(path.clone()),
            journal: Some(ledger.clone()),
            halt: None,
            until: restart_open
                .then(|| (start.to_offset(plan.utc_offset) + time::Duration::seconds(9)).time()),
            dry_run: false,
        };
        run_fake(&args);
        if restart_open {
            {
                let mut exchange = exchange.lock().expect("exchange");
                assert_eq!(
                    exchange
                        .requests
                        .iter()
                        .map(|(method, _)| method.as_str())
                        .collect::<Vec<_>>(),
                    ["POST", "PATCH"]
                );
                assert_eq!(exchange.net, -1000);
                exchange.trigger_on_poll = true;
            }
            args.until = None;
            run_fake(&args);
            println!("working stop restart broker writes=0");
        }
        let journal = Journal::new(&ledger, LedgerEnv::Sim, &plan, HashSet::new());
        let rows = journal.read().expect("read journal");
        let shape: Vec<_> = rows
            .iter()
            .map(|row| (row.leg.as_str(), row.action, row.status))
            .collect();
        let expected = if trigger_stop {
            vec![
                ("B", Action::Declare, Status::Scheduled),
                ("B", Action::Declare, Status::Scheduled),
                ("B", Action::Entry, Status::Filled),
                ("B", Action::StopExit, Status::Filled),
            ]
        } else {
            vec![
                ("A", Action::Declare, Status::Scheduled),
                ("B", Action::Declare, Status::Scheduled),
                ("A", Action::Entry, Status::Filled),
                ("A", Action::Declare, Status::Scheduled),
                ("A", Action::Exit, Status::Filled),
                ("B", Action::Declare, Status::Scheduled),
                ("B", Action::Entry, Status::Filled),
                ("B", Action::Cancel, Status::Scheduled),
                ("B", Action::StopExit, Status::Skipped),
                ("B", Action::Declare, Status::Scheduled),
                ("B", Action::Exit, Status::Filled),
            ]
        };
        assert_eq!(shape, expected);
        assert_fill_rows(&rows);
        let commands = |exchange: &Exchange| {
            exchange
                .requests
                .iter()
                .map(|(method, _)| method.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        };
        let before = {
            let exchange = exchange.lock().expect("exchange");
            let actual = commands(&exchange);
            let expected = if trigger_stop {
                "POST PATCH"
            } else {
                "POST POST POST PATCH DELETE POST"
            };
            assert_eq!(actual, expected);
            assert_eq!(exchange.net, 0);
            println!("broker writes {actual} units_net={}", exchange.net);
            exchange.requests.clone()
        };
        if !trigger_stop && !sample {
            let original = fs::read(&ledger).expect("ledger");
            run_fake(&args);
            assert_eq!(exchange.lock().expect("exchange").requests, before);
            assert_eq!(fs::read(&ledger).expect("restarted ledger"), original);
            println!(
                "restart broker writes=0 journal_bytes_unchanged={}",
                original.len()
            );
        }
        if sample {
            fs::copy(&ledger, "/tmp/r82-5c-sample.jsonl").expect("copy sample");
            println!("sample /tmp/r82-5c-sample.jsonl rows={}", rows.len());
        }
    });
}

fn assert_fill_rows(rows: &[Row]) {
    for row in rows.iter().filter(|r| r.status == Status::Filled) {
        assert_eq!(row.units, Some(1000));
        assert_eq!(row.cost_status, Some(CostStatus::Pending));
        assert!(
            row.order_id
                .as_ref()
                .is_some_and(|id| id.parse::<u64>().is_ok())
        );
        assert!(
            row.source_sha256
                .as_ref()
                .is_some_and(|hash| hash.len() == 64)
        );
        assert!(row.fill_time.is_some());
        if row.action != Action::Entry {
            assert_eq!(row.realized_pnl_jpy_ex_swap.map(|x| x.0), Some(-68_000));
        }
    }
}

#[test]
fn forward_two_legs_and_restart() {
    scenario(Scenario::Day, false);
}

#[test]
fn forward_broker_stop_without_market_exit() {
    scenario(Scenario::Stop, false);
}

#[test]
#[ignore = "writes the synthetic two-leg journal to /tmp"]
fn forward_export_sample() {
    scenario(Scenario::Day, true);
}

#[test]
fn forward_restores_working_stop_without_writes() {
    scenario(Scenario::RestartStop, false);
}
