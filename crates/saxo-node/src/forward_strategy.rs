use crate::Args;
use forward::{
    decide::{self, Action, Kind, MoveState, OrderState, Quote, StopSnapshot},
    journal::{Action as Record, Environment, Journal, Milli, Row, Side, Status},
    plan::{Direction, Leg, Plan},
    replay::Replay,
};
use nautilus_common::{actor::DataActor, timer::TimeEvent};
use nautilus_live::node::LiveNodeHandle;
use nautilus_model::{
    data::QuoteTick,
    enums::{OrderSide, OrderStatus, OrderType},
    events::{OrderEventAny, PositionEvent},
    identifiers::{ClientId, ClientOrderId, InstrumentId, StrategyId},
    orders::Order,
};
use nautilus_trading::{Strategy, StrategyConfig, StrategyCore, nautilus_strategy};
use saxo_node::{
    forward_control::{self, Control},
    forward_dispatch::{NautilusSink, dispatch},
    forward_events::{Event, OrderLifecycle, write_unfilled},
    forward_fills::FillContext,
    forward_orders::{OrderContext, reference},
    forward_restart::{Resolution, reconcile_intent},
    forward_state::{ForwardState, broker_matches, entry_intent},
};
use std::{
    collections::{HashMap, HashSet},
    fs::{File, OpenOptions},
    os::unix::fs::OpenOptionsExt,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use time::{OffsetDateTime, Time, format_description::well_known::Rfc3339};

pub fn preflight(args: &Args) -> anyhow::Result<()> {
    let journal_path = journal_path(args)?;
    let plan = read_plan(args)?;
    let _: InstrumentId = plan
        .instrument_id
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid instrument: {e}"))?;
    if journal_path.try_exists()? {
        anyhow::ensure!(journal_path.is_file(), "journal must be a file");
        Journal::new(journal_path, Environment::Sim, &plan, HashSet::new())
            .read()
            .map_err(|e| anyhow::anyhow!("journal: {e}"))?;
    } else {
        let parent = journal_path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(std::path::Path::new("."));
        anyhow::ensure!(parent.is_dir(), "journal parent is missing");
    }
    if let Some(halt) = &args.halt {
        let _ = halt.try_exists()?;
    }
    Ok(())
}
pub fn recovery_references(args: &Args) -> anyhow::Result<HashSet<ClientOrderId>> {
    let plan = read_plan(args)?;
    let journal = journal(journal_path(args)?, &plan);
    let rows = journal.read_or_empty().map_err(anyhow::Error::msg)?;
    let restored = replay(&journal, &rows, None)?;
    rows.iter()
        .filter(|r| r.action == Record::Declare && r.side.is_some())
        .filter(|row| {
            restored
                .positions
                .iter()
                .any(|p| p.leg == row.leg && p.event_date.to_string() == row.tokyo_date)
                || !rows.iter().any(|r| {
                    r.idem_key == row.idem_key
                        && matches!(r.action, Record::Entry | Record::Exit | Record::StopExit)
                })
        })
        .map(|row| reference(&row.idem_key))
        .collect()
}

pub fn lock_journal(args: &Args) -> anyhow::Result<File> {
    let mut path = journal_path(args)?.as_os_str().to_owned();
    path.push(".lock");
    let lock = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path)?;
    lock.try_lock()
        .map_err(|error| anyhow::anyhow!("journal lock unavailable: {error}"))?;
    Ok(lock)
}

fn read_plan(args: &Args) -> anyhow::Result<Plan> {
    let path = args
        .plan
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("missing plan"))?;
    Plan::read(path, OffsetDateTime::now_utc()).map_err(anyhow::Error::msg)
}

fn journal_path(args: &Args) -> anyhow::Result<&std::path::Path> {
    args.journal
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("missing journal"))
}

#[derive(Debug)]
pub struct Forward {
    core: StrategyCore,
    handle: LiveNodeHandle,
    result: Arc<Mutex<Option<i32>>>,
    plan: Plan,
    instrument: InstrumentId,
    journal: PathBuf,
    kill: PathBuf,
    halt: Option<PathBuf>,
    until: Option<Time>,
    dry_run: bool,
    quote: Option<Quote>,
    state: Option<ForwardState>,
    fills: HashMap<String, OrderLifecycle>,
    moves: HashMap<String, i64>,
}

