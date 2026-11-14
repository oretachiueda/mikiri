use std::{
    sync::{
        Arc, Mutex,
        mpsc::{Receiver, Sender, TryRecvError},
    },
    time::{Duration, Instant},
};

use guard::{Intent, Journal, NonFillOutcome, Recorded, Side};
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
    recorded: Recorded,
    had_fill: bool,
}

#[derive(Debug)]
pub struct DemoStrategy {
    core: StrategyCore,
    receiver: Receiver<Recorded>,
    quote_ready: Option<Sender<()>>,
    quote_wait_started: Instant,
    journal: Arc<Mutex<Journal>>,
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
        quote_ready: Sender<()>,
        journal: Arc<Mutex<Journal>>,
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
            quote_ready: Some(quote_ready),
            quote_wait_started: Instant::now(),
            journal,
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

    fn has_market_price(&self) -> bool {
        let cache = self.cache();
        cache.quote(&self.instrument).is_some() || cache.trade(&self.instrument).is_some()
    }

    fn finish_non_fill(&mut self, outcome: NonFillOutcome, reason: &str) {
        if let Some(submitted) = &self.submitted {
            let had_fill = submitted.had_fill
                || self
                    .cache()
                    .order(&submitted.id)
                    .is_some_and(|order| !order.filled_qty().is_zero());
            if !had_fill {
                let recorded = self
                    .journal
                    .lock()
                    .map_err(|_| anyhow::anyhow!("journal lock unavailable"))
                    .and_then(|mut journal| {
                        journal
                            .record_non_fill(&submitted.recorded, outcome)
                            .map_err(anyhow::Error::from)
                    });
                if let Err(error) = recorded {
                    eprintln!(
                        "{}",
                        crate::errors::mask(&format!("{error:#}"), Some(&self.account))
                    );
                    self.finish(
                        "refused: journal terminal outcome failed".into(),
                        Terminal::Rejected,
                    );
                    return;
                }
            }
        }
        self.finish_reason("rejected", reason, Terminal::Rejected);
    }

    fn submit_recorded(&mut self, recorded: Recorded) -> anyhow::Result<()> {
        anyhow::ensure!(self.has_market_price(), "no market quote");
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
            recorded,
            had_fill: false,
        });
        self.submit_order(order, None, Some(ClientId::new("IB")), None)
    }
}

