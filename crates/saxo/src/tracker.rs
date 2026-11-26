use crate::{
    auth::TokenManager,
    http::SaxoHttp,
    parse::{self, Fate},
};
use nautilus_core::UnixNanos;
use nautilus_live::execution::emitter::ExecutionEventEmitter;
use nautilus_model::{
    enums::{LiquiditySide, OrderType},
    identifiers::{ClientOrderId, TradeId, VenueOrderId},
    orders::{Order, OrderAny},
    types::{Currency, Price, Quantity},
};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};
use tokio::sync::Mutex;

pub(crate) struct Tracked {
    pub order: OrderAny,
    pub venue_id: Option<String>,
    seen: HashSet<String>,
    accepted: bool,
    filled: u64,
}
impl Tracked {
    pub fn new(order: OrderAny, venue_id: Option<String>) -> Self {
        Self {
            order,
            venue_id,
            seen: HashSet::new(),
            accepted: false,
            filled: 0,
        }
    }
}
pub(crate) type Orders = Arc<Mutex<HashMap<ClientOrderId, Tracked>>>;

pub(crate) async fn poll(
    http: &Arc<Mutex<SaxoHttp>>,
    tokens: &Arc<Mutex<TokenManager>>,
    orders: &Orders,
    ids: &Arc<Mutex<HashMap<ClientOrderId, String>>>,
    emitter: &ExecutionEventEmitter,
) {
    let keys: Vec<_> = orders.lock().await.keys().copied().collect();
    for key in keys {
        let id = match orders.lock().await.get(&key) {
            Some(o) => o.venue_id.clone(),
            None => continue,
        };
        let path = match &id {
            Some(id) if !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()) => {
                format!("/cs/v1/audit/orderactivities?OrderId={id}&EntryType=All")
            }
            Some(_) => continue,
            None => "/cs/v1/audit/orderactivities?EntryType=Last".to_owned(),
        };
        let reply = http
            .lock()
            .await
            .get(&mut *tokens.lock().await, &path)
            .await;
        let Ok(reply) = reply else { continue };
        if reply.status != 200 {
            continue;
        }
        let Ok(json) = std::str::from_utf8(&reply.body) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(json) else {
            continue;
        };
        let Some(rows) = value.get("Data").and_then(|d| d.as_array()) else {
            continue;
        };
        if id.is_none() {
            let found: HashSet<_> = rows
                .iter()
                .filter(|r| r["ExternalReference"].as_str() == Some(key.as_ref()))
                .filter_map(|r| r["OrderId"].as_str())
                .filter(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()))
                .collect();
            if found.len() == 1
                && let Some(id) = found.into_iter().next()
            {
                if let Some(order) = orders.lock().await.get_mut(&key) {
                    order.venue_id = Some(id.to_owned());
                }
                ids.lock().await.insert(key, id.to_owned());
            }
            continue;
        }
        let Some(id) = id else { continue };
        let fate = parse::fate(json, &id);
        let mut sorted: Vec<_> = rows
            .iter()
            .filter(|r| r["OrderId"].as_str() == Some(&id))
            .collect();
        sorted.sort_by(|a, b| {
            a["ActivityTime"]
                .as_str()
                .cmp(&b["ActivityTime"].as_str())
                .then_with(|| {
                    let a = a["LogId"].as_str().unwrap_or("").trim_start_matches('0');
                    let b = b["LogId"].as_str().unwrap_or("").trim_start_matches('0');
                    (a.len(), a).cmp(&(b.len(), b))
                })
        });
        let mut map = orders.lock().await;
        let Some(tracked) = map.get_mut(&key) else {
            continue;
        };
        let mut terminal = false;
        for row in sorted {
            let Some(log) = row["LogId"]
                .as_str()
                .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
            else {
                continue;
            };
            if tracked.seen.contains(log) {
                continue;
            }
            let Some(time) = row["ActivityTime"]
                .as_str()
                .and_then(|s| {
                    time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339)
                        .ok()
                })
                .and_then(|t| u64::try_from(t.unix_timestamp_nanos()).ok())
                .map(UnixNanos::from)
            else {
                continue;
            };
            let venue = VenueOrderId::from(id.as_str());
            let amount = row.get("FilledAmount").and_then(|v| v.as_f64());
            if let Some(amount) = amount {
                if !amount.is_finite()
                    || amount < 0.0
                    || amount.fract() != 0.0
                    || amount >= u64::MAX as f64
                {
                    eprintln!("Saxo history has invalid cumulative filled quantity");
                    break;
                }
                if amount < tracked.filled as f64 {
                    eprintln!("Saxo history has decreasing cumulative filled quantity");
                    break;
                }
            }
            let fill = if let Some(amount) = amount
                && amount > tracked.filled as f64
            {
                let price = row["ExecutionPrice"]
                    .as_f64()
                    .filter(|p| p.is_finite() && *p > 0.0)
                    .and_then(|p| Price::new_checked(p, 3).ok());
                let quantity = Quantity::new_checked(amount - tracked.filled as f64, 0).ok();
                match (price, quantity) {
                    (Some(price), Some(quantity)) => Some((amount as u64, price, quantity)),
                    _ => {
                        eprintln!("Saxo history lacks a valid execution price or quantity");
                        break;
                    }
                }
            } else {
                None
            };
            if !tracked.accepted {
                emitter.emit_order_accepted(&tracked.order, venue, time);
                tracked.accepted = true;
            }
            if let Some((amount, price, quantity)) = fill {
                emitter.emit_order_filled(
                    &tracked.order,
                    venue,
                    None,
                    TradeId::from(log),
                    quantity,
                    price,
                    Currency::JPY(),
                    None,
                    LiquiditySide::NoLiquiditySide,
                    time,
                );
                tracked.filled = amount;
            }
            let handled = match row["Status"].as_str() {
                Some("Placed" | "Fill" | "PartialFill") => true,
                Some("Changed") => {
                    if let Some(price) = row["Price"]
                        .as_f64()
                        .filter(|p| p.is_finite() && *p > 0.0)
                        .and_then(|p| Price::new_checked(p, 3).ok())
                    {
                        let is_stop = tracked.order.order_type() == OrderType::StopMarket;
                        emitter.emit_order_updated(
                            &tracked.order,
                            venue,
                            tracked.order.quantity(),
                            (!is_stop).then_some(price),
                            is_stop.then_some(price),
                            None,
                            time,
                        );
                        true
                    } else {
                        false
                    }
                }
                Some("FinalFill")
                    if matches!(fate, Fate::Done { .. })
                        && amount
                            .is_some_and(|v| v as u64 == tracked.filled && tracked.filled > 0) =>
                {
                    terminal = true;
                    true
                }
                Some("Cancelled") if matches!(fate, Fate::Cancelled { .. }) => {
                    emitter.emit_order_canceled(&tracked.order, Some(venue), time);
                    terminal = true;
                    true
                }
                Some("Expired" | "DoneForDay") => {
                    emitter.emit_order_expired(&tracked.order, Some(venue), time);
                    terminal = true;
                    true
                }
                Some("Rejected") => {
                    emitter.emit_order_rejected(&tracked.order, "Saxo rejected order", time, false);
                    terminal = true;
                    true
                }
                _ => false,
            };
            if handled {
                tracked.seen.insert(log.to_owned());
            }
            if terminal {
                break;
            }
        }
        if terminal {
            map.remove(&key);
        }
    }
}
pub(crate) async fn run(
    http: Arc<Mutex<SaxoHttp>>,
    tokens: Arc<Mutex<TokenManager>>,
    orders: Orders,
    ids: Arc<Mutex<HashMap<ClientOrderId, String>>>,
    emitter: ExecutionEventEmitter,
    interval: Duration,
) {
    loop {
        tokio::time::sleep(interval).await;
        poll(&http, &tokens, &orders, &ids, &emitter).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::test_support::server;
    use nautilus_common::messages::ExecutionEvent;
    use nautilus_core::{UUID4, time::get_atomic_clock_realtime};
    use nautilus_model::{
        enums::{AccountType, OrderSide, TimeInForce, TriggerType},
        events::OrderEventAny,
        identifiers::{AccountId, InstrumentId, StrategyId, TraderId},
        orders::{MarketOrder, StopMarketOrder},
    };
    use tokio::sync::mpsc;

    fn order(stop: bool) -> OrderAny {
        let trader = TraderId::from("TESTER-001");
        let strategy = StrategyId::from("S-001");
        let instrument = InstrumentId::from("USD/JPY.SAXO");
        let id = ClientOrderId::from("test-1");
        let qty = Quantity::from(1000);
        if stop {
            StopMarketOrder::new_checked(
                trader,
                strategy,
                instrument,
                id,
                OrderSide::Sell,
                qty,
                Price::from("145.000"),
                TriggerType::Default,
                TimeInForce::Day,
                None,
                false,
                false,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                UUID4::new(),
                0.into(),
            )
            .expect("stop")
            .into()
        } else {
            MarketOrder::new_checked(
                trader,
                strategy,
                instrument,
                id,
                OrderSide::Buy,
                qty,
                TimeInForce::Day,
                UUID4::new(),
                0.into(),
                false,
                false,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
            )
            .expect("market")
            .into()
        }
    }
    fn fixture() -> serde_json::Value {
        serde_json::from_str::<serde_json::Value>(include_str!(
            "../tests/fixtures/get_orderactivities_FinalFill_Placed_Market.json"
        ))
        .expect("fixture")["response"]
            .clone()
    }
    async fn run_history(value: serde_json::Value, stop: bool) -> (Vec<OrderEventAny>, bool) {
        let body = value.to_string();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let response: &'static str = Box::leak(response.into_boxed_str());
        let (base, _requests, task) = server(response, false).await;
        let http = Arc::new(Mutex::new(SaxoHttp::test_client(&base)));
        let tokens = Arc::new(Mutex::new(TokenManager::for_tests("token")));
        let order = order(stop);
        let orders: Orders = Arc::new(Mutex::new(HashMap::from([(
            order.client_order_id(),
            Tracked::new(order, Some("1001".into())),
        )])));
        let ids = Arc::new(Mutex::new(HashMap::new()));
        let mut emitter = ExecutionEventEmitter::new(
            get_atomic_clock_realtime(),
            TraderId::from("TESTER-001"),
            AccountId::from("SAXO-001"),
            AccountType::Margin,
            None,
        );
        let (tx, mut rx) = mpsc::unbounded_channel();
        emitter.set_sender(tx);
        poll(&http, &tokens, &orders, &ids, &emitter).await;
        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            if let ExecutionEvent::Order(event) = event {
                events.push(event);
            }
        }
        let retained = !orders.lock().await.is_empty();
        task.abort();
        (events, retained)
    }
    fn partial_then_final() -> serde_json::Value {
        let mut value = fixture();
        let rows = value["Data"].as_array_mut().expect("data");
        rows.remove(1);
        let mut partial = rows[1].clone();
        partial["LogId"] = "1004".into();
        partial["Status"] = "Fill".into();
        partial["FilledAmount"] = 400.into();
        partial["ActivityTime"] = "2030-01-02T14:19:35.849Z".into();
        rows.insert(1, partial);
        value
    }
    #[tokio::test]
    async fn partial_fill_then_final_books_only_increments() {
        let (events, retained) = run_history(partial_then_final(), false).await;
        assert!(!retained);
        assert_eq!(events.len(), 3);
        assert!(matches!(&events[0], OrderEventAny::Accepted(_)));
        match &events[1] {
            OrderEventAny::Filled(e) => {
                assert_eq!(e.last_qty, Quantity::from(400));
                assert_eq!(e.trade_id.to_string(), "1004");
            }
            other => panic!("{other:?}"),
        }
        match &events[2] {
            OrderEventAny::Filled(e) => {
                assert_eq!(e.last_qty, Quantity::from(600));
                assert_eq!(e.trade_id.to_string(), "1005");
            }
            other => panic!("{other:?}"),
        }
    }
    #[tokio::test]
    async fn cancellation_after_partial_fill_books_before_cancel() {
        let mut value = partial_then_final();
        let last = &mut value["Data"][2];
        last["Status"] = "Cancelled".into();
        last["FilledAmount"] = 400.into();
        let (events, retained) = run_history(value, false).await;
        assert!(!retained);
        assert_eq!(events.len(), 3);
        assert!(
            matches!(&events[1], OrderEventAny::Filled(e) if e.last_qty == Quantity::from(400))
        );
        assert!(matches!(&events[2], OrderEventAny::Canceled(_)));
    }
    #[tokio::test]
    async fn expired_and_done_for_day_emit_expired() {
        for status in ["Expired", "DoneForDay"] {
            let mut value = fixture();
            let rows = value["Data"].as_array_mut().expect("data");
            rows.truncate(1);
            rows[0]["Status"] = status.into();
            let (events, retained) = run_history(value, false).await;
            assert!(!retained);
            assert_eq!(events.len(), 2);
            assert!(matches!(&events[0], OrderEventAny::Accepted(_)));
            assert!(matches!(&events[1], OrderEventAny::Expired(_)));
        }
    }
    #[tokio::test]
    async fn changed_stop_uses_trigger_price_and_market_uses_price() {
        for stop in [true, false] {
            let mut value = fixture();
            let rows = value["Data"].as_array_mut().expect("data");
            rows.truncate(1);
            rows[0]["Status"] = "Changed".into();
            rows[0]["Price"] = 145.911.into();
            let (events, retained) = run_history(value, stop).await;
            assert!(retained);
            match &events[1] {
                OrderEventAny::Updated(e) => {
                    assert_eq!(e.trigger_price, stop.then_some(Price::from("145.911")));
                    assert_eq!(e.price, (!stop).then_some(Price::from("145.911")));
                }
                other => panic!("{other:?}"),
            }
        }
    }
    #[tokio::test]
    async fn final_fill_without_placed_accepts_first() {
        let mut value = fixture();
        let rows = value["Data"].as_array_mut().expect("data");
        rows.drain(0..2);
        let (events, retained) = run_history(value, false).await;
        assert!(!retained);
        assert_eq!(events.len(), 2);
        assert!(matches!(&events[0], OrderEventAny::Accepted(_)));
        assert!(
            matches!(&events[1], OrderEventAny::Filled(e) if e.last_qty == Quantity::from(1000))
        );
    }
    #[tokio::test]
    async fn declining_fill_stops_processing_and_keeps_order() {
        let mut value = partial_then_final();
        value["Data"][2]["FilledAmount"] = 300.into();
        let (events, retained) = run_history(value, false).await;
        assert!(retained);
        assert_eq!(events.len(), 2);
        assert!(
            matches!(&events[1], OrderEventAny::Filled(e) if e.last_qty == Quantity::from(400))
        );
    }
}