fn journal<'a>(path: &std::path::Path, plan: &'a Plan) -> Journal<'a> {
    Journal::new(path, Environment::Sim, plan, HashSet::new())
}

fn replay(journal: &Journal<'_>, rows: &[Row], quote: Option<Quote>) -> anyhow::Result<Replay> {
    let bids = rows
        .iter()
        .filter(|row| row.action == Record::Entry)
        .filter_map(|row| {
            let price = row.fill_price?;
            Some((row.leg.clone(), quote.map_or(price.0, |q| q.bid_milli)))
        })
        .collect();
    if rows.is_empty() {
        return Ok(Replay::empty(Environment::Sim));
    }
    journal.replay(&bids).map_err(anyhow::Error::msg)
}

fn stop_key(event_id: &str) -> String {
    format!("{event_id}-EXIT-SIM-1")
}

fn side(direction: &Direction) -> Side {
    match direction {
        Direction::Buy => Side::Buy,
        Direction::Sell => Side::Sell,
    }
}

fn order_state(status: OrderStatus) -> OrderState {
    match status {
        OrderStatus::Accepted
        | OrderStatus::PartiallyFilled
        | OrderStatus::Triggered
        | OrderStatus::PendingUpdate
        | OrderStatus::PendingCancel => OrderState::Working,
        OrderStatus::Filled | OrderStatus::Canceled => OrderState::Done,
        OrderStatus::Rejected | OrderStatus::Denied | OrderStatus::Expired => OrderState::Refused,
        _ => OrderState::Unknown,
    }
}

fn protective(plan: &Plan, row: &Row) -> bool {
    row.idem_key == stop_key(&row.event_id)
        && plan
            .legs
            .iter()
            .any(|l| l.name == row.leg && l.stop.enabled)
}

fn trade_action(plan: &Plan, intent: &Row) -> Record {
    if protective(plan, intent) {
        Record::StopExit
    } else if intent.idem_key.ends_with("-ENTRY-SIM") {
        Record::Entry
    } else {
        Record::Exit
    }
}

fn declaration<'a>(rows: &'a [Row], key: &str) -> anyhow::Result<&'a Row> {
    rows.iter()
        .find(|r| r.idem_key == key && r.action == Record::Declare)
        .ok_or_else(|| anyhow::anyhow!("Human: unrecognized broker order {key}"))
}

impl Forward {
    pub fn new(
        args: &Args,
        handle: LiveNodeHandle,
        result: Arc<Mutex<Option<i32>>>,
    ) -> anyhow::Result<Self> {
        preflight(args)?;
        let plan = read_plan(args)?;
        let instrument = plan.instrument_id.parse()?;
        let config = StrategyConfig::builder()
            .strategy_id(StrategyId::new_checked("forward-001")?)
            .manage_stop(false)
            .log_events(false)
            .log_commands(false)
            .build()?;
        Ok(Self {
            core: StrategyCore::new_checked(config)?,
            handle,
            result,
            plan,
            instrument,
            journal: journal_path(args)?.to_owned(),
            kill: args.kill.clone(),
            halt: args.halt.clone(),
            until: args.until,
            dry_run: args.dry_run,
            quote: None,
            state: None,
            fills: HashMap::new(),
            moves: HashMap::new(),
        })
    }

    fn finished(&self) -> bool {
        self.result.lock().map_or(true, |result| result.is_some())
    }

    fn finish(&self, code: i32, reason: &str) {
        eprintln!("{reason}");
        if let Ok(mut result) = self.result.lock() {
            *result = Some(code);
        }
        self.handle.stop();
    }

    fn broker_units(&self) -> anyhow::Result<i64> {
        self.cache()
            .positions_open(None, Some(&self.instrument), None, None, None)
            .iter()
            .try_fold(0_i64, |sum, p| {
                let qty = p.signed_qty.normalize().to_string().parse::<i64>()?;
                sum.checked_add(qty)
                    .ok_or_else(|| anyhow::anyhow!("Human: broker position overflow"))
            })
    }