impl DataActor for DemoStrategy {
    fn on_start(&mut self) -> anyhow::Result<()> {
        self.quote_wait_started = Instant::now();
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
        if !self.has_market_price() {
            if self.quote_wait_started.elapsed() >= Duration::from_secs(30) {
                self.finish("refused: no market quote".into(), Terminal::Rejected);
            }
            return Ok(());
        }
        if let Some(ready) = self.quote_ready.take()
            && ready.send(()).is_err()
        {
            self.finish("refused: preflight unavailable".into(), Terminal::Rejected);
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
                if let Some(submitted) = &mut self.submitted {
                    submitted.had_fill = true;
                }
                let Some(submitted) = &self.submitted else {
                    return;
                };
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
                self.finish_non_fill(NonFillOutcome::Rejected, event.reason.as_str());
            }
            OrderEventAny::Denied(event) => {
                self.finish_non_fill(NonFillOutcome::Denied, event.reason.as_str());
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

    fn empty_journal() -> (Arc<Mutex<guard::Journal>>, std::path::PathBuf) {
        let directory = std::env::temp_dir().join(format!("mikiri-execution-{}", UUID4::new()));
        std::fs::create_dir(&directory).unwrap();
        let config = directory.join("limits.toml");
        std::fs::write(&config, "expires_on = \"2026-11-30\"\nsymbols = [\"SPY\"]\nmax_position_shares = 1\nmax_daily_orders = 4\nmax_daily_loss_usd = 50.0\n").unwrap();
        let journal = Journal::open(directory.join("journal")).unwrap();
        (Arc::new(Mutex::new(journal)), directory)
    }

    fn recorded() -> (Recorded, Arc<Mutex<guard::Journal>>, std::path::PathBuf) {
        let (journal, directory) = empty_journal();
        let config = directory.join("limits.toml");
        let day = time::macros::date!(2026 - 11 - 11);
        let limits = guard::Limits::load(&config, day).unwrap();
        let intent = Intent::new(day, "demo", "SPY", Side::Buy, 1).unwrap();
        let approved = guard::check(
            &limits,
            &guard::Account {
                position: 0,
                daily_pnl_usd: 0.0,
            },
            0,
            &intent,
        )
        .unwrap();
        let recorded = journal.lock().unwrap().record(approved).unwrap();
        (recorded, journal, directory)
    }

    fn register_cache(
        strategy: &mut DemoStrategy,
    ) -> std::rc::Rc<std::cell::RefCell<nautilus_common::cache::Cache>> {
        let cache = std::rc::Rc::new(std::cell::RefCell::new(
            nautilus_common::cache::Cache::default(),
        ));
        nautilus_common::actor::DataActorNative::core_mut(strategy)
            .register(
                TraderId::new("DEMO-001"),
                std::rc::Rc::new(std::cell::RefCell::new(
                    nautilus_common::clock::VirtualClock::new(),
                )),
                cache.clone(),
            )
            .unwrap();
        cache
    }

    fn new_strategy(
        receiver: Receiver<Recorded>,
        result: Arc<Mutex<Option<(String, u8)>>>,
        journal: Arc<Mutex<Journal>>,
    ) -> (DemoStrategy, Receiver<()>) {
        let (ready, receiver_ready) = std::sync::mpsc::channel();
        let strategy = DemoStrategy::new(
            receiver,
            ready,
            journal,
            result,
            LiveNodeHandle::default(),
            "SYNTHETIC-ACCOUNT".into(),
        )
        .unwrap();
        (strategy, receiver_ready)
    }

    fn quote(instrument: &str) -> nautilus_model::data::QuoteTick {
        nautilus_model::data::QuoteTick::new(
            instrument.parse().unwrap(),
            nautilus_model::types::Price::from("100.00"),
            nautilus_model::types::Price::from("100.01"),
            Quantity::from(1_u32),
            Quantity::from(1_u32),
            UnixNanos::default(),
            UnixNanos::default(),
        )
    }

    #[test]
    fn recorded_preflight_without_quote_never_submits() {
        let (recorded, journal, directory) = recorded();
        let (sender, receiver) = std::sync::mpsc::channel();
        sender.send(recorded).unwrap();
        let result = Arc::new(Mutex::new(None));
        let (mut strategy, _ready) = new_strategy(receiver, result.clone(), journal.clone());
        register_cache(&mut strategy);
        let event = TimeEvent::new(
            "paper-order-poll".into(),
            UUID4::new(),
            UnixNanos::default(),
            UnixNanos::default(),
        );
        DataActor::on_time_event(&mut strategy, &event).unwrap();
        assert!(
            strategy.submitted.is_none(),
            "a recorded intent must wait for a cached market price"
        );
        assert!(result.lock().unwrap().is_none());
        drop(strategy);
        drop(journal);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn quote_timeout_keeps_journal_empty_and_other_instruments_do_not_unlock() {
        let (journal, directory) = empty_journal();
        let (_sender, receiver) = std::sync::mpsc::channel();
        let result = Arc::new(Mutex::new(None));
        let (mut strategy, ready) = new_strategy(receiver, result.clone(), journal.clone());
        let cache = register_cache(&mut strategy);
        cache
            .borrow_mut()
            .add_quote(quote("QQQ=STK.SMART"))
            .unwrap();
        let event = TimeEvent::new(
            "paper-order-poll".into(),
            UUID4::new(),
            UnixNanos::default(),
            UnixNanos::default(),
        );
        DataActor::on_time_event(&mut strategy, &event).unwrap();
        assert!(matches!(ready.try_recv(), Err(TryRecvError::Empty)));
        assert!(result.lock().unwrap().is_none());
        strategy.quote_wait_started = Instant::now() - Duration::from_secs(30);
        DataActor::on_time_event(&mut strategy, &event).unwrap();
        assert_eq!(
            *result.lock().unwrap(),
            Some(("refused: no market quote".into(), 1))
        );
        assert!(strategy.submitted.is_none());
        assert_eq!(std::fs::read(directory.join("journal")).unwrap(), b"");
        println!(
            "quote timeout: refused: no market quote; exit=1; submitted=false; journal_rows=0"
        );
        drop(strategy);
        drop(journal);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn matching_quote_unlocks_preflight_once() {
        let (journal, directory) = empty_journal();
        let (_sender, receiver) = std::sync::mpsc::channel();
        let result = Arc::new(Mutex::new(None));
        let (mut strategy, ready) = new_strategy(receiver, result.clone(), journal.clone());
        let cache = register_cache(&mut strategy);
        cache
            .borrow_mut()
            .add_quote(quote("SPY=STK.SMART"))
            .unwrap();
        let event = TimeEvent::new(
            "paper-order-poll".into(),
            UUID4::new(),
            UnixNanos::default(),
            UnixNanos::default(),
        );
        DataActor::on_time_event(&mut strategy, &event).unwrap();
        assert_eq!(ready.try_recv(), Ok(()));
        DataActor::on_time_event(&mut strategy, &event).unwrap();
        assert!(matches!(ready.try_recv(), Err(TryRecvError::Disconnected)));
        assert!(strategy.submitted.is_none());
        assert!(result.lock().unwrap().is_none());
        assert_eq!(std::fs::read(directory.join("journal")).unwrap(), b"");
        drop(strategy);
        drop(journal);
        std::fs::remove_dir_all(directory).unwrap();
    }

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
            let (recorded, journal, directory) = recorded();
            let (mut strategy, _ready) = new_strategy(receiver, result.clone(), journal.clone());
            register_cache(&mut strategy);
            strategy.submitted = Some(Submitted {
                id,
                side: "buy",
                shares: 1,
                position: 1,
                started: Instant::now(),
                recorded,
                had_fill: false,
            });
            let non_fill = matches!(
                &event,
                OrderEventAny::Rejected(_) | OrderEventAny::Denied(_)
            );
            Strategy::on_order_event(&mut strategy, event.clone());
            Strategy::on_order_event(&mut strategy, event);
            assert_eq!(*result.lock().unwrap(), Some((expected.into(), 1)));
            assert!(strategy.finished);
            assert_eq!(strategy.submitted.as_ref().unwrap().id, id);
            assert!(matches!(
                strategy.receiver.try_recv(),
                Err(TryRecvError::Empty)
            ));
            drop(strategy);
            drop(journal);
            let mut journal = Journal::open(directory.join("journal")).unwrap();
            assert_eq!(
                journal.net_shares("demo", "SPY").unwrap(),
                if non_fill { 0 } else { 1 }
            );
            assert_eq!(journal.orders_on(time::macros::date!(2026 - 11 - 11)), 1);
            let lines = std::fs::read_to_string(directory.join("journal")).unwrap();
            assert_eq!(lines.lines().count(), if non_fill { 2 } else { 1 });
            if non_fill {
                let line = crate::broker::Snapshot {
                    position: 0,
                    open_orders: 0,
                    current_equity_usd: Ok(100.0),
                }
                .status_line(journal.net_shares("demo", "SPY").unwrap(), Some(0.0));
                assert_eq!(
                    line,
                    "status account=paper position=0 journal_net=0 open_orders=0 match=true daily_pnl_usd=0.00"
                );
                println!("terminal non-fill replay: {line}; journal_rows=2; key_consumed=true");
                let limits = guard::Limits::load(
                    directory.join("limits.toml"),
                    time::macros::date!(2026 - 11 - 11),
                )
                .unwrap();
                let intent = Intent::new(
                    time::macros::date!(2026 - 11 - 11),
                    "demo",
                    "SPY",
                    Side::Buy,
                    1,
                )
                .unwrap();
                let approved = guard::check(
                    &limits,
                    &guard::Account {
                        position: 0,
                        daily_pnl_usd: 0.0,
                    },
                    1,
                    &intent,
                )
                .unwrap();
                assert!(matches!(
                    journal.record(approved),
                    Err(guard::JournalError::Duplicate)
                ));
            }
            drop(journal);
            std::fs::remove_dir_all(directory).unwrap();
        }
    }

    #[test]
    fn rejection_after_partial_fill_preserves_unresolved_journal() {
        use nautilus_model::{
            enums::LiquiditySide,
            events::OrderFilled,
            identifiers::{TradeId, VenueOrderId},
            types::{Currency, Price},
        };
        for cached_fill in [false, true] {
            let (recorded, journal, directory) = recorded();
            let (_sender, receiver) = std::sync::mpsc::channel();
            let result = Arc::new(Mutex::new(None));
            let (mut strategy, _ready) = new_strategy(receiver, result.clone(), journal.clone());
            let cache = register_cache(&mut strategy);
            let id = ClientOrderId::new("2026-11-11-demo-SPY-buy");
            let fill = OrderEventAny::Filled(OrderFilled::new(
                TraderId::new("DEMO-001"),
                StrategyId::new("demo-001"),
                strategy.instrument,
                id,
                VenueOrderId::new("synthetic-order"),
                AccountId::new("IB-D123"),
                TradeId::new("synthetic-fill"),
                OrderSide::Buy,
                OrderType::Market,
                Quantity::from("0.5"),
                Price::from("100.00"),
                Currency::USD(),
                LiquiditySide::Taker,
                UUID4::new(),
                UnixNanos::default(),
                UnixNanos::default(),
                false,
                None,
                None,
                None,
            ));
            if cached_fill {
                let mut order = market_order(
                    recorded.intent(),
                    id,
                    TraderId::new("DEMO-001"),
                    UnixNanos::default(),
                )
                .unwrap();
                order
                    .apply(OrderEventAny::Submitted(
                        nautilus_model::events::OrderSubmitted::new(
                            TraderId::new("DEMO-001"),
                            StrategyId::new("demo-001"),
                            strategy.instrument,
                            id,
                            AccountId::new("IB-D123"),
                            UUID4::new(),
                            UnixNanos::default(),
                            UnixNanos::default(),
                        ),
                    ))
                    .unwrap();
                order.apply(fill.clone()).unwrap();
                cache
                    .borrow_mut()
                    .add_order(order, None, None, false)
                    .unwrap();
            }
            strategy.submitted = Some(Submitted {
                id,
                side: "buy",
                shares: 1,
                position: 1,
                started: Instant::now(),
                recorded,
                had_fill: false,
            });
            if !cached_fill {
                Strategy::on_order_event(&mut strategy, fill);
                assert!(!strategy.finished);
            }
            Strategy::on_order_event(
                &mut strategy,
                OrderEventAny::Rejected(OrderRejected::new(
                    TraderId::new("DEMO-001"),
                    StrategyId::new("demo-001"),
                    "SPY=STK.SMART".parse().unwrap(),
                    id,
                    AccountId::new("IB-D123"),
                    "synthetic reject".into(),
                    UUID4::new(),
                    UnixNanos::default(),
                    UnixNanos::default(),
                    false,
                    false,
                )),
            );
            assert_eq!(
                *result.lock().unwrap(),
                Some(("rejected reason=synthetic reject".into(), 1))
            );
            assert_eq!(
                journal.lock().unwrap().net_shares("demo", "SPY").unwrap(),
                1
            );
            assert_eq!(
                std::fs::read_to_string(directory.join("journal"))
                    .unwrap()
                    .lines()
                    .count(),
                1
            );
            drop(strategy);
            drop(journal);
            let journal = Journal::open(directory.join("journal")).unwrap();
            assert_eq!(journal.net_shares("demo", "SPY").unwrap(), 1);
            drop(journal);
            std::fs::remove_dir_all(directory).unwrap();
        }
    }

    #[test]
    fn timeout_leaves_order_pending_and_finishes_only_once() {
        let (recorded, journal, directory) = recorded();
        let (_sender, receiver) = std::sync::mpsc::channel();
        let result = Arc::new(Mutex::new(None));
        let (mut strategy, _ready) = new_strategy(receiver, result.clone(), journal.clone());
        assert!(!strategy.config().manage_stop);
        strategy.submitted = Some(Submitted {
            id: ClientOrderId::new("2026-11-11-demo-SPY-buy"),
            side: "buy",
            shares: 1,
            position: 1,
            started: Instant::now() - Duration::from_secs(61),
            recorded,
            had_fill: false,
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
        assert_eq!(
            journal.lock().unwrap().net_shares("demo", "SPY").unwrap(),
            1
        );
        drop(strategy);
        drop(journal);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn missing_preflight_never_submits() {
        let (_recorded, journal, directory) = recorded();
        let (sender, receiver) = std::sync::mpsc::channel();
        drop(sender);
        let result = Arc::new(Mutex::new(None));
        let (mut strategy, _ready) = new_strategy(receiver, result.clone(), journal.clone());
        register_cache(&mut strategy)
            .borrow_mut()
            .add_quote(quote("SPY=STK.SMART"))
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
        drop(strategy);
        drop(journal);
        std::fs::remove_dir_all(directory).unwrap();
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
