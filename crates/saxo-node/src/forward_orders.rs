use forward::{
    decide::{Action, Kind},
    journal::{Permit, Side},
    plan::Direction,
};
use nautilus_core::{UUID4, UnixNanos};
use nautilus_model::{
    enums::{ContingencyType, OrderSide, TimeInForce, TriggerType},
    identifiers::{ClientOrderId, InstrumentId, OrderListId, StrategyId, TraderId},
    orders::{MarketOrder, OrderAny, StopMarketOrder},
    types::{Price, Quantity},
};

pub fn reference(value: &str) -> anyhow::Result<ClientOrderId> {
    anyhow::ensure!(
        !value.is_empty()
            && value.len() <= 50
            && value
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-'),
        "Human: invalid external reference"
    );
    ClientOrderId::new_checked(value).map_err(|e| anyhow::anyhow!("{e}"))
}
fn price(value: i64) -> anyhow::Result<Price> {
    anyhow::ensure!(value > 0, "Human: invalid trigger");
    format!("{}.{:03}", value / 1000, value % 1000)
        .parse()
        .map_err(|e: String| anyhow::anyhow!("{e}"))
}
pub struct OrderContext<'a> {
    pub trader: TraderId,
    pub strategy: StrategyId,
    pub instrument: InstrumentId,
    pub timestamp: UnixNanos,
    pub kill_file: &'a std::path::Path,
}
/// Builds market and optional protective orders from a consumed journal permit.
///
/// ```compile_fail
/// use saxo_node::forward_orders::build;
/// let _ = build((), None, todo!(), todo!());
/// ```
pub fn build(
    permit: Permit,
    stop_permit: Option<Permit>,
    action: Action,
    context: OrderContext<'_>,
) -> anyhow::Result<Vec<OrderAny>> {
    anyhow::ensure!(!context.kill_file.try_exists()?, "Human: kill file present");
    let OrderContext {
        trader,
        strategy,
        instrument,
        timestamp: ts,
        ..
    } = context;
    let (key, direction, units, trigger, kind) = match action {
        Action::Open {
            key,
            direction,
            units,
            stop_trigger_milli,
        } => (key, direction, units, Some(stop_trigger_milli), Kind::Entry),
        Action::Submit {
            key,
            kind,
            direction,
            units,
            ..
        } => (key, direction, units, None, kind),
        _ => anyhow::bail!("Human: action cannot create orders"),
    };
    let row_side = match direction {
        Direction::Buy => Side::Buy,
        Direction::Sell => Side::Sell,
    };
    let order_side = match direction {
        Direction::Buy => OrderSide::Buy,
        Direction::Sell => OrderSide::Sell,
    };
    let order_id = reference(&key)?;
    let leg = permit.leg().to_owned();
    let authorized = permit
        .authorize(&key, &leg, row_side, units)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    anyhow::ensure!(
        matches!(
            (kind, authorized.action),
            (Kind::Entry, forward::journal::Action::Entry)
                | (Kind::Exit, forward::journal::Action::Exit)
        ),
        "Human: permit action mismatch"
    );
    let quantity = Quantity::from(u64::try_from(units)?);
    let (list, stop_id) = if trigger.is_some() {
        let stop_key = format!(
            "{}-EXIT-{}-1",
            authorized.event_id,
            match authorized.env {
                forward::journal::Environment::Sim => "SIM",
                forward::journal::Environment::Live => "LIVE",
            }
        );
        let stop_side = match row_side {
            Side::Buy => Side::Sell,
            Side::Sell => Side::Buy,
        };
        let stop = stop_permit.ok_or_else(|| anyhow::anyhow!("Human: missing stop permit"))?;
        let stop_row = stop
            .authorize(&stop_key, &leg, stop_side, units)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        anyhow::ensure!(
            stop_row.action == forward::journal::Action::Exit,
            "Human: invalid stop permit"
        );
        (
            Some(OrderListId::new(format!("L-{}", UUID4::new()))),
            Some(reference(&stop_key)?),
        )
    } else {
        anyhow::ensure!(stop_permit.is_none(), "Human: unexpected stop permit");
        (None, None)
    };
    let market = MarketOrder::new_checked(
        trader,
        strategy,
        instrument,
        order_id,
        order_side,
        quantity,
        TimeInForce::Day,
        UUID4::new(),
        ts,
        false,
        false,
        list.map(|_| ContingencyType::Oto),
        list,
        stop_id.map(|id| vec![id]),
        None,
        None,
        None,
        None,
        None,
    )?;
    let mut orders = vec![market.into()];
    if let (Some(trigger), Some(list), Some(stop_id)) = (trigger, list, stop_id) {
        let side = match order_side {
            OrderSide::Buy => OrderSide::Sell,
            OrderSide::Sell => OrderSide::Buy,
        };
        let stop = StopMarketOrder::new_checked(
            trader,
            strategy,
            instrument,
            stop_id,
            side,
            quantity,
            price(trigger)?,
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
            Some(order_id),
            None,
            None,
            None,
            None,
            UUID4::new(),
            ts,
        )?;
        orders.push(stop.into());
    }
    Ok(orders)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use forward::{
        journal::{Environment, Journal, Row},
        plan::Plan,
    };
    use nautilus_model::{enums::OrderType, orders::Order};
    use std::{collections::HashSet, fs, os::unix::fs::PermissionsExt};
    use time::OffsetDateTime;
    const PLAN: &str = r#"
instrument_id="SYNTH.SAXO"
price_increment_milli=5
utc_offset_seconds=3600
base_capital_milli=1000000
halt_threshold_milli=100
expires="2099-01-01"
event_dates=["2098-01-01"]
[blackout]
start="01:00:00"
end="02:00:00"
[sizing]
unit_cap=100
absolute_cap=200
multiplier=2
lot_size=10
minimum_units=10
[[legs]]
name="A"
direction="buy"
max_spread_milli=9
[legs.entry]
start="03:10:00"
end="03:20:00"
[legs.exit]
start="04:10:00"
end="04:20:00"
[legs.stop]
enabled=true
width_ppm=20000
[legs.order_limits]
entry=1
stop=1
exit=2
cancel=1
"#;
    pub(crate) fn plan(dir: &std::path::Path) -> Plan {
        let path = dir.join("plan");
        fs::write(&path, PLAN).expect("write");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("chmod");
        Plan::read(&path, OffsetDateTime::UNIX_EPOCH).expect("plan")
    }
    pub(crate) fn intent() -> Row {
        serde_json::from_value(serde_json::json!({"version":1,"env":"SIM","event_id":"A-20980101","leg":"A","tokyo_date":"2098-01-01","action":"entry","idem_key":"A-20980101-ENTRY-SIM","status":"scheduled","recorded_at":"2098-01-01T03:00:00+01:00","side":"Buy"})).expect("intent")
    }
    pub(crate) fn ids() -> (TraderId, StrategyId, InstrumentId, UnixNanos) {
        (
            TraderId::from("TRADER-001"),
            StrategyId::from("TEST-001"),
            "SYNTH.SAXO".parse().expect("instrument"),
            0.into(),
        )
    }
    #[test]
    fn permit_builds_entry_with_protective_stop() {
        let dir = tempfile::tempdir().expect("dir");
        let p = plan(dir.path());
        let j = Journal::new(
            dir.path().join("journal"),
            Environment::Sim,
            &p,
            HashSet::new(),
        );
        let i = intent();
        let key = i.idem_key.clone();
        let permit = j.begin(i).expect("permit");
        assert_eq!(j.read().expect("valid")[0].units, None);
        let mut stop_intent = intent();
        stop_intent.action = forward::journal::Action::Exit;
        stop_intent.side = Some(Side::Sell);
        stop_intent.units = Some(20);
        stop_intent.idem_key = format!("{}-EXIT-SIM-1", stop_intent.event_id);
        let stop_permit = j.begin(stop_intent).expect("stop permit");
        let (trader, strategy, instrument, ts) = ids();
        let orders = build(
            permit,
            Some(stop_permit),
            Action::Open {
                key: key.clone(),
                direction: Direction::Buy,
                units: 20,
                stop_trigger_milli: 12345,
            },
            OrderContext {
                trader,
                strategy,
                instrument,
                timestamp: ts,
                kill_file: &dir.path().join("kill"),
            },
        )
        .expect("build");
        assert_eq!(orders.len(), 2);
        assert_eq!(orders[0].client_order_id().to_string(), key);
        assert_eq!(orders[0].order_type(), OrderType::Market);
        assert_eq!(orders[0].quantity(), Quantity::from(20));
        assert_eq!(orders[1].order_type(), OrderType::StopMarket);
        assert_eq!(orders[1].order_side(), OrderSide::Sell);
        assert_eq!(
            orders[1].trigger_price().expect("trigger").to_string(),
            "12.345"
        );
        assert!(
            orders[1]
                .client_order_id()
                .to_string()
                .ends_with("EXIT-SIM-1")
        );
    }
    #[test]
    fn permit_rejects_excess_and_bad_reference_before_order() {
        let dir = tempfile::tempdir().expect("dir");
        let p = plan(dir.path());
        let j = Journal::new(
            dir.path().join("journal"),
            Environment::Sim,
            &p,
            HashSet::new(),
        );
        let i = intent();
        let key = i.idem_key.clone();
        let permit = j.begin(i).expect("permit");
        let (trader, strategy, instrument, ts) = ids();
        let err = build(
            permit,
            None,
            Action::Submit {
                key,
                kind: Kind::Entry,
                direction: Direction::Buy,
                units: 110,
                exit_late: false,
            },
            OrderContext {
                trader,
                strategy,
                instrument,
                timestamp: ts,
                kill_file: &dir.path().join("kill"),
            },
        )
        .expect_err("cap");
        assert!(err.to_string().contains("Human"));
        assert_eq!(j.read().expect("valid").len(), 1);
        assert!(reference(&"X".repeat(51)).is_err());
        assert!(reference("invalid_id").is_err());
    }
    #[test]
    fn actual_fill_event_is_recorded_and_revalidated() {
        use crate::forward_fills::{FillAccumulator, FillContext, Terminal};
        use forward::journal::{Action as JournalAction, Milli};
        use nautilus_model::{
            enums::LiquiditySide,
            events::OrderFilled,
            identifiers::{AccountId, TradeId, VenueOrderId},
            types::Currency,
        };
        let dir = tempfile::tempdir().expect("dir");
        let p = plan(dir.path());
        let j = Journal::new(
            dir.path().join("journal"),
            Environment::Sim,
            &p,
            HashSet::new(),
        );
        let entry = intent();
        let key = entry.idem_key.clone();
        let permit = j.begin(entry.clone()).expect("begin");
        let _orders = build(
            permit,
            None,
            Action::Submit {
                key: key.clone(),
                kind: Kind::Entry,
                direction: Direction::Buy,
                units: 20,
                exit_late: false,
            },
            OrderContext {
                trader: ids().0,
                strategy: ids().1,
                instrument: ids().2,
                timestamp: ids().3,
                kill_file: &dir.path().join("kill"),
            },
        )
        .expect("order");
        let fill = OrderFilled::new(
            ids().0,
            ids().1,
            ids().2,
            ClientOrderId::from(key.as_str()),
            VenueOrderId::from("V-001"),
            AccountId::from("SIM-001"),
            TradeId::from("T-001"),
            OrderSide::Buy,
            OrderType::Market,
            Quantity::from(20),
            "12.345".parse().expect("price"),
            Currency::JPY(),
            LiquiditySide::Taker,
            UUID4::new(),
            1_000_000_000_u64.into(),
            1_000_000_000_u64.into(),
            false,
            None,
            None,
            None,
        );
        let mut accumulator = FillAccumulator::new(20).expect("accumulator");
        accumulator.push(fill.clone()).expect("fill");
        accumulator
            .finish(
                &j,
                FillContext {
                    intent: entry,
                    action: JournalAction::Entry,
                    entry_price_milli: None,
                    entry_side: None,
                    total_cost_milli: None,
                    equity_before_milli: Some(900_000),
                },
                Terminal::Filled,
            )
            .expect("append");
        let rows = j.read().expect("journal validates");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].fill_price, Some(Milli(12_345)));
        assert_eq!(rows[1].units, Some(20));
        assert_eq!(rows[1].source_sha256.as_ref().expect("hash").len(), 64);

        let mut closing = intent();
        closing.action = JournalAction::Exit;
        closing.side = Some(Side::Sell);
        closing.units = Some(20);
        closing.idem_key = format!("{}-EXIT-SIM-2", closing.event_id);
        let _close = j.begin(closing.clone()).expect("close permit");
        let mut exit_fill = fill.clone();
        exit_fill.client_order_id = ClientOrderId::from(closing.idem_key.as_str());
        exit_fill.order_side = OrderSide::Sell;
        exit_fill.last_px = "12.500".parse().expect("price");
        let mut accumulator = FillAccumulator::new(20).expect("accumulator");
        accumulator.push(exit_fill).expect("fill");
        accumulator
            .finish(
                &j,
                FillContext {
                    intent: closing,
                    action: JournalAction::Exit,
                    entry_price_milli: Some(12_345),
                    entry_side: Some(Side::Buy),
                    total_cost_milli: Some(100),
                    equity_before_milli: None,
                },
                Terminal::Filled,
            )
            .expect("close fill");
        let rows = j.read().expect("valid close");
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[3].realized_pnl_jpy_ex_swap, Some(Milli(3000)));
    }
    #[test]
    fn restart_tracks_existing_or_records_missed_once() {
        use crate::forward_restart::{Resolution, reconcile_intent};
        let dir = tempfile::tempdir().expect("dir");
        let p = plan(dir.path());
        let path = dir.path().join("journal");
        let j = Journal::new(&path, Environment::Sim, &p, HashSet::new());
        let _permit = j.begin(intent()).expect("begin");
        let declaration = j.read().expect("read").remove(0);
        assert_eq!(
            reconcile_intent(&j, &declaration, true).expect("track"),
            Resolution::Track
        );
        assert_eq!(j.read().expect("read").len(), 1);
        let reopened = Journal::new(&path, Environment::Sim, &p, HashSet::new());
        assert_eq!(
            reconcile_intent(&reopened, &declaration, false).expect("miss"),
            Resolution::Missed
        );
        assert_eq!(
            reconcile_intent(&reopened, &declaration, false).expect("again"),
            Resolution::Missed
        );
        let rows = reopened.read().expect("read");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].reason.as_deref(), Some("restart"));
        assert!(reopened.begin(intent()).is_err());
    }
    pub(crate) fn synthetic_fill(
        key: &str,
        trade: &str,
        units: u64,
        price: &str,
    ) -> nautilus_model::events::OrderFilled {
        use nautilus_model::{
            enums::LiquiditySide,
            events::OrderFilled,
            identifiers::{AccountId, TradeId, VenueOrderId},
            types::Currency,
        };
        OrderFilled::new(
            ids().0,
            ids().1,
            ids().2,
            ClientOrderId::from(key),
            VenueOrderId::from("V-001"),
            AccountId::from("SIM-001"),
            TradeId::from(trade),
            OrderSide::Buy,
            OrderType::Market,
            Quantity::from(units),
            price.parse().expect("price"),
            Currency::JPY(),
            LiquiditySide::Taker,
            UUID4::new(),
            1_000_000_000_u64.into(),
            1_000_000_000_u64.into(),
            false,
            None,
            None,
            None,
        )
    }
    #[test]
    fn multiple_fills_are_one_terminal_weighted_row() {
        use crate::forward_fills::{FillAccumulator, FillContext, Terminal};
        use forward::journal::{Action as JournalAction, Milli, Status};
        let dir = tempfile::tempdir().expect("dir");
        let p = plan(dir.path());
        let j = Journal::new(
            dir.path().join("journal"),
            Environment::Sim,
            &p,
            HashSet::new(),
        );
        let entry = intent();
        let _permit = j.begin(entry.clone()).expect("begin");
        let key = entry.idem_key.clone();
        let mut acc = FillAccumulator::new(20).expect("accumulator");
        for (trade, units, price) in [
            ("T-1", 5, "10.000"),
            ("T-2", 5, "10.001"),
            ("T-3", 10, "10.001"),
        ] {
            acc.push(synthetic_fill(&key, trade, units, price))
                .expect("fill");
            assert_eq!(j.read().expect("read").len(), 1);
        }
        acc.finish(
            &j,
            FillContext {
                intent: entry,
                action: JournalAction::Entry,
                entry_price_milli: None,
                entry_side: None,
                total_cost_milli: None,
                equity_before_milli: Some(900_000),
            },
            Terminal::Filled,
        )
        .expect("terminal");
        let rows = j.read().expect("rows");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].units, Some(20));
        assert_eq!(rows[1].fill_price, Some(Milli(10_001)));
        assert_eq!(rows[1].status, Status::Filled);
    }
    #[test]
    fn canceled_partially_filled_order_writes_one_partial_row() {
        use crate::forward_fills::{FillAccumulator, FillContext, Terminal};
        use forward::journal::{Action as JournalAction, Milli, Status};
        let dir = tempfile::tempdir().expect("dir");
        let p = plan(dir.path());
        let j = Journal::new(
            dir.path().join("journal"),
            Environment::Sim,
            &p,
            HashSet::new(),
        );
        let entry = intent();
        let _permit = j.begin(entry.clone()).expect("begin");
        let key = entry.idem_key.clone();
        let mut acc = FillAccumulator::new(20).expect("accumulator");
        acc.push(synthetic_fill(&key, "T-1", 5, "10.000"))
            .expect("fill");
        acc.push(synthetic_fill(&key, "T-2", 5, "10.001"))
            .expect("fill");
        assert_eq!(j.read().expect("read").len(), 1);
        acc.finish(
            &j,
            FillContext {
                intent: entry,
                action: JournalAction::Entry,
                entry_price_milli: None,
                entry_side: None,
                total_cost_milli: None,
                equity_before_milli: Some(900_000),
            },
            Terminal::Canceled,
        )
        .expect("terminal");
        let rows = j.read().expect("rows");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].units, Some(10));
        assert_eq!(rows[1].fill_price, Some(Milli(10_001)));
        assert_eq!(rows[1].status, Status::Partial);
    }
    #[test]
    fn kill_after_permit_blocks_order_construction() {
        let dir = tempfile::tempdir().expect("dir");
        let p = plan(dir.path());
        let j = Journal::new(
            dir.path().join("journal"),
            Environment::Sim,
            &p,
            HashSet::new(),
        );
        let entry = intent();
        let key = entry.idem_key.clone();
        let permit = j.begin(entry).expect("permit");
        let kill = dir.path().join("kill");
        fs::write(&kill, "").expect("kill");
        let (trader, strategy, instrument, timestamp) = ids();
        let result = build(
            permit,
            None,
            Action::Submit {
                key,
                kind: Kind::Entry,
                direction: Direction::Buy,
                units: 20,
                exit_late: false,
            },
            OrderContext {
                trader,
                strategy,
                instrument,
                timestamp,
                kill_file: &kill,
            },
        );
        assert!(
            result
                .expect_err("killed")
                .to_string()
                .contains("kill file")
        );
        assert_eq!(j.read().expect("read").len(), 1);
    }
    #[test]
    fn reserved_stop_key_cannot_be_reissued_after_restart() {
        let dir = tempfile::tempdir().expect("dir");
        let p = plan(dir.path());
        let path = dir.path().join("journal");
        let j = Journal::new(&path, Environment::Sim, &p, HashSet::new());
        let mut stop = intent();
        stop.action = forward::journal::Action::Exit;
        stop.side = Some(Side::Sell);
        stop.units = Some(20);
        stop.idem_key = format!("{}-EXIT-SIM-1", stop.event_id);
        let _permit = j.begin(stop.clone()).expect("reserve");
        let before = fs::read(&path).expect("read");
        let reopened = Journal::new(&path, Environment::Sim, &p, HashSet::new());
        assert!(reopened.begin(stop).is_err());
        assert_eq!(fs::read(path).expect("read"), before);
    }
}
