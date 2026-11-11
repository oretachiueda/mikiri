use std::{
    sync::{
        Arc, Mutex,
        mpsc::{Receiver, TryRecvError},
    },
    time::{Duration, Instant},
};

use guard::{Intent, Recorded, Side};
use nautilus_common::{actor::DataActor, timer::TimeEvent};
use nautilus_core::{UUID4, UnixNanos};
use nautilus_live::node::LiveNodeHandle;
use nautilus_model::{
    enums::{OrderSide, OrderStatus, TimeInForce},
    events::OrderEventAny,
    identifiers::{ClientId, ClientOrderId, InstrumentId, StrategyId, TraderId},
    orders::{MarketOrder, Order, OrderAny},
    types::Quantity,
};
use nautilus_trading::{Strategy, StrategyConfig, StrategyCore, nautilus_strategy};

use crate::cli::{Terminal, order_id, terminal_exit};

#[derive(Debug)]
struct Submitted {
    id: ClientOrderId,
    side: &'static str,
    shares: u32,
    position: i64,
    started: Instant,
}

#[derive(Debug)]
pub struct DemoStrategy {
    core: StrategyCore,
    receiver: Receiver<Recorded>,
    result: Arc<Mutex<Option<(String, u8)>>>,
    handle: LiveNodeHandle,
    account: String,
    instrument: InstrumentId,
    submitted: Option<Submitted>,
    finished: bool,
}

impl DemoStrategy {
    pub fn new(
        receiver: Receiver<Recorded>,
        result: Arc<Mutex<Option<(String, u8)>>>,
        handle: LiveNodeHandle,
        account: String,
    ) -> anyhow::Result<Self> {
        let config = StrategyConfig::builder()
            .strategy_id(StrategyId::new_checked("demo-001")?)
            .manage_stop(false)
            .log_events(false)
            .log_commands(false)
            .build()?;
        Ok(Self {
            core: StrategyCore::new_checked(config)?,
            receiver,
            result,
            handle,
            account,
            instrument: "SPY=STK.SMART".parse()?,
            submitted: None,
            finished: false,
        })
    }

    fn finish(&mut self, line: String, terminal: Terminal) {
        self.finished = true;
        let mut result = self
            .result
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if result.is_none() {
            *result = Some((line, terminal_exit(terminal)));
        }
        self.handle.stop();
    }

    fn finish_reason(&mut self, status: &str, reason: &str, terminal: Terminal) {
        let reason = crate::errors::mask(reason, Some(&self.account))
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let reason = if reason.is_empty() {
            "unavailable"
        } else {
            &reason
        };
        self.finish(format!("{status} reason={reason}"), terminal);
    }

    fn submit_recorded(&mut self, recorded: Recorded) -> anyhow::Result<()> {
        let intent = recorded.intent();
        anyhow::ensure!(
            intent.strategy() == "demo" && intent.symbol() == "SPY",
            "unexpected recorded intent"
        );
        let id = order_id(intent).map_err(anyhow::Error::msg)?;
        let id = ClientOrderId::new_checked(id)?;
        let current = self
            .cache()
            .positions_open(None, Some(&self.instrument), None, None, None)
            .iter()
            .try_fold(0_i64, |sum, position| {
                let shares = position.signed_qty.normalize().to_string().parse::<i64>()?;
                sum.checked_add(shares)
                    .ok_or_else(|| anyhow::anyhow!("position overflow"))
            })?;
        let delta = match intent.side() {
            Side::Buy => i64::from(intent.shares()),
            Side::Sell => -i64::from(intent.shares()),
        };
        let position = current
            .checked_add(delta)
            .ok_or_else(|| anyhow::anyhow!("position overflow"))?;
        let trader = self
            .trader_id()
            .ok_or_else(|| anyhow::anyhow!("strategy not registered"))?;
        let order = market_order(intent, id, trader, self.clock().timestamp_ns())?;
        self.submitted = Some(Submitted {
            id,
            side: match intent.side() {
                Side::Buy => "buy",
                Side::Sell => "sell",
            },
            shares: intent.shares(),
            position,
            started: Instant::now(),
        });
        self.submit_order(order, None, Some(ClientId::new("IB")), None)
    }
}

impl DataActor for DemoStrategy {
    fn on_start(&mut self) -> anyhow::Result<()> {
        self.subscribe_quotes(self.instrument, Some(ClientId::new("IB")), None);
        self.clock().set_timer(
            "paper-order-poll",
            Duration::from_millis(100),
            None,
            None,
            None,
            None,
            None,
        )
    }

    fn on_stop(&mut self) -> anyhow::Result<()> {
        self.unsubscribe_quotes(self.instrument, Some(ClientId::new("IB")), None);
        Ok(())
    }