    fn start(&mut self) -> anyhow::Result<()> {
        let now = OffsetDateTime::now_utc();
        let journal = journal(&self.journal, &self.plan);
        let rows = journal.read_or_empty().map_err(anyhow::Error::msg)?;
        let mut restored = replay(&journal, &rows, None)?;
        let broker_units = self.broker_units()?;
        broker_matches(&restored.positions, broker_units)?;
        if self.stop_if_killed(&journal, &restored, now)? {
            return Ok(());
        }
        let cache = self.cache();
        for order in cache.orders_open(None, Some(&self.instrument), None, None, None) {
            let row = declaration(&rows, order.client_order_id().as_ref())?;
            let stop = protective(&self.plan, row);
            let filled = order
                .filled_qty()
                .as_decimal()
                .normalize()
                .to_string()
                .parse::<i64>()?;
            let recorded = rows.iter().find(|r| {
                r.idem_key == row.idem_key
                    && matches!(r.action, Record::Entry | Record::Exit | Record::StopExit)
            });
            anyhow::ensure!(
                recorded.is_none()
                    && filled == 0
                    && order.order_type()
                        == if stop {
                            OrderType::StopMarket
                        } else {
                            OrderType::Market
                        },
                "Human: broker order differs from journal"
            );
            anyhow::ensure!(
                row.side
                    == Some(match order.order_side() {
                        OrderSide::Buy => Side::Buy,
                        OrderSide::Sell => Side::Sell,
                    }),
                "Human: broker order side differs from journal"
            );
        }
        if self
            .until
            .is_some_and(|until| now.to_offset(self.plan.utc_offset).time() >= until)
        {
            return self.summary(&restored, &rows, "reached until");
        }
        if !self.dry_run {
            for row in rows
                .iter()
                .filter(|r| r.action == Record::Declare && r.side.is_some())
            {
                let order = cache.order(&reference(&row.idem_key)?);
                let resolution = reconcile_intent(&journal, row, order.is_some())?;
                if resolution == Resolution::Track
                    && let Some(order) = order
                    && order.is_closed()
                {
                    anyhow::ensure!(
                        order.filled_qty().is_zero(),
                        "Human: broker fill missing from journal"
                    );
                    let reason = match order.status() {
                        OrderStatus::Canceled => "canceled",
                        OrderStatus::Expired => "expired",
                        OrderStatus::Rejected => "rejected",
                        _ => anyhow::bail!("Human: unknown closed broker order"),
                    };
                    write_unfilled(&journal, row.clone(), trade_action(&self.plan, row), reason)?;
                    anyhow::ensure!(reason != "rejected", "Human: recovered order was rejected");
                }
            }
            let rows = journal.read_or_empty().map_err(anyhow::Error::msg)?;
            restored = replay(&journal, &rows, None)?;
            self.state = Some(ForwardState::prepare(
                &self.plan,
                &journal,
                &restored,
                broker_units,
                now,
                &self.kill,
            )?);
        }
        let rows = journal.read_or_empty().map_err(anyhow::Error::msg)?;
        for position in &restored.positions {
            self.stop_snapshot(&rows, &restored, position)?;
        }
        self.subscribe_quotes(self.instrument, Some(ClientId::new("SAXO")), None);
        self.clock().set_timer(
            "forward-poll",
            Duration::from_secs(1),
            None,
            None,
            None,
            None,
            None,
        )
    }

