use crate::{
    auth::TokenManager,
    config::SaxoExecutionClientConfig,
    http::{SaxoHttp, WriteOutcome},
    parse, reports,
    request::{self, Side},
    tracker::{self, Orders, Tracked},
};
use nautilus_common::{
    clients::ExecutionClient,
    messages::execution::{
        CancelOrder, GenerateFillReports, GenerateOrderStatusReports,
        GeneratePositionStatusReports, ModifyOrder, QueryAccount, SubmitOrder, SubmitOrderList,
    },
};
use nautilus_core::{Params, UnixNanos, time::get_atomic_clock_realtime};
use nautilus_live::{ExecutionClientCore, execution::emitter::ExecutionEventEmitter};
use nautilus_model::{
    accounts::AccountAny,
    enums::{ContingencyType, OmsType, OrderSide, OrderType, TimeInForce},
    identifiers::{AccountId, ClientId, ClientOrderId, InstrumentId, Venue},
    orders::{Order, OrderAny},
    reports::{FillReport, OrderStatusReport, PositionStatusReport},
    types::{AccountBalance, MarginBalance},
};
use nautilus_network::http::Method;
use std::{collections::HashMap, sync::Arc};
use tokio::sync::Mutex;

pub struct SaxoExecutionClient {
    core: ExecutionClientCore,
    pub emitter: ExecutionEventEmitter,
    config: SaxoExecutionClientConfig,
    http: Arc<Mutex<SaxoHttp>>,
    pub order_ids: Arc<Mutex<HashMap<ClientOrderId, String>>>,
    tracked: Orders,
    poll_task: Option<tokio::task::JoinHandle<()>>,
}

impl SaxoExecutionClient {
    pub fn new(
        core: ExecutionClientCore,
        config: SaxoExecutionClientConfig,
    ) -> anyhow::Result<Self> {
        let http = SaxoHttp::new(config.env)?;
        Ok(Self::build(core, config, http))
    }
    fn build(core: ExecutionClientCore, config: SaxoExecutionClientConfig, http: SaxoHttp) -> Self {
        let emitter = ExecutionEventEmitter::new(
            get_atomic_clock_realtime(),
            core.trader_id,
            core.account_id,
            core.account_type,
            core.base_currency,
        );
        Self {
            core,
            emitter,
            config,
            http: Arc::new(Mutex::new(http)),
            order_ids: Arc::new(Mutex::new(HashMap::new())),
            tracked: Arc::new(Mutex::new(HashMap::new())),
            poll_task: None,
        }
    }
    #[cfg(test)]
    pub(crate) fn with_http(
        core: ExecutionClientCore,
        config: SaxoExecutionClientConfig,
        http: SaxoHttp,
    ) -> Self {
        Self::build(core, config, http)
    }
    async fn get_json(&self, path: &str) -> anyhow::Result<serde_json::Value> {
        let reply = self
            .http
            .lock()
            .await
            .get(&mut *self.config.tokens.lock().await, path)
            .await?;
        anyhow::ensure!(
            reply.status == 200,
            "Saxo request failed with status {}",
            reply.status
        );
        Ok(serde_json::from_slice(&reply.body)?)
    }
    fn valid(order: &OrderAny) -> anyhow::Result<(Side, u64)> {
        anyhow::ensure!(
            order.instrument_id() == InstrumentId::from("USD/JPY.SAXO"),
            "unsupported instrument"
        );
        anyhow::ensure!(
            !order.is_reduce_only() && !order.is_quote_quantity(),
            "unsupported order flags"
        );
        anyhow::ensure!(
            order.time_in_force() == TimeInForce::Day,
            "unsupported time in force"
        );
        let side = match order.order_side() {
            OrderSide::Buy => Side::Buy,
            OrderSide::Sell => Side::Sell,
        };
        let q = order.quantity().as_f64();
        anyhow::ensure!(
            q >= 1000.0 && q.fract() == 0.0 && q < u64::MAX as f64,
            "invalid quantity"
        );
        Ok((side, q as u64))
    }
    fn submit(&self, orders: Vec<OrderAny>, body: serde_json::Value) -> anyhow::Result<()> {
        anyhow::ensure!(self.core.is_connected(), "client is not connected");
        let handle = tokio::runtime::Handle::try_current()?;
        let http = self.http.clone();
        let tokens = self.config.tokens.clone();
        let emitter = self.emitter.clone();
        let ids = self.order_ids.clone();
        let tracked = self.tracked.clone();
        let reference = orders[0].client_order_id().to_string();
        handle.spawn(async move {
            for order in &orders {
                emitter.emit_order_submitted(order);
            }
            let result = http
                .lock()
                .await
                .write(
                    &mut *tokens.lock().await,
                    Method::POST,
                    "/trade/v2/orders",
                    body.to_string().into(),
                    &reference,
                )
                .await;
            match result {
                Ok(WriteOutcome::Answered { status, body }) if (200..300).contains(&status) => {
                    let parsed = std::str::from_utf8(&body)
                        .ok()
                        .and_then(|s| parse::order_ids(s).ok());
                    let mut venue_ids = Vec::new();
                    if let Some(parsed) = parsed {
                        venue_ids.push(parsed.order_id);
                        venue_ids.extend(parsed.related);
                    }
                    let mut map = tracked.lock().await;
                    let mut id_map = ids.lock().await;
                    for (i, order) in orders.into_iter().enumerate() {
                        let id = venue_ids
                            .get(i)
                            .filter(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()))
                            .cloned();
                        if let Some(id) = &id {
                            id_map.insert(order.client_order_id(), id.clone());
                        }
                        map.insert(order.client_order_id(), Tracked::new(order, id));
                    }
                }
                Ok(WriteOutcome::Unknown) | Ok(WriteOutcome::Answered { status: 409, .. }) => {
                    let mut map = tracked.lock().await;
                    for order in orders {
                        map.insert(order.client_order_id(), Tracked::new(order, None));
                    }
                }
                Ok(WriteOutcome::Answered { status, .. }) if status != 409 => {
                    for order in &orders {
                        emitter.emit_order_rejected(
                            order,
                            "Saxo rejected order",
                            get_atomic_clock_realtime().get_time_ns(),
                            false,
                        );
                    }
                }
                Err(e) => {
                    log::error!("Saxo order was not sent: {e}");
                    for order in &orders {
                        emitter.emit_order_rejected(
                            order,
                            "Saxo request could not be sent",
                            get_atomic_clock_realtime().get_time_ns(),
                            false,
                        );
                    }
                }
                _ => {}
            }
        });
        Ok(())
    }
}