    fn on_time_event(&mut self, _event: &TimeEvent) -> anyhow::Result<()> {
        if self.finished {
            return Ok(());
        }
        if let Some(submitted) = &self.submitted {
            if submitted.started.elapsed() >= Duration::from_secs(60) {
                self.finish(
                    format!("pending client_order_id={}", submitted.id),
                    Terminal::Timeout,
                );
            }
            return Ok(());
        }
        match self.receiver.try_recv() {
            Ok(recorded) => {
                if self.submit_recorded(recorded).is_err() {
                    self.finish(
                        "refused: order submission failed".into(),
                        Terminal::Rejected,
                    );
                }
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                self.finish("refused: preflight unavailable".into(), Terminal::Rejected);
            }
        }
        Ok(())
    }
}

nautilus_strategy!(DemoStrategy, {
    fn on_order_event(&mut self, event: OrderEventAny) {
        if self.finished {
            return;
        }
        let Some(submitted) = &self.submitted else {
            return;
        };
        if event.client_order_id() != submitted.id {
            return;
        }
        match event {
            OrderEventAny::Filled(_) => {
                let filled = self
                    .cache()
                    .order(&submitted.id)
                    .is_some_and(|order| order.status() == OrderStatus::Filled);
                if filled {
                    self.finish(
                        format!(
                            "filled side={} shares={} position={}",
                            submitted.side, submitted.shares, submitted.position
                        ),
                        Terminal::Filled,
                    );
                }
            }
            OrderEventAny::Rejected(event) => {
                self.finish_reason("rejected", event.reason.as_str(), Terminal::Rejected);
            }
            OrderEventAny::Denied(event) => {
                self.finish_reason("rejected", event.reason.as_str(), Terminal::Rejected);
            }
            OrderEventAny::Canceled(event) => {
                self.finish_reason(
                    "canceled",
                    event
                        .reason
                        .as_ref()
                        .map_or("canceled", |reason| reason.as_str()),
                    Terminal::Canceled,
                );
            }
            OrderEventAny::Expired(_) => {
                self.finish_reason("canceled", "expired", Terminal::Canceled);
            }
            _ => {}
        }
    }
});