    fn stop_snapshot(
        &self,
        rows: &[Row],
        restored: &Replay,
        position: &decide::Position,
    ) -> anyhow::Result<Option<StopSnapshot>> {
        if !restored.needs_broker_stop.contains(&position.leg) {
            return Ok(None);
        }
        let entry = rows
            .iter()
            .rev()
            .find(|r| {
                r.leg == position.leg
                    && r.tokyo_date == position.event_date.to_string()
                    && r.action == Record::Entry
                    && matches!(r.status, Status::Filled | Status::Partial)
            })
            .ok_or_else(|| anyhow::anyhow!("Human: entry fill unavailable"))?;
        let key = stop_key(&entry.event_id);
        let intent = declaration(rows, &key)?;
        let remaining = position
            .units
            .checked_abs()
            .ok_or_else(|| anyhow::anyhow!("Human: invalid position size"))?;
        let outcome = rows
            .iter()
            .rev()
            .find(|r| r.idem_key == key && r.action == Record::StopExit);
        let filled_units = outcome.and_then(|r| r.units).unwrap_or(0);
        let entry_filled_units = entry
            .units
            .ok_or_else(|| anyhow::anyhow!("Human: entry quantity unavailable"))?;
        let exit_filled_units = entry_filled_units
            .checked_sub(remaining)
            .and_then(|n| n.checked_sub(filled_units))
            .filter(|n| *n >= 0)
            .ok_or_else(|| anyhow::anyhow!("Human: inconsistent stop quantity"))?;
        let (state, quantity, trigger_milli) = if let Some(row) = outcome {
            let state = match row.reason.as_deref() {
                Some("canceled") => OrderState::Done,
                Some("rejected" | "expired") => OrderState::Refused,
                _ => OrderState::Unknown,
            };
            (state, intent.units.unwrap_or(0), 0)
        } else {
            let cache = self.cache();
            let order = cache
                .order(&reference(&key)?)
                .ok_or_else(|| anyhow::anyhow!("Human: broker stop missing"))?;
            anyhow::ensure!(
                order.order_type() == OrderType::StopMarket,
                "Human: invalid stop type"
            );
            let quantity = order
                .quantity()
                .as_decimal()
                .normalize()
                .to_string()
                .parse::<i64>()?;
            let price: Milli = serde_json::from_str(
                &order
                    .trigger_price()
                    .ok_or_else(|| anyhow::anyhow!("Human: stop trigger missing"))?
                    .to_string(),
            )?;
            (order_state(order.status()), quantity, price.0)
        };
        let mut move_state = match self.moves.get(&key) {
            Some(requested) if *requested == trigger_milli => MoveState::Done,
            Some(_) => MoveState::Working,
            None => MoveState::NotAttempted,
        };
        if self
            .fills
            .get(&key)
            .is_some_and(|fills| fills.filled_units() > 0)
        {
            move_state = MoveState::Working;
        }
        Ok(Some(StopSnapshot {
            move_state,
            key,
            event_date: position.event_date,
            state,
            quantity,
            filled_units,
            fill_price_milli: outcome.and_then(|r| r.fill_price).map(|p| p.0),
            trigger_milli,
            entry_filled_units,
            exit_filled_units,
        }))
    }

    fn summary(&self, restored: &Replay, rows: &[Row], reason: &str) -> anyhow::Result<()> {
        let cache = self.cache();
        let open = cache.orders_open_count(None, Some(&self.instrument), None, None, None);
        let net = self.broker_units()?;
        let date = OffsetDateTime::now_utc()
            .to_offset(self.plan.utc_offset)
            .date()
            .to_string();
        let legs: Vec<_> = self
            .plan
            .legs
            .iter()
            .map(|leg| {
                let state = if restored.positions.iter().any(|p| p.leg == leg.name) {
                    "position open".to_owned()
                } else {
                    rows.iter()
                        .rev()
                        .find(|row| row.leg == leg.name && row.tokyo_date == date)
                        .map_or_else(
                            || "waiting".to_owned(),
                            |row| format!("{:?}/{:?}", row.action, row.status),
                        )
                };
                format!("{}={state}", leg.name)
            })
            .collect();
        self.finish(
            0,
            &format!(
                "forward {reason} open_orders={open} position_net={net} {}",
                legs.join(" ")
            ),
        );
        Ok(())
    }

    fn stop_if_killed(
        &self,
        journal: &Journal<'_>,
        restored: &Replay,
        now: OffsetDateTime,
    ) -> anyhow::Result<bool> {
        if !restored.killed && !self.kill.try_exists()? {
            return Ok(false);
        }
        if !self.dry_run {
            let input = restored.input(now, &self.plan.legs[0].name, self.quote, None, &self.plan);
            forward_control::evaluate(
                &self.plan,
                journal,
                &input,
                &self.kill,
                self.halt.as_deref(),
            )?;
        }
        self.finish(4, "forward kill file or journal kill present");
        Ok(true)
    }

