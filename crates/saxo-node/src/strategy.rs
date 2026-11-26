use nautilus_common::{actor::DataActor, timer::TimeEvent};
use nautilus_core::UUID4;
use nautilus_live::node::LiveNodeHandle;
use nautilus_model::{
    data::QuoteTick,
    enums::{ContingencyType, OrderSide, OrderStatus, TimeInForce, TriggerType},
    events::{OrderEventAny, PositionEvent},
    identifiers::{ClientId, ClientOrderId, InstrumentId, OrderListId, StrategyId},
    orders::{MarketOrder, Order, OrderAny, StopMarketOrder},
    types::{Price, Quantity},
};
use nautilus_trading::{Strategy, StrategyConfig, StrategyCore, nautilus_strategy};
use saxo_node::roundtrip::{self, Action, Event, Phase, State};
use std::{
    fs::{File, OpenOptions},
    io::Write,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::time::Instant;

fn fill_event(
    id: ClientOrderId,
    entry: ClientOrderId,
    stop: ClientOrderId,
    full: bool,
) -> Option<Event> {
    if id == stop {
        Some(if full {
            Event::StopFilled
        } else {
            Event::StopRejected
        })
    } else if full && id == entry {
        Some(Event::EntryFilled)
    } else if full {
        Some(Event::ExitFilled)
    } else {
        None
    }
}

#[derive(Debug)]
pub struct Roundtrip {
    core: StrategyCore,
    handle: LiveNodeHandle,
    result: Arc<Mutex<Option<i32>>>,
    state: State,
    instrument: InstrumentId,
    entry: ClientOrderId,
    stop: ClientOrderId,
    list: OrderListId,
    exit: ClientOrderId,
    kill: PathBuf,
    deadline: Instant,
    file: File,
    saw_position: bool,
}
impl Roundtrip {
    pub fn new(
        handle: LiveNodeHandle,
        result: Arc<Mutex<Option<i32>>>,
        kill: PathBuf,
        deadline: Instant,
        out: PathBuf,
    ) -> anyhow::Result<(Self, File)> {
        std::fs::create_dir_all(&out)?;
        let stamp = OffsetDateTime::now_utc().format(&time::macros::format_description!(
            "[year][month][day]T[hour][minute][second]Z"
        ))?;
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(out.join(format!("roundtrip-{stamp}.jsonl")))?;
        let fallback = file.try_clone()?;
        let config = StrategyConfig::builder()
            .strategy_id(StrategyId::new_checked("saxo-roundtrip-001")?)
            .manage_stop(false)
            .log_events(false)
            .log_commands(false)
            .build()?;
        let run_id = UUID4::new();
        Ok((
            Self {
                core: StrategyCore::new_checked(config)?,
                handle,
                result,
                state: State::default(),
                instrument: "USD/JPY.SAXO".parse()?,
                entry: ClientOrderId::new(format!("E-{run_id}")),
                stop: ClientOrderId::new(format!("S-{run_id}")),
                list: OrderListId::new(format!("L-{run_id}")),
                exit: ClientOrderId::new(format!("X-{run_id}")),
                kill,
                deadline,
                file,
                saw_position: false,
            },
            fallback,
        ))
    }
    fn log(
        &mut self,
        kind: &str,
        client: Option<ClientOrderId>,
        venue: Option<String>,
        side: Option<&str>,
        trigger: Option<i64>,
        note: &str,
    ) -> anyhow::Result<()> {
        let row = serde_json::json!({"ts": OffsetDateTime::now_utc().format(&Rfc3339)?, "kind": kind, "client_order_id": client.map(|v| v.to_string()), "venue_order_id": venue, "side": side, "qty": client.map(|_| 1000), "price": null, "trigger_price": trigger.map(|v| format!("{}.{:03}", v / 1000, v % 1000)), "note": note});
        serde_json::to_writer(&mut self.file, &row)?;
        writeln!(self.file)?;
        self.file.flush()?;
        Ok(())
    }
    fn log_order_event(&mut self, kind: &str, event: &OrderEventAny) -> anyhow::Result<()> {
        let id = event.client_order_id();
        let (quantity, price, trigger) = match event {
            OrderEventAny::Filled(fill) => (
                Some(fill.last_qty.to_string()),
                Some(fill.last_px.to_string()),
                None,
            ),
            OrderEventAny::Updated(updated) => (
                Some(updated.quantity.to_string()),
                updated.price.map(|p| p.to_string()),
                updated.trigger_price.map(|p| p.to_string()),
            ),
            _ => (Some("1000".to_owned()), None, None),
        };
        let row = serde_json::json!({"ts": OffsetDateTime::now_utc().format(&Rfc3339)?, "kind":kind, "client_order_id":id.to_string(), "venue_order_id":event.venue_order_id().map(|v| v.to_string()), "side":if id == self.entry { "BUY" } else { "SELL" }, "qty":quantity, "price":price, "trigger_price":trigger, "note":""});
        serde_json::to_writer(&mut self.file, &row)?;
        writeln!(self.file)?;
        self.file.flush()?;
        Ok(())
    }
    fn fail(&mut self) {
        self.state.phase = Phase::InvalidPrice;
        if self.finish().is_err() {
            if let Ok(mut r) = self.result.lock() {
                *r = Some(1);
            }
            self.handle.stop();
        }
    }
    fn net(&self) -> String {
        self.cache()
            .positions_open(None, Some(&self.instrument), None, None, None)
            .iter()
            .map(|p| p.signed_qty)
            .reduce(|a, b| a + b)
            .map_or_else(|| "0".to_owned(), |n| n.normalize().to_string())
    }
    fn finish(&mut self) -> anyhow::Result<()> {
        if self
            .result
            .lock()
            .map_err(|_| anyhow::anyhow!("result lock poisoned"))?
            .is_some()
        {
            return Ok(());
        }
        let code = match self.state.phase {
            Phase::Finished { .. } => 0,
            Phase::Rejected => 6,
            Phase::Killed => 4,
            Phase::TimedOut => 5,
            Phase::CommandLimit | Phase::InvalidPrice => 1,
            _ => return Ok(()),
        };
        let orders = self
            .cache()
            .orders_open_count(None, Some(&self.instrument), None, None, None);
        let net = self.net();
        let note = format!(
            "state={:?} open_orders={orders} position_net={net}{}",
            self.state.phase,
            if self.state.phase
                == (Phase::Finished {
                    stop_triggered: true
                })
            {
                " stop_triggered"
            } else {
                ""
            }
        );
        self.log("final", None, None, None, None, &note)?;
        if let Ok(mut result) = self.result.lock() {
            *result = Some(code);
        }
        self.handle.stop();
        Ok(())
    }
    fn dispatch(&mut self, event: Event) -> anyhow::Result<()> {
        if self.kill.exists() && event != Event::Kill {
            return self.dispatch(Event::Kill);
        }
        if Instant::now() >= self.deadline && event != Event::Timeout && event != Event::Kill {
            return self.dispatch(Event::Timeout);
        }
        let (next, actions) = roundtrip::step(self.state, event);
        self.state = next;
        for action in actions {
            self.execute(action)?;
        }
        self.finish()
    }
    fn may_send(&mut self) -> anyhow::Result<bool> {
        if self.kill.exists() {
            self.dispatch(Event::Kill)?;
            return Ok(false);
        }
        if Instant::now() >= self.deadline {
            self.dispatch(Event::Timeout)?;
            return Ok(false);
        }
        Ok(true)
    }
    fn execute(&mut self, action: Action) -> anyhow::Result<()> {
        let client = Some(ClientId::new("SAXO"));
        match action {
            Action::SubmitEntryAndStop {
                quantity,
                trigger_thousandths,
            } => {
                let trader = self
                    .trader_id()
                    .ok_or_else(|| anyhow::anyhow!("strategy not registered"))?;
                let sid = StrategyId::new("saxo-roundtrip-001");
                let list = self.list;
                let now = self.clock().timestamp_ns();
                let entry: OrderAny = MarketOrder::new_checked(
                    trader,
                    sid,
                    self.instrument,
                    self.entry,
                    OrderSide::Buy,
                    Quantity::from(quantity),
                    TimeInForce::Day,
                    UUID4::new(),
                    now,
                    false,
                    false,
                    Some(ContingencyType::Oto),
                    Some(list),
                    Some(vec![self.stop]),
                    None,
                    None,
                    None,
                    None,
                    None,
                )?
                .into();
                let trigger = Price::from(
                    format!(
                        "{}.{:03}",
                        trigger_thousandths / 1000,
                        trigger_thousandths % 1000
                    )
                    .as_str(),
                );
                let stop: OrderAny = StopMarketOrder::new_checked(
                    trader,
                    sid,
                    self.instrument,
                    self.stop,
                    OrderSide::Sell,
                    Quantity::from(quantity),
                    trigger,
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
                    Some(self.entry),
                    None,
                    None,
                    None,
                    None,
                    UUID4::new(),
                    now,
                )?
                .into();
                self.log(
                    "submit_list",
                    Some(self.entry),
                    None,
                    Some("BUY"),
                    Some(trigger_thousandths),
                    "entry and stop",
                )?;
                if self.may_send()? {
                    self.submit_order_list(vec![entry, stop], None, client, None)?;
                }
            }
            Action::ModifyStop {
                trigger_thousandths,
            } => {
                let venue = self
                    .cache()
                    .order(&self.stop)
                    .and_then(|o| o.venue_order_id())
                    .map(|v| v.to_string());
                self.log(
                    "modify",
                    Some(self.stop),
                    venue,
                    Some("SELL"),
                    Some(trigger_thousandths),
                    "",
                )?;
                let price = Price::from(
                    format!(
                        "{}.{:03}",
                        trigger_thousandths / 1000,
                        trigger_thousandths % 1000
                    )
                    .as_str(),
                );
                if self.may_send()? {
                    self.modify_order(self.stop, None, None, Some(price), client, None)?;
                }
            }
            Action::CancelStop => {
                let venue = self
                    .cache()
                    .order(&self.stop)
                    .and_then(|o| o.venue_order_id())
                    .map(|v| v.to_string());
                self.log("cancel", Some(self.stop), venue, Some("SELL"), None, "")?;
                if self.may_send()? {
                    self.cancel_order(self.stop, client, None)?;
                }
            }
            Action::ClosePosition { quantity } => {
                let trader = self
                    .trader_id()
                    .ok_or_else(|| anyhow::anyhow!("strategy not registered"))?;
                let order = MarketOrder::new_checked(
                    trader,
                    StrategyId::new("saxo-roundtrip-001"),
                    self.instrument,
                    self.exit,
                    OrderSide::Sell,
                    Quantity::from(quantity),
                    TimeInForce::Day,
                    UUID4::new(),
                    self.clock().timestamp_ns(),
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
                )?;
                self.log("close", Some(self.exit), None, Some("SELL"), None, "")?;
                if self.may_send()? {
                    self.submit_order(order.into(), None, client, None)?;
                }
            }
        }
        Ok(())
    }
    fn poll_flat(&mut self) -> anyhow::Result<()> {
        let flat = self.net() == "0";
        if !flat {
            self.saw_position = true;
        }
        if self.saw_position && matches!(self.state.phase, Phase::WaitingForFlat { .. }) {
            self.dispatch(Event::PositionFlat(flat))?;
        }
        Ok(())
    }
}
impl DataActor for Roundtrip {
    fn on_start(&mut self) -> anyhow::Result<()> {
        self.subscribe_quotes(self.instrument, Some(ClientId::new("SAXO")), None);
        self.clock().set_timer(
            "saxo-roundtrip-poll",
            Duration::from_secs(1),
            None,
            None,
            None,
            None,
            None,
        )
    }
    fn on_quote(&mut self, quote: &QuoteTick) -> anyhow::Result<()> {
        if quote.instrument_id == self.instrument {
            if !self.may_send()? {
                return Ok(());
            }
            if self.state.phase == Phase::WaitingForQuote
                && (self.net() != "0"
                    || self.cache().orders_open_count(
                        None,
                        Some(&self.instrument),
                        None,
                        None,
                        None,
                    ) != 0)
            {
                self.state.phase = Phase::InvalidPrice;
                self.finish()?;
                return Ok(());
            }
            let decimal = quote.bid_price.as_decimal() * Price::from("1000000").as_decimal();
            let bid = decimal.trunc().to_string().parse::<i64>()?;
            if let Err(e) = self.dispatch(Event::FirmBid(bid)) {
                self.fail();
                return Err(e);
            }
        }
        Ok(())
    }
    fn on_time_event(&mut self, _: &TimeEvent) -> anyhow::Result<()> {
        let result = if self.kill.exists() {
            self.dispatch(Event::Kill)
        } else if Instant::now() >= self.deadline {
            self.dispatch(Event::Timeout)
        } else {
            self.poll_flat()
        };
        if result.is_err() {
            self.fail();
        }
        result
    }
}
nautilus_strategy!(Roundtrip, {
    fn on_position_event(&mut self, _event: PositionEvent) {
        if self.result.lock().map_or(true, |result| result.is_some()) {
            return;
        }
        if self.poll_flat().is_err() {
            self.fail();
        }
    }
    fn on_order_event(&mut self, event: OrderEventAny) {
        if self.result.lock().map_or(true, |result| result.is_some()) {
            return;
        }
        let id = event.client_order_id();
        if id != self.entry && id != self.stop && id != self.exit {
            return;
        }
        let kind = match &event {
            OrderEventAny::Accepted(_) => "accepted",
            OrderEventAny::Filled(_) => "filled",
            OrderEventAny::Rejected(_) => "rejected",
            OrderEventAny::Denied(_) => "denied",
            OrderEventAny::ModifyRejected(_) => "modify_rejected",
            OrderEventAny::CancelRejected(_) => "cancel_rejected",
            OrderEventAny::Updated(_) => "updated",
            OrderEventAny::Canceled(_) => "canceled",
            _ => return,
        };
        if self.log_order_event(kind, &event).is_err() {
            self.fail();
            return;
        }
        let e = match event {
            OrderEventAny::Filled(_) => fill_event(
                id,
                self.entry,
                self.stop,
                self.cache()
                    .order(&id)
                    .is_some_and(|o| o.status() == OrderStatus::Filled),
            ),
            OrderEventAny::Accepted(_) if id == self.stop => Some(Event::StopAccepted),
            OrderEventAny::Updated(_) if id == self.stop => Some(Event::StopUpdated),
            OrderEventAny::Canceled(_) if id == self.stop => Some(Event::StopCanceled),
            OrderEventAny::ModifyRejected(_) if id == self.stop => Some(Event::StopRejected),
            OrderEventAny::Rejected(_) | OrderEventAny::Denied(_) if id == self.entry => {
                Some(Event::EntryRejected)
            }
            OrderEventAny::Rejected(_) | OrderEventAny::Denied(_) if id == self.stop => {
                Some(Event::StopRejected)
            }
            _ => None,
        };
        if let Some(e) = e
            && self.dispatch(e).is_err()
        {
            self.fail();
        }
    }
});

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn partial_stop_fill_fails_closed_without_exit_order() {
        let entry = ClientOrderId::new("entry");
        let stop = ClientOrderId::new("stop");
        let event = fill_event(stop, entry, stop, false).expect("stop event");
        let state = State {
            phase: Phase::WaitingForCancel,
            commands: 3,
            ..State::default()
        };
        let (state, actions) = roundtrip::step(state, event);
        assert_eq!(state.phase, Phase::Rejected);
        assert!(actions.is_empty());
        assert!(roundtrip::step(state, Event::StopCanceled).1.is_empty());
    }
    #[test]
    fn jsonl_is_flushed_and_contains_only_selected_fields() {
        let directory = std::env::temp_dir().join(format!("saxo-jsonl-{}", UUID4::new()));
        let result = Arc::new(Mutex::new(None));
        let (mut strategy, _) = Roundtrip::new(
            LiveNodeHandle::default(),
            result,
            directory.join("kill"),
            Instant::now() + Duration::from_secs(30),
            directory.clone(),
        )
        .expect("strategy");
        strategy
            .log(
                "submit_list",
                Some(strategy.entry),
                None,
                Some("BUY"),
                Some(155_156),
                "entry and stop",
            )
            .expect("write");
        let path = std::fs::read_dir(&directory)
            .expect("directory")
            .next()
            .expect("file")
            .expect("entry")
            .path();
        let contents = std::fs::read_to_string(path).expect("flushed output");
        let value: serde_json::Value = serde_json::from_str(contents.trim()).expect("json");
        assert_eq!(value["kind"], "submit_list");
        assert_eq!(value["trigger_price"], "155.156");
        assert_eq!(value["qty"], 1000);
        assert!(value["ts"].as_str().expect("timestamp").ends_with('Z'));
        assert!(!contents.contains("AccountGroupId"));
        drop(strategy);
        std::fs::remove_dir_all(directory).expect("cleanup");
    }
}