fn market_order(
    intent: &Intent,
    id: ClientOrderId,
    trader: TraderId,
    now: UnixNanos,
) -> anyhow::Result<OrderAny> {
    Ok(MarketOrder::new_checked(
        trader,
        StrategyId::new_checked("demo-001")?,
        "SPY=STK.SMART".parse()?,
        id,
        match intent.side() {
            Side::Buy => OrderSide::Buy,
            Side::Sell => OrderSide::Sell,
        },
        Quantity::from(intent.shares()),
        TimeInForce::Day,
        UUID4::new(),
        now,
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
    )?
    .into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nautilus_model::{
        enums::OrderType,
        events::{OrderCanceled, OrderDenied, OrderExpired, OrderRejected},
        identifiers::AccountId,
    };

    #[test]
    fn terminal_events_report_masked_reasons_without_resubmitting() {
        let trader = TraderId::new("DEMO-001");
        let strategy_id = StrategyId::new("demo-001");
        let instrument = "SPY=STK.SMART".parse().unwrap();
        let id = ClientOrderId::new("2026-11-11-demo-SPY-buy");
        let event_id = UUID4::new();
        let now = UnixNanos::default();
        let reason = "201 account SYNTHETIC-ACCOUNT\nD123 U456";
        let cases = [
            (
                OrderEventAny::Rejected(OrderRejected::new(
                    trader,
                    strategy_id,
                    instrument,
                    id,
                    AccountId::new("IB-D123"),
                    reason.into(),
                    event_id,
                    now,
                    now,
                    false,
                    false,
                )),
                "rejected reason=201 account <account> <account> <account>",
            ),
            (
                OrderEventAny::Denied(OrderDenied::new(
                    trader,
                    strategy_id,
                    instrument,
                    id,
                    reason.into(),
                    event_id,
                    now,
                    now,
                )),
                "rejected reason=201 account <account> <account> <account>",
            ),
            (
                OrderEventAny::Canceled(OrderCanceled::new(
                    trader,
                    strategy_id,
                    instrument,
                    id,
                    event_id,
                    now,
                    now,
                    false,
                    None,
                    None,
                    Some(reason.into()),
                )),
                "canceled reason=201 account <account> <account> <account>",
            ),
            (
                OrderEventAny::Canceled(OrderCanceled::new(
                    trader,
                    strategy_id,
                    instrument,
                    id,
                    event_id,
                    now,
                    now,
                    false,
                    None,
                    None,
                    None,
                )),
                "canceled reason=canceled",
            ),
            (
                OrderEventAny::Expired(OrderExpired::new(
                    trader,
                    strategy_id,
                    instrument,
                    id,
                    event_id,
                    now,
                    now,
                    false,
                    None,
                    None,
                )),
                "canceled reason=expired",
            ),
            (
                OrderEventAny::Denied(OrderDenied::new(
                    trader,
                    strategy_id,
                    instrument,
                    id,
                    "\n \t".into(),
                    event_id,
                    now,
                    now,
                )),
                "rejected reason=unavailable",
            ),
        ];
        for (event, expected) in cases {
            let (_sender, receiver) = std::sync::mpsc::channel();
            let result = Arc::new(Mutex::new(None));
            let mut strategy = DemoStrategy::new(
                receiver,
                result.clone(),
                LiveNodeHandle::default(),
                "SYNTHETIC-ACCOUNT".into(),
            )
            .unwrap();
            strategy.submitted = Some(Submitted {
                id,
                side: "buy",
                shares: 1,
                position: 1,
                started: Instant::now(),
            });
            Strategy::on_order_event(&mut strategy, event.clone());
            Strategy::on_order_event(&mut strategy, event);
            assert_eq!(*result.lock().unwrap(), Some((expected.into(), 1)));
            assert!(strategy.finished);
            assert_eq!(strategy.submitted.as_ref().unwrap().id, id);
            assert!(matches!(
                strategy.receiver.try_recv(),
                Err(TryRecvError::Empty)
            ));
        }
    }

    #[test]
    fn timeout_leaves_order_pending_and_finishes_only_once() {
        let (_sender, receiver) = std::sync::mpsc::channel();
        let result = Arc::new(Mutex::new(None));
        let mut strategy = DemoStrategy::new(
            receiver,
            result.clone(),
            LiveNodeHandle::default(),
            "SYNTHETIC-ACCOUNT".into(),
        )
        .unwrap();
        assert!(!strategy.config().manage_stop);
        strategy.submitted = Some(Submitted {
            id: ClientOrderId::new("2026-11-11-demo-SPY-buy"),
            side: "buy",
            shares: 1,
            position: 1,
            started: Instant::now() - Duration::from_secs(61),
        });
        let event = TimeEvent::new(
            "paper-order-poll".into(),
            UUID4::new(),
            UnixNanos::default(),
            UnixNanos::default(),
        );
        DataActor::on_time_event(&mut strategy, &event).unwrap();
        DataActor::on_time_event(&mut strategy, &event).unwrap();
        assert_eq!(
            *result.lock().unwrap(),
            Some(("pending client_order_id=2026-11-11-demo-SPY-buy".into(), 1))
        );
        assert!(strategy.finished);
        assert!(strategy.submitted.is_some());
    }

    #[test]
    fn missing_preflight_never_submits() {
        let (sender, receiver) = std::sync::mpsc::channel();
        drop(sender);
        let result = Arc::new(Mutex::new(None));
        let mut strategy = DemoStrategy::new(
            receiver,
            result.clone(),
            LiveNodeHandle::default(),
            "SYNTHETIC-ACCOUNT".into(),
        )
        .unwrap();
        let event = TimeEvent::new(
            "paper-order-poll".into(),
            UUID4::new(),
            UnixNanos::default(),
            UnixNanos::default(),
        );
        DataActor::on_time_event(&mut strategy, &event).unwrap();
        assert!(strategy.submitted.is_none());
        assert_eq!(
            *result.lock().unwrap(),
            Some(("refused: preflight unavailable".into(), 1))
        );
    }

    #[test]
    fn market_order_preserves_side_quantity_day_and_client_id() {
        for (side, expected) in [(Side::Buy, OrderSide::Buy), (Side::Sell, OrderSide::Sell)] {
            let intent = Intent::new(
                time::Date::from_calendar_date(2026, time::Month::November, 11).unwrap(),
                "demo",
                "SPY",
                side,
                1,
            )
            .unwrap();
            let id = ClientOrderId::new("2026-11-11-demo-SPY-buy");
            let order =
                market_order(&intent, id, TraderId::new("DEMO-001"), UnixNanos::default()).unwrap();
            assert_eq!(order.client_order_id(), id);
            assert_eq!(order.order_side(), expected);
            assert_eq!(order.quantity(), Quantity::from(1_u32));
            assert_eq!(order.order_type(), OrderType::Market);
            assert_eq!(order.time_in_force(), TimeInForce::Day);
            assert_eq!(order.instrument_id().to_string(), "SPY=STK.SMART");
            assert!(order.tags().is_none());
        }
        assert!(!ibapi::orders::Order::default().outside_rth);
    }
}