    fn evaluate(&mut self) -> anyhow::Result<()> {
        let now = OffsetDateTime::now_utc();
        let local = now.to_offset(self.plan.utc_offset);
        let journal = journal(&self.journal, &self.plan);
        let rows = journal.read_or_empty().map_err(anyhow::Error::msg)?;
        let mut restored = replay(&journal, &rows, self.quote)?;
        let first = &self.plan.legs[0];
        let control = restored.input(now, &first.name, self.quote, None, &self.plan);
        if self.stop_if_killed(&journal, &restored, now)? {
            return Ok(());
        }
        if self.until.is_some_and(|until| local.time() >= until) {
            return self.summary(&restored, &rows, "reached until");
        }
        if !restored.positions.is_empty() && self.quote.is_none() {
            return Ok(());
        }
        let halted = if self.dry_run {
            self.halt.as_ref().map_or(Ok(false), |p| p.try_exists())?
        } else {
            match forward_control::evaluate(
                &self.plan,
                &journal,
                &control,
                &self.kill,
                self.halt.as_deref(),
            )? {
                Control::Killed => {
                    self.finish(4, "forward kill file present");
                    return Ok(());
                }
                Control::Halted => true,
                Control::Continue => false,
            }
        };
        let cache = self.cache();
        for order in &mut restored.orders {
            if order.state == OrderState::Unknown
                && let Some(live) = cache.order(&reference(&order.key)?)
            {
                order.state = order_state(live.status());
                order.filled_units = live
                    .filled_qty()
                    .as_decimal()
                    .normalize()
                    .to_string()
                    .parse()?;
            }
        }
        if let Some(state) = &self.state {
            restored
                .orders
                .retain(|o| !state.permits.contains_key(&o.key));
        }
        let open_orders = cache.orders_open_count(None, Some(&self.instrument), None, None, None)
            + cache.orders_inflight_count(None, Some(&self.instrument), None, None, None);
        let mut all_done = self.plan.event_dates.contains(&local.date())
            && restored.positions.is_empty()
            && open_orders == 0
            && restored.orders.iter().all(|o| {
                o.state == OrderState::Done
                    || (o.state == OrderState::Refused && o.event_date < local.date())
            });
        for leg in &self.plan.legs {
            let stop = restored
                .positions
                .iter()
                .find(|p| p.leg == leg.name)
                .map(|p| self.stop_snapshot(&rows, &restored, p))
                .transpose()?
                .flatten();
            let mut input = restored.input(now, &leg.name, self.quote, stop.as_ref(), &self.plan);
            if self.state.as_ref().is_some_and(|state| {
                rows.iter().any(|r| {
                    r.leg == leg.name
                        && r.tokyo_date == local.date().to_string()
                        && state.permits.contains_key(&r.idem_key)
                })
            }) {
                input.entry_attempted = false;
            }
            input.halted |= halted;
            anyhow::ensure!(
                !restored.orders.iter().any(|order| {
                    order.leg == leg.name
                        && order.event_date == local.date()
                        && order.state == OrderState::Refused
                }),
                "Human: unresolved refused order for {}",
                leg.name
            );
            all_done &= input.entry_attempted;
            let action = decide::next(&self.plan, &input);
            match action {
                Action::Human | Action::Alert => anyhow::bail!("{action:?}: {}", leg.name),
                Action::Killed => {
                    self.finish(4, "forward killed");
                    return Ok(());
                }
                Action::Wait => {}
                _ if self.dry_run => eprintln!("forward dry-run {}: {action:?}", leg.name),
                _ => {
                    let leg_name = leg.name.clone();
                    let position = restored.positions.iter().find(|p| p.leg == leg_name);
                    return self.execute(action, &leg_name, position, now, &rows);
                }
            }
        }
        if all_done {
            self.summary(&restored, &rows, "all legs finished")?;
        }
        Ok(())
    }