async fn report_balance(
    http: &SaxoHttp,
    tokens: &mut TokenManager,
    emitter: &ExecutionEventEmitter,
) -> anyhow::Result<()> {
    let reply = http
        .get(tokens, "/port/v1/balances/me")
        .await
        .map_err(|e| anyhow::anyhow!("balance request failed: {e}"))?;
    anyhow::ensure!(
        reply.status == 200,
        "balance request failed: HTTP {}",
        reply.status
    );
    let value = serde_json::from_slice(&reply.body)
        .map_err(|e| anyhow::anyhow!("invalid balance response: {e}"))?;
    let (balance, margin, info) = reports::balances(&value)
        .map_err(|e| anyhow::anyhow!("invalid balance response: {e:#}"))?;
    emitter
        .try_emit_account_state(
            vec![balance],
            vec![margin],
            true,
            get_atomic_clock_realtime().get_time_ns(),
            Some(info),
        )
        .map_err(|e| anyhow::anyhow!("account state delivery failed: {e}"))?;
    Ok(())
}

#[async_trait::async_trait(?Send)]
impl ExecutionClient for SaxoExecutionClient {
    fn is_connected(&self) -> bool {
        self.core.is_connected()
    }
    fn client_id(&self) -> ClientId {
        self.core.client_id
    }
    fn account_id(&self) -> AccountId {
        self.core.account_id
    }
    fn venue(&self) -> Venue {
        self.core.venue
    }
    fn oms_type(&self) -> OmsType {
        OmsType::Netting
    }
    fn get_account(&self) -> Option<AccountAny> {
        None
    }
    fn retain_unresolved_submissions(&self) -> bool {
        true
    }
    fn requires_authoritative_fills(&self) -> bool {
        true
    }
    fn provides_bulk_position_coverage(&self, id: InstrumentId) -> bool {
        id == InstrumentId::from("USD/JPY.SAXO")
    }
    fn generate_account_state(
        &self,
        balances: Vec<AccountBalance>,
        margins: Vec<MarginBalance>,
        reported: bool,
        ts_event: UnixNanos,
        info: Option<Params>,
    ) -> anyhow::Result<()> {
        self.emitter
            .try_emit_account_state(balances, margins, reported, ts_event, info)
    }
    fn start(&mut self) -> anyhow::Result<()> {
        let sender = nautilus_common::live::runner::try_get_exec_event_sender()
            .ok_or_else(|| anyhow::anyhow!("execution event sender is not installed"))?;
        self.emitter.set_sender(sender);
        Ok(())
    }
    fn stop(&mut self) -> anyhow::Result<()> {
        if let Some(task) = self.poll_task.take() {
            task.abort();
        }
        self.core.set_disconnected();
        Ok(())
    }
    async fn connect(&mut self) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.config.poll_interval.is_zero(),
            "poll interval must be positive"
        );
        if self.poll_task.is_some() {
            return Ok(());
        }
        let http = self.http.lock().await;
        let mut tokens = self.config.tokens.lock().await;
        let client = http.get(&mut tokens, "/port/v1/clients/me").await?;
        let instrument = http
            .get(&mut tokens, "/ref/v1/instruments/details/42/FxSpot")
            .await?;
        let accounts = http.get(&mut tokens, "/port/v1/accounts/me").await?;
        anyhow::ensure!(
            client.status == 200 && instrument.status == 200 && accounts.status == 200,
            "connection checks failed"
        );
        crate::account::check(
            std::str::from_utf8(&accounts.body)?,
            &self.config.account_key,
            self.config.env,
            std::str::from_utf8(&client.body)?,
            std::str::from_utf8(&instrument.body)?,
        )
        .map_err(|e| anyhow::anyhow!("connection checks failed: {e}"))?;
        report_balance(&http, &mut tokens, &self.emitter).await?;
        drop(http);
        drop(tokens);
        let task = tokio::runtime::Handle::try_current()?.spawn(tracker::run(
            self.http.clone(),
            self.config.tokens.clone(),
            self.tracked.clone(),
            self.order_ids.clone(),
            self.emitter.clone(),
            self.config.poll_interval,
        ));
        self.poll_task = Some(task);
        self.core.set_connected();
        Ok(())
    }
    async fn disconnect(&mut self) -> anyhow::Result<()> {
        self.stop()
    }
    async fn generate_order_status_reports(
        &self,
        cmd: &GenerateOrderStatusReports,
    ) -> anyhow::Result<Vec<OrderStatusReport>> {
        if cmd
            .instrument_id
            .is_some_and(|id| id != InstrumentId::from("USD/JPY.SAXO"))
        {
            return Ok(Vec::new());
        }
        let value = self.get_json("/port/v1/orders/me").await?;
        let mut reports = reports::open_orders(&value, self.core.account_id, cmd.ts_init)?;
        let tracked: Vec<_> = self
            .tracked
            .lock()
            .await
            .values()
            .filter_map(|t| t.venue_id.as_ref().map(|id| (t.order.clone(), id.clone())))
            .collect();
        for (order, id) in tracked {
            anyhow::ensure!(
                !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()),
                "invalid order id"
            );
            let history = self
                .get_json(&format!(
                    "/cs/v1/audit/orderactivities?OrderId={id}&EntryType=All"
                ))
                .await?;
            if let Some(report) =
                reports::history_order(&history, &order, &id, self.core.account_id, cmd.ts_init)?
            {
                if let Some(open) = reports.iter_mut().find(|r| r.venue_order_id.as_ref() == id) {
                    open.filled_qty = report.filled_qty;
                    open.ts_last = report.ts_last;
                    if report.filled_qty.as_f64() > 0.0 {
                        open.order_status = nautilus_model::enums::OrderStatus::PartiallyFilled;
                    }
                } else if !cmd.open_only {
                    reports.push(report);
                }
            }
        }
        reports.retain(|report| {
            cmd.start.is_none_or(|v| report.ts_last >= v)
                && cmd.end.is_none_or(|v| report.ts_last <= v)
        });
        Ok(reports)
    }
    async fn generate_fill_reports(
        &self,
        cmd: GenerateFillReports,
    ) -> anyhow::Result<Vec<FillReport>> {
        if cmd
            .instrument_id
            .is_some_and(|id| id != InstrumentId::from("USD/JPY.SAXO"))
        {
            return Ok(Vec::new());
        }
        let mut path = "/cs/v1/audit/orderactivities?EntryType=All".to_owned();
        for (name, ts) in [("FromDateTime", cmd.start), ("ToDateTime", cmd.end)] {
            if let Some(ts) = ts {
                let dt = time::OffsetDateTime::from_unix_timestamp_nanos(i128::from(ts.as_u64()))?;
                let date = dt
                    .format(&time::format_description::well_known::Rfc3339)?
                    .replace(':', "%3A");
                path.push_str(&format!("&{name}={date}"));
            }
        }
        let value = self.get_json(&path).await?;
        reports::fills(
            &value,
            self.core.account_id,
            cmd.ts_init,
            cmd.start,
            cmd.end,
            cmd.venue_order_id,
        )
    }
    async fn generate_position_status_reports(
        &self,
        cmd: &GeneratePositionStatusReports,
    ) -> anyhow::Result<Vec<PositionStatusReport>> {
        if cmd
            .instrument_id
            .is_some_and(|id| id != InstrumentId::from("USD/JPY.SAXO"))
        {
            return Ok(Vec::new());
        }
        let value = self.get_json("/port/v1/positions/me").await?;
        Ok(vec![reports::positions(
            &value,
            self.core.account_id,
            cmd.ts_init,
        )?])
    }
    fn query_account(&self, _cmd: QueryAccount) -> anyhow::Result<()> {
        let http = self.http.clone();
        let tokens = self.config.tokens.clone();
        let emitter = self.emitter.clone();
        tokio::runtime::Handle::try_current()?.spawn(async move {
            let result =
                report_balance(&*http.lock().await, &mut *tokens.lock().await, &emitter).await;
            if let Err(error) = result {
                log::error!("Saxo account query failed: {error}");
            }
        });
        Ok(())
    }
    fn submit_order(&self, cmd: SubmitOrder) -> anyhow::Result<()> {
        let order = self.core.get_order(&cmd.client_order_id)?;
        let result = (|| {
            let (side, units) = Self::valid(&order)?;
            anyhow::ensure!(
                order.contingency_type().is_none(),
                "contingent order requires a list"
            );
            let id = order.client_order_id().to_string();
            Ok::<_, anyhow::Error>(match order.order_type() {
                OrderType::Market => request::market(&self.config.account_key, side, units, &id)?,
                OrderType::StopMarket => request::stop(
                    &self.config.account_key,
                    side,
                    units,
                    order
                        .trigger_price()
                        .ok_or_else(|| anyhow::anyhow!("missing trigger"))?
                        .as_f64(),
                    &id,
                )?,
                _ => anyhow::bail!("unsupported order type"),
            })
        })();
        match result {
            Ok(body) => self.submit(vec![order], body),
            Err(e) => {
                self.emitter.emit_order_denied(&order, &e.to_string());
                Ok(())
            }
        }
    }
    fn submit_order_list(&self, cmd: SubmitOrderList) -> anyhow::Result<()> {
        let orders = self.core.get_orders_for_list(&cmd.order_list)?;
        let result = (|| {
            anyhow::ensure!(orders.len() == 2, "expected entry and stop");
            let (entry, stop) = (&orders[0], &orders[1]);
            let (side, units) = Self::valid(entry)?;
            let (stop_side, stop_units) = Self::valid(stop)?;
            anyhow::ensure!(
                entry.order_type() == OrderType::Market
                    && stop.order_type() == OrderType::StopMarket
                    && entry.contingency_type() == Some(ContingencyType::Oto)
                    && stop.parent_order_id() == Some(entry.client_order_id())
                    && entry.linked_order_ids() == Some(&[stop.client_order_id()][..])
                    && units == stop_units
                    && side != stop_side,
                "invalid order list"
            );
            Ok::<_, anyhow::Error>(request::open_with_stop(
                &self.config.account_key,
                side,
                units,
                stop.trigger_price()
                    .ok_or_else(|| anyhow::anyhow!("missing trigger"))?
                    .as_f64(),
                (
                    entry.client_order_id().as_ref(),
                    stop.client_order_id().as_ref(),
                ),
            )?)
        })();
        match result {
            Ok(body) => self.submit(orders, body),
            Err(e) => {
                for order in &orders {
                    self.emitter.emit_order_denied(order, &e.to_string());
                }
                Ok(())
            }
        }
    }
    fn modify_order(&self, cmd: ModifyOrder) -> anyhow::Result<()> {
        let order = self.core.get_order(&cmd.client_order_id)?;
        let input = (|| {
            anyhow::ensure!(self.core.is_connected(), "not connected");
            anyhow::ensure!(
                order.order_type() == OrderType::StopMarket
                    && cmd.quantity.is_none()
                    && cmd.price.is_none(),
                "only stop trigger changes are supported"
            );
            let (side, units) = Self::valid(&order)?;
            let trigger = cmd
                .trigger_price
                .ok_or_else(|| anyhow::anyhow!("missing trigger"))?;
            let id = cmd
                .venue_order_id
                .ok_or_else(|| anyhow::anyhow!("missing venue order id"))?;
            let reference = cmd.client_order_id.to_string();
            let body = request::move_stop(
                &self.config.account_key,
                id.as_ref(),
                side,
                units,
                trigger.as_f64(),
                &reference,
            )?;
            Ok::<_, anyhow::Error>((id, reference, body))
        })();
        let (id, reference, body) = match input {
            Ok(v) => v,
            Err(e) => {
                self.emitter.emit_order_modify_denied(
                    &order,
                    &e.to_string(),
                    get_atomic_clock_realtime().get_time_ns(),
                    cmd.causation_id,
                );
                return Ok(());
            }
        };
        let http = self.http.clone();
        let tokens = self.config.tokens.clone();
        let emitter = self.emitter.clone();
        tokio::runtime::Handle::try_current()?.spawn(async move {
            let result = http.lock().await.write(&mut *tokens.lock().await, Method::PATCH, "/trade/v2/orders", body.to_string().into(), &reference).await;
            if matches!(result, Ok(WriteOutcome::Answered {status, ..}) if !(200..300).contains(&status) && status != 409) {
                emitter.emit_order_modify_rejected(&order, Some(id), "Saxo rejected modification", get_atomic_clock_realtime().get_time_ns(), cmd.causation_id);
            }
        });
        Ok(())
    }
    fn cancel_order(&self, cmd: CancelOrder) -> anyhow::Result<()> {
        let order = self.core.get_order(&cmd.client_order_id)?;
        let input = (|| {
            anyhow::ensure!(self.core.is_connected(), "not connected");
            Self::valid(&order)?;
            let id = cmd
                .venue_order_id
                .ok_or_else(|| anyhow::anyhow!("missing venue order id"))?;
            let reference = cmd.client_order_id.to_string();
            request::market(&self.config.account_key, Side::Buy, 1000, &reference)?;
            let path = request::cancel_path(id.as_ref(), &self.config.account_key)?;
            Ok::<_, anyhow::Error>((id, reference, path))
        })();
        let (id, reference, path) = match input {
            Ok(v) => v,
            Err(e) => {
                self.emitter.emit_order_cancel_denied(
                    &order,
                    &e.to_string(),
                    get_atomic_clock_realtime().get_time_ns(),
                    cmd.causation_id,
                );
                return Ok(());
            }
        };
        let http = self.http.clone();
        let tokens = self.config.tokens.clone();
        let emitter = self.emitter.clone();
        tokio::runtime::Handle::try_current()?.spawn(async move {
            let result = http.lock().await.write(&mut *tokens.lock().await, Method::DELETE, &path, bytes::Bytes::new(), &reference).await;
            if matches!(result, Ok(WriteOutcome::Answered {status, ..}) if !(200..300).contains(&status) && status != 409) {
                emitter.emit_order_cancel_rejected(&order, Some(id), "Saxo rejected cancellation", get_atomic_clock_realtime().get_time_ns(), cmd.causation_id);
            }
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{auth::TokenManager, env::Env, http::test_support::server};
    use nautilus_common::{cache::Cache, messages::ExecutionEvent};
    use nautilus_core::UUID4;
    use nautilus_model::{
        enums::{AccountType, TimeInForce, TriggerType},
        events::{OrderEventAny, OrderInitialized},
        identifiers::{OrderListId, StrategyId, TraderId, VenueOrderId},
        orders::{LimitOrder, MarketOrder, OrderList, StopMarketOrder},
        types::{Price, Quantity},
    };
    use std::{cell::RefCell, rc::Rc, time::Duration};
    use tokio::{sync::mpsc, time::timeout};

    type RequestRx = mpsc::UnboundedReceiver<(tokio::time::Instant, String)>;
    async fn setup(
        response: &'static str,
        hang: bool,
    ) -> (
        SaxoExecutionClient,
        Rc<RefCell<Cache>>,
        mpsc::UnboundedReceiver<ExecutionEvent>,
        RequestRx,
        tokio::task::JoinHandle<()>,
    ) {
        let (base, requests, task) = server(response, hang).await;
        let cache = Rc::new(RefCell::new(Cache::default()));
        let core = ExecutionClientCore::new(
            TraderId::from("TESTER-001"),
            ClientId::from("SAXO"),
            Venue::from("SAXO"),
            OmsType::Netting,
            AccountId::from("SAXO-001"),
            AccountType::Margin,
            None,
            cache.clone(),
        );
        core.set_connected();
        let config = SaxoExecutionClientConfig {
            env: Env::Sim,
            account_key: "test-key".into(),
            poll_interval: Duration::from_secs(1),
            tokens: Arc::new(Mutex::new(TokenManager::for_tests("token"))),
        };
        let mut client = SaxoExecutionClient::with_http(core, config, SaxoHttp::test_client(&base));
        let (tx, rx) = mpsc::unbounded_channel();
        client.emitter.set_sender(tx);
        (client, cache, rx, requests, task)
    }
    fn order_with_links(
        kind: OrderType,
        id: &str,
        side: OrderSide,
        list: Option<OrderListId>,
        parent: Option<ClientOrderId>,
        child: Option<ClientOrderId>,
    ) -> OrderAny {
        let trader = TraderId::from("TESTER-001");
        let strategy = StrategyId::from("S-001");
        let instrument = InstrumentId::from("USD/JPY.SAXO");
        let id = ClientOrderId::from(id);
        let quantity = Quantity::from(1000);
        match kind {
            OrderType::Market => MarketOrder::new_checked(
                trader,
                strategy,
                instrument,
                id,
                side,
                quantity,
                TimeInForce::Day,
                UUID4::new(),
                0.into(),
                false,
                false,
                child.map(|_| ContingencyType::Oto),
                list,
                child.map(|c| vec![c]),
                parent,
                None,
                None,
                None,
                None,
            )
            .expect("market")
            .into(),
            OrderType::StopMarket => StopMarketOrder::new_checked(
                trader,
                strategy,
                instrument,
                id,
                side,
                quantity,
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
                list,
                None,
                parent,
                None,
                None,
                None,
                None,
                UUID4::new(),
                0.into(),
            )
            .expect("stop")
            .into(),
            OrderType::Limit => LimitOrder::new_checked(
                trader,
                strategy,
                instrument,
                id,
                side,
                quantity,
                Price::from("145.000"),
                TimeInForce::Day,
                None,
                false,
                false,
                false,
                None,
                None,
                None,
                None,
                list,
                None,
                parent,
                None,
                None,
                None,
                None,
                UUID4::new(),
                0.into(),
            )
            .expect("limit")
            .into(),
            _ => panic!("unsupported test order"),
        }
    }
    fn order(kind: OrderType, id: &str, side: OrderSide) -> OrderAny {
        order_with_links(kind, id, side, None, None, None)
    }
    fn submit(client: &SaxoExecutionClient, cache: &Rc<RefCell<Cache>>, order: &OrderAny) {
        cache
            .borrow_mut()
            .add_order(order.clone(), None, None, false)
            .expect("cache");
        client
            .submit_order(SubmitOrder::from_order(
                order,
                client.core.trader_id,
                None,
                None,
                UUID4::new(),
                0.into(),
            ))
            .expect("submit");
    }
    async fn event(rx: &mut mpsc::UnboundedReceiver<ExecutionEvent>) -> ExecutionEvent {
        timeout(Duration::from_secs(3), rx.recv())
            .await
            .expect("event timeout")
            .expect("event")
    }
    async fn request(rx: &mut RequestRx) -> String {
        timeout(Duration::from_secs(3), rx.recv())
            .await
            .expect("request timeout")
            .expect("request")
            .1
    }
    const OK: &str =
        "HTTP/1.1 200 OK\r\nContent-Length: 17\r\nConnection: close\r\n\r\n{\"OrderId\":\"123\"}";
    const LIST_OK: &str = "HTTP/1.1 200 OK\r\nContent-Length: 46\r\nConnection: close\r\n\r\n{\"OrderId\":\"123\",\"Orders\":[{\"OrderId\":\"124\"}]}";
    #[tokio::test]
    async fn market_posts_once_and_submits() {
        let (client, cache, mut events, mut requests, task) = setup(OK, false).await;
        let o = order(OrderType::Market, "market-1", OrderSide::Buy);
        submit(&client, &cache, &o);
        assert!(matches!(
            event(&mut events).await,
            ExecutionEvent::Order(OrderEventAny::Submitted(_))
        ));
        let req = request(&mut requests).await;
        assert!(req.starts_with("POST /trade/v2/orders "));
        assert!(req.contains("\"OrderType\":\"Market\""));
        assert!(req.to_ascii_lowercase().contains("x-request-id: market-1"));
        timeout(Duration::from_secs(2), async {
            while !client
                .order_ids
                .lock()
                .await
                .contains_key(&o.client_order_id())
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("id");
        assert_eq!(
            client
                .order_ids
                .lock()
                .await
                .get(&o.client_order_id())
                .map(String::as_str),
            Some("123")
        );
        assert!(
            timeout(Duration::from_millis(100), requests.recv())
                .await
                .is_err()
        );
        task.abort();
    }
    #[tokio::test]
    async fn standalone_stop_posts_stop_order() {
        let (client, cache, mut events, mut requests, task) = setup(OK, false).await;
        let o = order(OrderType::StopMarket, "stop-alone", OrderSide::Sell);
        submit(&client, &cache, &o);
        assert!(matches!(
            event(&mut events).await,
            ExecutionEvent::Order(OrderEventAny::Submitted(_))
        ));
        let raw = request(&mut requests).await;
        let body: serde_json::Value =
            serde_json::from_str(raw.split_once("\r\n\r\n").expect("body").1).expect("json");
        assert_eq!(body["OrderType"], "Stop");
        assert_eq!(body["OrderPrice"], 145.0);
        assert!(
            timeout(Duration::from_millis(100), requests.recv())
                .await
                .is_err()
        );
        task.abort();
    }
    #[tokio::test]
    async fn bracket_posts_one_request_with_one_stop() {
        let (client, cache, mut events, mut requests, task) = setup(LIST_OK, false).await;
        let entry_id = ClientOrderId::from("entry-1");
        let stop_id = ClientOrderId::from("stop-1");
        let list_id = OrderListId::from("list-1");
        let entry = order_with_links(
            OrderType::Market,
            "entry-1",
            OrderSide::Buy,
            Some(list_id),
            None,
            Some(stop_id),
        );
        let stop = order_with_links(
            OrderType::StopMarket,
            "stop-1",
            OrderSide::Sell,
            Some(list_id),
            Some(entry_id),
            None,
        );
        for o in [&entry, &stop] {
            cache
                .borrow_mut()
                .add_order(o.clone(), None, None, false)
                .expect("cache");
        }
        let list = OrderList::new(
            list_id,
            entry.instrument_id(),
            entry.strategy_id(),
            vec![entry_id, stop_id],
            0.into(),
        );
        let cmd = SubmitOrderList::new(
            client.core.trader_id,
            None,
            entry.strategy_id(),
            list,
            vec![
                OrderInitialized::from(&entry),
                OrderInitialized::from(&stop),
            ],
            None,
            None,
            None,
            UUID4::new(),
            0.into(),
            None,
        );
        client.submit_order_list(cmd).expect("list");
        assert!(matches!(
            event(&mut events).await,
            ExecutionEvent::Order(OrderEventAny::Submitted(_))
        ));
        assert!(matches!(
            event(&mut events).await,
            ExecutionEvent::Order(OrderEventAny::Submitted(_))
        ));
        let raw = request(&mut requests).await;
        let body: serde_json::Value =
            serde_json::from_str(raw.split_once("\r\n\r\n").expect("body").1).expect("json");
        assert_eq!(body["Orders"].as_array().expect("orders").len(), 1);
        assert_eq!(body["Orders"][0]["ExternalReference"], "stop-1");
        timeout(Duration::from_secs(2), async {
            while client.order_ids.lock().await.len() < 2 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("ids");
        assert_eq!(
            client
                .order_ids
                .lock()
                .await
                .get(&stop_id)
                .map(String::as_str),
            Some("124")
        );
        assert!(
            timeout(Duration::from_millis(100), requests.recv())
                .await
                .is_err()
        );
        task.abort();
    }
    #[tokio::test]
    async fn unsupported_type_and_invalid_reference_are_denied_without_request() {
        let (client, cache, mut events, mut requests, task) = setup(OK, false).await;
        for o in [
            order(OrderType::Limit, "limit-1", OrderSide::Buy),
            order(OrderType::Market, "invalid_id", OrderSide::Buy),
        ] {
            submit(&client, &cache, &o);
            assert!(matches!(
                event(&mut events).await,
                ExecutionEvent::Order(OrderEventAny::Denied(_))
            ));
        }
        assert!(
            timeout(Duration::from_millis(150), requests.recv())
                .await
                .is_err()
        );
        task.abort();
    }
    #[tokio::test]
    async fn conflict_and_timeout_do_not_retry_or_emit_terminal_event() {
        for (response, hang) in [
            (
                "HTTP/1.1 409 Conflict\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                false,
            ),
            (OK, true),
        ] {
            let (client, cache, mut events, mut requests, task) = setup(response, hang).await;
            submit(
                &client,
                &cache,
                &order(OrderType::Market, "unknown-1", OrderSide::Buy),
            );
            assert!(matches!(
                event(&mut events).await,
                ExecutionEvent::Order(OrderEventAny::Submitted(_))
            ));
            let _ = request(&mut requests).await;
            tokio::time::sleep(Duration::from_millis(1300)).await;
            assert!(events.try_recv().is_err());
            assert!(requests.try_recv().is_err());
            task.abort();
        }
    }
    #[tokio::test]
    async fn stop_modify_and_cancel_send_expected_requests() {
        let (client, cache, mut events, mut requests, task) = setup(OK, false).await;
        let o = order(OrderType::StopMarket, "stop-2", OrderSide::Sell);
        cache
            .borrow_mut()
            .add_order(o.clone(), None, None, false)
            .expect("cache");
        let id = Some(VenueOrderId::from("123"));
        let modify = ModifyOrder::new(
            client.core.trader_id,
            None,
            o.strategy_id(),
            o.instrument_id(),
            o.client_order_id(),
            id,
            None,
            None,
            Some(Price::from("144.000")),
            UUID4::new(),
            0.into(),
            None,
            None,
        );
        client.modify_order(modify).expect("modify");
        let raw = request(&mut requests).await;
        assert!(raw.starts_with("PATCH /trade/v2/orders "));
        assert!(raw.contains("\"OrderPrice\":144.0"));
        let cancel = CancelOrder::new(
            client.core.trader_id,
            None,
            o.strategy_id(),
            o.instrument_id(),
            o.client_order_id(),
            id,
            UUID4::new(),
            0.into(),
            None,
            None,
        );
        client.cancel_order(cancel).expect("cancel");
        let raw = request(&mut requests).await;
        assert!(raw.starts_with("DELETE /trade/v2/orders/123?AccountKey=test-key "));
        assert!(events.try_recv().is_err());
        task.abort();
    }
    async fn history_server(
        responses: Vec<(u16, String)>,
    ) -> (
        String,
        mpsc::UnboundedReceiver<String>,
        tokio::task::JoinHandle<()>,
    ) {
        use tokio::{
            io::{AsyncReadExt, AsyncWriteExt},
            net::TcpListener,
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let base = format!("http://{}", listener.local_addr().expect("address"));
        let (tx, rx) = mpsc::unbounded_channel();
        let task = tokio::spawn(async move {
            let mut i = 0;
            loop {
                let (mut stream, _) = listener.accept().await.expect("accept");
                let mut buf = vec![0; 8192];
                let n = stream.read(&mut buf).await.expect("read");
                let req = String::from_utf8_lossy(&buf[..n]).into_owned();
                let _ = tx.send(req);
                let (status, body) = &responses[i.min(responses.len() - 1)];
                i += 1;
                let response = format!(
                    "HTTP/1.1 {status} OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
            }
        });
        (base, rx, task)
    }
    fn fixture(s: &str) -> String {
        serde_json::from_str::<serde_json::Value>(s).expect("fixture")["response"].to_string()
    }
    async fn history_setup(
        responses: Vec<(u16, String)>,
    ) -> (
        SaxoExecutionClient,
        mpsc::UnboundedReceiver<ExecutionEvent>,
        mpsc::UnboundedReceiver<String>,
        tokio::task::JoinHandle<()>,
    ) {
        let (base, requests, task) = history_server(responses).await;
        let cache = Rc::new(RefCell::new(Cache::default()));
        let core = ExecutionClientCore::new(
            TraderId::from("TESTER-001"),
            ClientId::from("SAXO"),
            Venue::from("SAXO"),
            OmsType::Netting,
            AccountId::from("SAXO-001"),
            AccountType::Margin,
            None,
            cache,
        );
        let config = SaxoExecutionClientConfig {
            env: Env::Sim,
            account_key: "test-key".into(),
            tokens: Arc::new(Mutex::new(TokenManager::for_tests("token"))),
            poll_interval: Duration::from_millis(10),
        };
        let mut client = SaxoExecutionClient::with_http(core, config, SaxoHttp::test_client(&base));
        let (tx, rx) = mpsc::unbounded_channel();
        client.emitter.set_sender(tx);
        (client, rx, requests, task)
    }
    async fn track(client: &SaxoExecutionClient, order: OrderAny, id: Option<&str>) {
        client.tracked.lock().await.insert(
            order.client_order_id(),
            Tracked::new(order, id.map(str::to_owned)),
        );
    }
    async fn poll_once(client: &SaxoExecutionClient) {
        tracker::poll(
            &client.http,
            &client.config.tokens,
            &client.tracked,
            &client.order_ids,
            &client.emitter,
        )
        .await;
    }
    #[tokio::test]
    async fn history_entry_fill_and_stop_accepted() {
        let mut entry: serde_json::Value = serde_json::from_str(&fixture(include_str!(
            "../tests/fixtures/get_orderactivities_FinalFill_Placed_Market.json"
        )))
        .expect("entry");
        let stop: serde_json::Value = serde_json::from_str(&fixture(include_str!(
            "../tests/fixtures/get_orderactivities_Placed_Stop.json"
        )))
        .expect("stop");
        entry["Data"]
            .as_array_mut()
            .expect("data")
            .extend(stop["Data"].as_array().expect("stop data").iter().cloned());
        let (client, mut events, mut requests, task) =
            history_setup(vec![(200, entry.to_string())]).await;
        let entry = order(OrderType::Market, "entry-history", OrderSide::Buy);
        let entry_id = entry.client_order_id();
        let stop = order(OrderType::StopMarket, "stop-history", OrderSide::Sell);
        let stop_id = stop.client_order_id();
        track(&client, entry, Some("1001")).await;
        track(&client, stop, Some("1012")).await;
        poll_once(&client).await;
        let first = request_path(&mut requests).await;
        let second = request_path(&mut requests).await;
        assert!(first.contains("OrderId=1001") || second.contains("OrderId=1001"));
        assert!(first.contains("OrderId=1012") || second.contains("OrderId=1012"));
        let mut accepted = Vec::new();
        let mut fills = Vec::new();
        while let Ok(event) = events.try_recv() {
            match event {
                ExecutionEvent::Order(OrderEventAny::Accepted(e)) => {
                    accepted.push(e.client_order_id)
                }
                ExecutionEvent::Order(OrderEventAny::Filled(e)) => fills.push(e.client_order_id),
                other => panic!("unexpected {other:?}"),
            }
        }
        assert_eq!(accepted.len(), 2);
        assert!(accepted.contains(&entry_id));
        assert!(accepted.contains(&stop_id));
        assert_eq!(fills, vec![entry_id]);
        task.abort();
    }
    async fn request_path(requests: &mut mpsc::UnboundedReceiver<String>) -> String {
        timeout(Duration::from_secs(2), requests.recv())
            .await
            .expect("request timeout")
            .expect("request")
    }
    #[tokio::test]
    async fn repeated_history_emits_fill_once() {
        let full = fixture(include_str!(
            "../tests/fixtures/get_orderactivities_FinalFill_Placed_Market.json"
        ));
        let (client, mut events, mut requests, task) = history_setup(vec![(200, full)]).await;
        track(
            &client,
            order(OrderType::Market, "repeat-1", OrderSide::Buy),
            Some("1001"),
        )
        .await;
        poll_once(&client).await;
        poll_once(&client).await;
        let _ = request_path(&mut requests).await;
        assert!(matches!(
            event(&mut events).await,
            ExecutionEvent::Order(OrderEventAny::Accepted(_))
        ));
        assert!(matches!(
            event(&mut events).await,
            ExecutionEvent::Order(OrderEventAny::Filled(_))
        ));
        assert!(events.try_recv().is_err());
        assert!(requests.try_recv().is_err());
        task.abort();
    }
    #[tokio::test]
    async fn triggered_stop_market_history_fills_original_stop() {
        let body = fixture(include_str!(
            "../tests/fixtures/get_orderactivities_FinalFill_Placed_Market_Stop.json"
        ));
        let (client, mut events, _, task) = history_setup(vec![(200, body)]).await;
        let o = order(OrderType::StopMarket, "stop-trigger", OrderSide::Sell);
        let id = o.client_order_id();
        track(&client, o, Some("1029")).await;
        poll_once(&client).await;
        assert!(matches!(
            event(&mut events).await,
            ExecutionEvent::Order(OrderEventAny::Accepted(_))
        ));
        match event(&mut events).await {
            ExecutionEvent::Order(OrderEventAny::Filled(fill)) => {
                assert_eq!(fill.client_order_id, id);
                assert_eq!(fill.trade_id.to_string(), "1032");
                assert_eq!(fill.last_qty, Quantity::from(1000));
                assert_eq!(fill.last_px, Price::from("146.564"));
            }
            other => panic!("unexpected {other:?}"),
        }
        task.abort();
    }
    #[tokio::test]
    async fn unresolved_reference_is_found_then_filled() {
        let body = fixture(include_str!(
            "../tests/fixtures/get_orderactivities_FinalFill_Placed_Market.json"
        ));
        let (client, mut events, mut requests, task) = history_setup(vec![(200, body)]).await;
        let o = order(OrderType::Market, "r79-S1-E", OrderSide::Buy);
        let id = o.client_order_id();
        track(&client, o, None).await;
        poll_once(&client).await;
        assert!(request_path(&mut requests).await.contains("EntryType=Last"));
        assert!(events.try_recv().is_err());
        assert_eq!(
            client.order_ids.lock().await.get(&id).map(String::as_str),
            Some("1001")
        );
        poll_once(&client).await;
        assert!(
            request_path(&mut requests)
                .await
                .contains("OrderId=1001&EntryType=All")
        );
        assert!(matches!(
            event(&mut events).await,
            ExecutionEvent::Order(OrderEventAny::Accepted(_))
        ));
        assert!(matches!(
            event(&mut events).await,
            ExecutionEvent::Order(OrderEventAny::Filled(_))
        ));
        task.abort();
    }
    #[tokio::test]
    async fn unresolved_reference_absent_emits_nothing() {
        let body = fixture(include_str!(
            "../tests/fixtures/get_orderactivities_FinalFill_Placed_Market.json"
        ));
        let (client, mut events, mut requests, task) = history_setup(vec![(200, body)]).await;
        track(
            &client,
            order(OrderType::Market, "missing-1", OrderSide::Buy),
            None,
        )
        .await;
        poll_once(&client).await;
        poll_once(&client).await;
        let _ = request_path(&mut requests).await;
        let _ = request_path(&mut requests).await;
        assert!(events.try_recv().is_err());
        assert_eq!(client.tracked.lock().await.len(), 1);
        task.abort();
    }
    #[tokio::test]
    async fn server_error_retries_next_poll_without_event() {
        let body = fixture(include_str!(
            "../tests/fixtures/get_orderactivities_Placed_Stop.json"
        ));
        let (client, mut events, mut requests, task) =
            history_setup(vec![(500, "error".into()), (200, body)]).await;
        track(
            &client,
            order(OrderType::StopMarket, "retry-1", OrderSide::Sell),
            Some("1012"),
        )
        .await;
        poll_once(&client).await;
        let _ = request_path(&mut requests).await;
        assert!(events.try_recv().is_err());
        poll_once(&client).await;
        let _ = request_path(&mut requests).await;
        assert!(matches!(
            event(&mut events).await,
            ExecutionEvent::Order(OrderEventAny::Accepted(_))
        ));
        task.abort();
    }
    #[tokio::test]
    async fn conflict_submission_recovers_from_history() {
        let body = fixture(include_str!(
            "../tests/fixtures/get_orderactivities_FinalFill_Placed_Market.json"
        ));
        let (client, mut events, mut requests, task) =
            history_setup(vec![(409, String::new()), (200, body.clone()), (200, body)]).await;
        client.core.set_connected();
        let order = order(OrderType::Market, "r79-S1-E", OrderSide::Buy);
        let body = request::market("test-key", Side::Buy, 1000, "r79-S1-E").expect("body");
        client.submit(vec![order], body).expect("submit");
        assert!(matches!(
            event(&mut events).await,
            ExecutionEvent::Order(OrderEventAny::Submitted(_))
        ));
        assert!(request_path(&mut requests).await.starts_with("POST"));
        timeout(Duration::from_secs(2), async {
            while client.tracked.lock().await.is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("tracked");
        poll_once(&client).await;
        assert!(request_path(&mut requests).await.contains("EntryType=Last"));
        poll_once(&client).await;
        assert!(request_path(&mut requests).await.contains("OrderId=1001"));
        assert!(matches!(
            event(&mut events).await,
            ExecutionEvent::Order(OrderEventAny::Accepted(_))
        ));
        assert!(matches!(
            event(&mut events).await,
            ExecutionEvent::Order(OrderEventAny::Filled(_))
        ));
        task.abort();
    }
    #[tokio::test]
    async fn connect_emits_account_and_rejects_failed_balance() {
        let mut details: serde_json::Value =
            serde_json::from_str(&fixture(include_str!("../tests/fixtures/live_client.json")))
                .expect("client");
        details["ForceOpenDefaultValue"] = false.into();
        let instrument = fixture(include_str!(
            "../tests/fixtures/live_USDJPY_instrument.json"
        ));
        let accounts = fixture(include_str!(
            "../tests/fixtures/get_port_v1_accounts_me.json"
        ));
        let balance = fixture(include_str!(
            "../tests/fixtures/get_port_v1_balances_me.json"
        ));
        let responses = || {
            vec![
                (200, details.to_string()),
                (200, instrument.clone()),
                (200, accounts.clone()),
                (200, balance.clone()),
            ]
        };
        let (mut client, mut events, mut requests, task) = history_setup(responses()).await;
        client.config.account_key = "TestAccountKey1==".into();
        client.connect().await.expect("connect");
        for _ in 0..4 {
            let _ = request_path(&mut requests).await;
        }
        assert!(matches!(
            event(&mut events).await,
            ExecutionEvent::Account(_)
        ));
        assert!(client.is_connected());
        client.stop().expect("stop");
        task.abort();
        let mut failed = responses();
        failed[3] = (500, "error".into());
        let (mut client, mut events, _, task) = history_setup(failed).await;
        client.config.account_key = "TestAccountKey1==".into();
        let error = client
            .connect()
            .await
            .expect_err("failed balance must reject connection");
        assert!(error.to_string().contains("balance"));
        assert!(!client.is_connected());
        assert!(events.try_recv().is_err());
        task.abort();
    }
    #[tokio::test]
    async fn connection_starts_and_stops_single_poller() {
        let mut client_json: serde_json::Value =
            serde_json::from_str(&fixture(include_str!("../tests/fixtures/live_client.json")))
                .expect("client");
        client_json["ForceOpenDefaultValue"] = false.into();
        let instrument = fixture(include_str!(
            "../tests/fixtures/live_USDJPY_instrument.json"
        ));
        let accounts = fixture(include_str!(
            "../tests/fixtures/get_port_v1_accounts_me.json"
        ));
        let history = fixture(include_str!(
            "../tests/fixtures/get_orderactivities_Placed_Stop.json"
        ));
        let (mut client, mut events, mut requests, task) = history_setup(vec![
            (200, client_json.to_string()),
            (200, instrument),
            (200, accounts),
            (
                200,
                fixture(include_str!(
                    "../tests/fixtures/get_port_v1_balances_me.json"
                )),
            ),
            (200, history),
        ])
        .await;
        client.config.account_key = "TestAccountKey1==".into();
        track(
            &client,
            order(OrderType::StopMarket, "poll-1", OrderSide::Sell),
            Some("1012"),
        )
        .await;
        client.connect().await.expect("connect");
        client.connect().await.expect("connect again");
        for _ in 0..4 {
            let _ = request_path(&mut requests).await;
        }
        assert!(matches!(
            event(&mut events).await,
            ExecutionEvent::Account(_)
        ));
        assert!(request_path(&mut requests).await.contains("OrderId=1012"));
        assert!(matches!(
            event(&mut events).await,
            ExecutionEvent::Order(OrderEventAny::Accepted(_))
        ));
        client.disconnect().await.expect("disconnect");
        while requests.try_recv().is_ok() {}
        assert!(
            timeout(Duration::from_millis(80), requests.recv())
                .await
                .is_err()
        );
        assert!(!client.is_connected());
        task.abort();
    }
    #[tokio::test]
    async fn changed_and_cancelled_events_follow_history() {
        let changed = fixture(include_str!(
            "../tests/fixtures/get_orderactivities_Changed_Placed_Stop.json"
        ));
        let (client, mut events, _, task) = history_setup(vec![(200, changed.clone())]).await;
        track(
            &client,
            order(OrderType::StopMarket, "changed-1", OrderSide::Sell),
            Some("1034"),
        )
        .await;
        poll_once(&client).await;
        poll_once(&client).await;
        assert!(matches!(
            event(&mut events).await,
            ExecutionEvent::Order(OrderEventAny::Accepted(_))
        ));
        match event(&mut events).await {
            ExecutionEvent::Order(OrderEventAny::Updated(e)) => {
                assert_eq!(e.trigger_price, Some(Price::from("145.911")))
            }
            other => panic!("unexpected {other:?}"),
        };
        assert!(events.try_recv().is_err());
        task.abort();
        let cancelled = fixture(include_str!(
            "../tests/fixtures/get_orderactivities_Cancelled_Placed_Stop.json"
        ));
        let (client, mut events, _, task) = history_setup(vec![(200, cancelled)]).await;
        track(
            &client,
            order(OrderType::StopMarket, "cancelled-1", OrderSide::Sell),
            Some("1008"),
        )
        .await;
        poll_once(&client).await;
        assert!(matches!(
            event(&mut events).await,
            ExecutionEvent::Order(OrderEventAny::Accepted(_))
        ));
        assert!(matches!(
            event(&mut events).await,
            ExecutionEvent::Order(OrderEventAny::Canceled(_))
        ));
        task.abort();
    }
    #[tokio::test]
    async fn rejected_history_ends_tracking() {
        let json = r#"{"Data":[{"ActivityTime":"2030-01-02T14:19:35Z","LogId":"9","OrderId":"42","Status":"Rejected"}]}"#;
        let (client, mut events, _, task) = history_setup(vec![(200, json.into())]).await;
        track(
            &client,
            order(OrderType::Market, "rejected-1", OrderSide::Buy),
            Some("42"),
        )
        .await;
        poll_once(&client).await;
        assert!(matches!(
            event(&mut events).await,
            ExecutionEvent::Order(OrderEventAny::Accepted(_))
        ));
        assert!(matches!(
            event(&mut events).await,
            ExecutionEvent::Order(OrderEventAny::Rejected(_))
        ));
        assert!(client.tracked.lock().await.is_empty());
        task.abort();
    }
    #[tokio::test]
    async fn reconciliation_methods_fetch_fixtures_and_emit_account() {
        let order_body = fixture(include_str!(
            "../tests/fixtures/get_port_v1_orders_me_1.json"
        ));
        let fill_body = fixture(include_str!(
            "../tests/fixtures/get_orderactivities_FinalFill_Placed_Market.json"
        ));
        let pos_body = fixture(include_str!(
            "../tests/fixtures/get_port_v1_positions_me_2.json"
        ));
        let balance_body = fixture(include_str!(
            "../tests/fixtures/get_port_v1_balances_me.json"
        ));
        let (client, mut events, mut requests, task) = history_setup(vec![
            (200, order_body),
            (200, fill_body),
            (200, pos_body),
            (200, balance_body),
        ])
        .await;
        let orders = GenerateOrderStatusReports::new(
            UUID4::new(),
            0.into(),
            true,
            None,
            None,
            None,
            None,
            None,
        );
        let open = client
            .generate_order_status_reports(&orders)
            .await
            .expect("orders");
        assert_eq!(open.len(), 1);
        assert!(
            request_path(&mut requests)
                .await
                .contains("/port/v1/orders/me")
        );
        let cmd =
            GenerateFillReports::new(UUID4::new(), 0.into(), None, None, None, None, None, None);
        let fills = client.generate_fill_reports(cmd).await.expect("fills");
        assert_eq!(fills.len(), 1);
        assert!(
            request_path(&mut requests)
                .await
                .contains("/cs/v1/audit/orderactivities")
        );
        let cmd = GeneratePositionStatusReports::new(
            UUID4::new(),
            0.into(),
            None,
            None,
            None,
            None,
            None,
        );
        let positions = client
            .generate_position_status_reports(&cmd)
            .await
            .expect("positions");
        assert_eq!(positions[0].quantity, Quantity::from(2000));
        assert!(
            request_path(&mut requests)
                .await
                .contains("/port/v1/positions/me")
        );
        let cmd = QueryAccount::new(
            client.core.trader_id,
            None,
            client.core.account_id,
            UUID4::new(),
            0.into(),
            None,
            None,
        );
        client.query_account(cmd).expect("query");
        assert!(
            request_path(&mut requests)
                .await
                .contains("/port/v1/balances/me")
        );
        assert!(matches!(
            event(&mut events).await,
            ExecutionEvent::Account(_)
        ));
        task.abort();
    }
    #[tokio::test]
    async fn fill_report_passes_requested_date_range() {
        let body = fixture(include_str!(
            "../tests/fixtures/get_orderactivities_FinalFill_Placed_Market.json"
        ));
        let (client, _, mut requests, task) = history_setup(vec![(200, body)]).await;
        let start: UnixNanos = 1_893_456_000_000_000_000_u64.into();
        let end: UnixNanos = 1_924_992_000_000_000_000_u64.into();
        let cmd = GenerateFillReports::new(
            UUID4::new(),
            0.into(),
            None,
            None,
            Some(start),
            Some(end),
            None,
            None,
        );
        client.generate_fill_reports(cmd).await.expect("fills");
        let raw = request_path(&mut requests).await;
        assert!(raw.contains("FromDateTime=2030-01-01T00%3A00%3A00Z"));
        assert!(raw.contains("ToDateTime=2031-01-01T00%3A00%3A00Z"));
        task.abort();
    }
    #[tokio::test]
    async fn force_open_connection_fails_without_writes() {
        let client_json = fixture(include_str!("../tests/fixtures/live_client.json"));
        let instrument = fixture(include_str!(
            "../tests/fixtures/live_USDJPY_instrument.json"
        ));
        let accounts = fixture(include_str!(
            "../tests/fixtures/get_port_v1_accounts_me.json"
        ));
        let (mut client, _, mut requests, task) =
            history_setup(vec![(200, client_json), (200, instrument), (200, accounts)]).await;
        client.config.account_key = "TestAccountKey1==".into();
        assert!(client.connect().await.is_err());
        assert!(!client.is_connected());
        for _ in 0..3 {
            assert!(request_path(&mut requests).await.starts_with("GET "));
        }
        assert!(
            timeout(Duration::from_millis(150), requests.recv())
                .await
                .is_err()
        );
        task.abort();
    }
}
