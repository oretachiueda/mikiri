use crate::{
    auth::TokenManager,
    config::{SaxoDataClientConfig, SaxoExecutionClientConfig},
    env::Env,
    factories::{SaxoDataClientFactory, SaxoExecutionClientFactory},
    http::TEST_BASE,
    test_support::{fixture, server},
};
use nautilus_common::{
    actor::DataActor, enums::Environment, live::get_runtime, logging::logger::LoggerConfig,
};
use nautilus_core::UUID4;
use nautilus_live::{
    config::RoutingConfig,
    node::{LiveNode, LiveNodeHandle},
};
use nautilus_model::{
    data::QuoteTick,
    enums::{ContingencyType, OrderSide, TimeInForce, TriggerType},
    events::{OrderEventAny, PositionEvent},
    identifiers::{ClientId, ClientOrderId, InstrumentId, OrderListId, StrategyId, TraderId},
    orders::{MarketOrder, OrderAny, StopMarketOrder},
    types::{Price, Quantity},
};
use nautilus_trading::{Strategy, StrategyConfig, StrategyCore, nautilus_strategy};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::Mutex;

#[derive(Debug)]
struct TestStrategy {
    core: StrategyCore,
    handle: LiveNodeHandle,
    sent: bool,
    denied: Arc<AtomicBool>,
}
impl DataActor for TestStrategy {
    fn on_start(&mut self) -> anyhow::Result<()> {
        self.subscribe_quotes(
            InstrumentId::from("USD/JPY.SAXO"),
            Some(ClientId::from("SAXO")),
            None,
        );
        Ok(())
    }
    fn on_quote(&mut self, _: &QuoteTick) -> anyhow::Result<()> {
        if self.sent {
            return Ok(());
        }
        self.sent = true;
        let trader = self
            .trader_id()
            .ok_or_else(|| anyhow::anyhow!("missing trader"))?;
        let strategy = StrategyId::from("E2E-001");
        let instrument = InstrumentId::from("USD/JPY.SAXO");
        let entry = ClientOrderId::from("r79-S1-E");
        let stop = ClientOrderId::from("r79-S1-S");
        let list = OrderListId::from("e2e-list");
        let now = self.clock().timestamp_ns();
        let market: OrderAny = MarketOrder::new_checked(
            trader,
            strategy,
            instrument,
            entry,
            OrderSide::Buy,
            Quantity::from(1000),
            TimeInForce::Day,
            UUID4::new(),
            now,
            false,
            false,
            Some(ContingencyType::Oto),
            Some(list),
            Some(vec![stop]),
            None,
            None,
            None,
            None,
            None,
        )?
        .into();
        let stop_order: OrderAny = StopMarketOrder::new_checked(
            trader,
            strategy,
            instrument,
            stop,
            OrderSide::Sell,
            Quantity::from(1000),
            Price::from("146.300"),
            TriggerType::Default,
            TimeInForce::Day,
            None,
            false,
            false,
            None,
            None,
            None,
            None,
            Some(list),
            None,
            Some(entry),
            None,
            None,
            None,
            None,
            UUID4::new(),
            now,
        )?
        .into();
        self.submit_order_list(
            vec![market, stop_order],
            None,
            Some(ClientId::from("SAXO")),
            None,
        )
    }
}
nautilus_strategy!(TestStrategy, {
    fn on_order_event(&mut self, event: OrderEventAny) {
        if matches!(event, OrderEventAny::Denied(_) | OrderEventAny::Rejected(_)) {
            self.denied.store(true, Ordering::SeqCst);
            self.handle.stop();
        }
    }
    fn on_position_event(&mut self, _: PositionEvent) {
        self.handle.stop();
    }
});
#[test]
fn node_submits_and_books_fill_using_fake_saxo() {
    get_runtime().block_on(async {
        assert_eq!(
            fixture(include_str!(
                "../tests/fixtures/get_port_v1_balances_me.json"
            ))["Currency"],
            "EUR"
        );
        let (base, posts, server_task) = server().await;
        *TEST_BASE.lock().expect("lock") = Some(base);
        let tokens = Arc::new(Mutex::new(TokenManager::for_tests("synthetic")));
        let mut data = SaxoDataClientConfig::new(Env::Sim, tokens.clone());
        data.poll_interval = Duration::from_millis(100);
        let mut exec = SaxoExecutionClientConfig::new(Env::Sim, "TestAccountKey1==".into(), tokens);
        exec.poll_interval = Duration::from_millis(100);
        let routing = RoutingConfig::builder().default(true).build();
        let mut node = LiveNode::builder(TraderId::from("TESTER-001"), Environment::Live)
            .expect("builder")
            .with_reconciliation(true)
            .with_logging(LoggerConfig::from_spec("stdout=Warn;fileout=Off").expect("logging"))
            .add_data_client_with_routing(
                None,
                Box::new(SaxoDataClientFactory),
                Box::new(data),
                routing.clone(),
            )
            .expect("data")
            .add_exec_client_with_routing(
                None,
                Box::new(SaxoExecutionClientFactory),
                Box::new(exec),
                routing,
            )
            .expect("exec")
            .build()
            .expect("node");
        let handle = node.handle();
        let denied = Arc::new(AtomicBool::new(false));
        let config = StrategyConfig::builder()
            .strategy_id(StrategyId::from("E2E-001"))
            .manage_stop(false)
            .log_events(false)
            .log_commands(false)
            .build()
            .expect("config");
        node.add_strategy(TestStrategy {
            core: StrategyCore::new_checked(config).expect("core"),
            handle: handle.clone(),
            sent: false,
            denied: denied.clone(),
        })
        .expect("strategy");
        let stop = handle.clone();
        let timer = get_runtime().spawn(async move {
            tokio::time::sleep(Duration::from_secs(15)).await;
            stop.stop();
        });
        let result = tokio::time::timeout(Duration::from_secs(30), node.run()).await;
        timer.abort();
        let cache = node.kernel().cache();
        let cache = cache.borrow();
        let net = cache
            .positions_open(
                None,
                Some(&InstrumentId::from("USD/JPY.SAXO")),
                None,
                None,
                None,
            )
            .iter()
            .map(|p| p.signed_qty)
            .reduce(|a, b| a + b)
            .map_or_else(|| "0".to_owned(), |x| x.normalize().to_string());
        println!(
            "node={result:?} denied={} posts={} net={net}",
            denied.load(Ordering::SeqCst),
            posts.load(Ordering::SeqCst)
        );
        *TEST_BASE.lock().expect("lock") = None;
        server_task.abort();
        assert!(
            matches!(result, Ok(Ok(()))),
            "node failed or exceeded 30 seconds: {result:?}"
        );
        assert!(!denied.load(Ordering::SeqCst), "risk engine denied order");
        assert_eq!(posts.load(Ordering::SeqCst), 1);
        assert_eq!(net, "1000");
    });
}