    fn execute(
        &mut self,
        action: Action,
        leg_name: &str,
        position: Option<&decide::Position>,
        now: OffsetDateTime,
        rows: &[Row],
    ) -> anyhow::Result<()> {
        let journal = journal(&self.journal, &self.plan);
        let leg = self
            .plan
            .legs
            .iter()
            .find(|l| l.name == leg_name)
            .ok_or_else(|| anyhow::anyhow!("Human: unknown leg"))?;
        let state = self
            .state
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("Human: strategy not prepared"))?;
        let (permit, stop_permit) = match &action {
            Action::Open { key, units, .. }
            | Action::Submit {
                key,
                units,
                kind: Kind::Entry,
                ..
            } => {
                let Some(permit) = state.permits.remove(key) else {
                    return Self::record_entry_outcome(
                        &journal,
                        &self.plan,
                        leg,
                        now,
                        Status::Missed,
                        "started_after_declaration_window",
                    );
                };
                let stop = if matches!(action, Action::Open { .. }) {
                    let mut row = entry_intent(&self.plan, leg, now)?;
                    row.action = Record::Exit;
                    row.idem_key = stop_key(&row.event_id);
                    row.side = Some(if side(&leg.direction) == Side::Buy {
                        Side::Sell
                    } else {
                        Side::Buy
                    });
                    row.units = Some(*units);
                    Some(journal.begin(row).map_err(anyhow::Error::msg)?)
                } else {
                    None
                };
                (Some(permit), stop)
            }
            Action::Submit {
                key,
                direction,
                units,
                exit_late,
                ..
            } => {
                let position =
                    position.ok_or_else(|| anyhow::anyhow!("Human: exit without position"))?;
                let mut row = entry_intent(
                    &self.plan,
                    leg,
                    now.to_offset(self.plan.utc_offset)
                        .replace_date(position.event_date),
                )?;
                row.recorded_at = now.format(&Rfc3339)?;
                row.declared_at = Some(row.recorded_at.clone());
                row.action = Record::Exit;
                row.idem_key.clone_from(key);
                row.side = Some(side(direction));
                row.units = Some(*units);
                row.reason = exit_late.then(|| "exit_late".to_owned());
                (Some(journal.begin(row).map_err(anyhow::Error::msg)?), None)
            }
            Action::CancelStop { key } | Action::Cancel { key } => {
                let mut row = declaration(rows, key)?.clone();
                row.action = Record::Cancel;
                row.recorded_at = now.format(&Rfc3339)?;
                row.side = None;
                row.units = None;
                journal.append(&row).map_err(anyhow::Error::msg)?;
                (None, None)
            }
            Action::MoveStop { key, trigger_milli } => {
                self.moves.insert(key.clone(), *trigger_milli);
                (None, None)
            }
            Action::MissedLate | Action::SkippedSpread | Action::SkippedSize | Action::Halted => {
                let (status, reason) = match action {
                    Action::MissedLate => (Status::Missed, "late"),
                    Action::SkippedSpread => (Status::Skipped, "spread"),
                    Action::SkippedSize => (Status::Skipped, "size"),
                    _ => (Status::Skipped, "halt"),
                };
                let key = entry_intent(&self.plan, leg, now)?.idem_key;
                state.permits.remove(&key);
                return Self::record_entry_outcome(&journal, &self.plan, leg, now, status, reason);
            }
            _ => anyhow::bail!("Human: invalid dispatch action"),
        };
        let kill = self.kill.clone();
        let context = OrderContext {
            trader: self
                .trader_id()
                .ok_or_else(|| anyhow::anyhow!("missing trader"))?,
            strategy: self
                .strategy_id()
                .ok_or_else(|| anyhow::anyhow!("missing strategy"))?,
            instrument: self.instrument,
            timestamp: self.clock().timestamp_ns(),
            kill_file: &kill,
        };
        dispatch(
            &mut NautilusSink(self),
            action,
            permit,
            stop_permit,
            context,
        )
    }

    fn record_entry_outcome(
        journal: &Journal<'_>,
        plan: &Plan,
        leg: &Leg,
        now: OffsetDateTime,
        status: Status,
        reason: &str,
    ) -> anyhow::Result<()> {
        let mut row = entry_intent(plan, leg, now)?;
        row.status = status;
        row.side = None;
        row.declared_at = None;
        row.reason = Some(reason.into());
        journal.append(&row).map_err(anyhow::Error::msg)
    }

    fn book(&mut self, event: OrderEventAny) -> anyhow::Result<()> {
        let id = event.client_order_id();
        let (event, rejected) = match event {
            OrderEventAny::Filled(fill) => (
                Event::Filled {
                    fill: Box::new(fill),
                    complete: self
                        .cache()
                        .order(&id)
                        .is_some_and(|o| o.status() == OrderStatus::Filled),
                },
                false,
            ),
            OrderEventAny::Canceled(_) => (Event::Canceled, false),
            OrderEventAny::Expired(_) => (Event::Expired, false),
            OrderEventAny::Rejected(_) | OrderEventAny::Denied(_) => (Event::Rejected, true),
            OrderEventAny::ModifyDenied(_) | OrderEventAny::ModifyRejected(_) => {
                anyhow::bail!("Alert: stop modification rejected")
            }
            OrderEventAny::CancelDenied(_) | OrderEventAny::CancelRejected(_) => {
                anyhow::bail!("Human: cancellation rejected")
            }
            OrderEventAny::FillVoided(_) => anyhow::bail!("Human: broker voided a fill"),
            _ => return Ok(()),
        };
        let key = id.to_string();
        let journal = journal(&self.journal, &self.plan);
        let rows = journal.read().map_err(anyhow::Error::msg)?;
        let intent = declaration(&rows, &key)?;
        let cache = self.cache();
        let order = cache
            .order(&id)
            .ok_or_else(|| anyhow::anyhow!("Human: order missing from cache"))?;
        if !self.fills.contains_key(&key) {
            let action = trade_action(&self.plan, intent);
            let entry = rows.iter().rev().find(|r| {
                r.event_id == intent.event_id && r.action == Record::Entry && r.fill_price.is_some()
            });
            let context = FillContext {
                intent: intent.clone(),
                action,
                entry_price_milli: entry.and_then(|r| r.fill_price).map(|m| m.0),
                entry_side: entry.and_then(|r| r.side),
                total_cost_milli: Some(0),
                equity_before_milli: None,
            };
            let requested = order
                .quantity()
                .as_decimal()
                .normalize()
                .to_string()
                .parse()?;
            self.fills
                .insert(key.clone(), OrderLifecycle::new(requested, context)?);
        }
        let lifecycle = self
            .fills
            .get_mut(&key)
            .ok_or_else(|| anyhow::anyhow!("Human: missing lifecycle"))?;
        lifecycle.on_event(&journal, event)?;
        if order.is_closed() {
            self.fills.remove(&key);
        }
        anyhow::ensure!(!rejected, "Human: order rejected: {key}");
        Ok(())
    }

    fn tick(&mut self) {
        if !self.finished()
            && let Err(error) = self.evaluate()
        {
            self.finish(6, &format!("forward: {error:#}"));
        }
    }
}

impl DataActor for Forward {
    fn on_start(&mut self) -> anyhow::Result<()> {
        if let Err(error) = self.start() {
            self.finish(6, &format!("forward startup: {error:#}"));
        }
        Ok(())
    }

    fn on_stop(&mut self) -> anyhow::Result<()> {
        self.clock().cancel_timer("forward-poll");
        self.unsubscribe_quotes(self.instrument, Some(ClientId::new("SAXO")), None);
        Ok(())
    }

    fn on_quote(&mut self, quote: &QuoteTick) -> anyhow::Result<()> {
        if quote.instrument_id != self.instrument || self.finished() {
            return Ok(());
        }
        match serde_json::from_str::<Milli>(&quote.bid_price.to_string()).and_then(|bid| {
            serde_json::from_str::<Milli>(&quote.ask_price.to_string()).map(|ask| Quote {
                bid_milli: bid.0,
                ask_milli: ask.0,
            })
        }) {
            Ok(quote) => {
                self.quote = Some(quote);
                self.tick();
            }
            Err(error) => self.finish(6, &format!("Human: invalid quote: {error}")),
        }
        Ok(())
    }

    fn on_time_event(&mut self, _: &TimeEvent) -> anyhow::Result<()> {
        self.tick();
        Ok(())
    }
}

nautilus_strategy!(Forward, {
    fn external_order_instrument_ids(&self) -> Option<Vec<InstrumentId>> {
        Some(vec![self.instrument])
    }
    fn on_position_event(&mut self, _: PositionEvent) {
        self.tick();
    }
    fn on_order_event(&mut self, event: OrderEventAny) {
        if !self.dry_run
            && self.state.is_some()
            && let Err(error) = self.book(event)
        {
            self.finish(6, &format!("forward order event: {error:#}"));
        }
        self.tick();
    }
});
