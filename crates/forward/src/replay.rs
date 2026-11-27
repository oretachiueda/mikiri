use crate::{
    decide::{Cancellation, Input, Kind, Order, OrderState, Position, Quote, StopSnapshot},
    journal::{Action, Environment, Journal, Row, Side, Status},
    plan::Plan,
};
use std::collections::{HashMap, HashSet};
use time::{Date, OffsetDateTime, format_description::well_known::Iso8601};

#[derive(Debug)]
pub struct Replay {
    pub orders: Vec<Order>,
    pub cancellations: Vec<Cancellation>,
    pub positions: Vec<Position>,
    pub realized_milli: i64,
    pub halted: bool,
    pub killed: bool,
    pub used_keys: HashSet<String>,
    pub needs_broker_stop: Vec<String>,
    env: Environment,
    attempted: HashSet<(String, Date)>,
}
impl Replay {
    pub fn input<'a>(
        &'a self,
        now: OffsetDateTime,
        leg: &'a str,
        quote: Option<Quote>,
        stop: Option<&'a StopSnapshot>,
        plan: &Plan,
    ) -> Input<'a> {
        let date = now.to_offset(plan.utc_offset).date();
        Input {
            now,
            env: self.env,
            leg,
            quote,
            positions: &self.positions,
            realized_milli: self.realized_milli,
            halted: self.halted,
            kill: self.killed,
            orders: &self.orders,
            cancellations: &self.cancellations,
            entry_attempted: self.attempted.contains(&(leg.to_owned(), date)),
            stop,
        }
    }
}
#[derive(Default)]
struct Holding {
    units: i64,
    entry_milli: i64,
}
fn fill(s: Status) -> bool {
    matches!(s, Status::Filled | Status::Partial)
}
fn replay(
    rows: &[Row],
    plan: &Plan,
    env: Environment,
    bids: &HashMap<String, i64>,
) -> Result<Replay, String> {
    let mut replacements: HashMap<&str, usize> = HashMap::new();
    for (i, r) in rows.iter().enumerate() {
        if r.correction_of.is_some() && fill(r.status) {
            replacements.insert(&r.idem_key, i);
        }
    }
    let mut output = Replay {
        orders: Vec::new(),
        cancellations: Vec::new(),
        positions: Vec::new(),
        realized_milli: 0,
        halted: false,
        killed: false,
        used_keys: HashSet::new(),
        needs_broker_stop: Vec::new(),
        env,
        attempted: HashSet::new(),
    };
    let mut holdings: HashMap<(String, Date), Holding> = HashMap::new();
    for original in rows {
        if original.correction_of.is_some() {
            continue;
        }
        let r = if fill(original.status) {
            replacements
                .get(original.idem_key.as_str())
                .map_or(original, |&i| &rows[i])
        } else {
            original
        };
        let date = Date::parse(&r.tokyo_date, &Iso8601::DATE).map_err(|_| "invalid event date")?;
        output.used_keys.insert(r.idem_key.clone());
        if matches!(r.action, Action::Entry) {
            output.attempted.insert((r.leg.clone(), date));
        }
        match r.action {
            Action::Halt => output.halted = true,
            Action::Kill => output.killed = true,
            Action::Resume => {
                output.halted = false;
                output.killed = false;
            }
            _ => {}
        }
        let recorded_intent = r.action == Action::Declare
            && r.status == Status::Scheduled
            && r.units.is_some()
            && r.side.is_some();
        let resolved_intent = recorded_intent
            && rows
                .iter()
                .any(|other| other.idem_key == r.idem_key && fill(other.status));
        if !resolved_intent
            && (matches!(
                r.action,
                Action::Entry | Action::Exit | Action::StopExit | Action::ManualClose
            ) || recorded_intent)
            && matches!(
                r.status,
                Status::Scheduled | Status::Filled | Status::Partial
            )
        {
            let entry_intent =
                recorded_intent && r.idem_key.ends_with(&format!("-ENTRY-{}", env.name()));
            let kind = if r.action == Action::Entry || entry_intent {
                Kind::Entry
            } else {
                Kind::Exit
            };
            if entry_intent {
                output.attempted.insert((r.leg.clone(), date));
            }
            let state = if r.status == Status::Filled {
                OrderState::Done
            } else {
                OrderState::Unknown
            };
            output.orders.push(Order {
                leg: r.leg.clone(),
                event_date: date,
                key: r.idem_key.clone(),
                kind,
                state,
                filled_units: r.units.unwrap_or(0),
            });
        }
        if r.action == Action::Cancel && r.status == Status::Scheduled {
            output.cancellations.push(Cancellation {
                leg: r.leg.clone(),
                event_date: date,
                order_key: r.idem_key.clone(),
                state: OrderState::Unknown,
            });
        }
        if !fill(r.status) {
            continue;
        }
        let units = r.units.ok_or("missing fill units")?;
        let signed = match r.side.ok_or("missing fill side")? {
            Side::Buy => units,
            Side::Sell => units.checked_neg().ok_or("units overflow")?,
        };
        let holding = holdings.entry((r.leg.clone(), date)).or_default();
        if r.action == Action::Entry {
            if holding.units != 0 {
                return Err("multiple entries for event".into());
            }
            holding.units = signed;
            holding.entry_milli = r.fill_price.ok_or("missing entry price")?.0;
        } else {
            if holding.units == 0
                || holding.units.signum() == signed.signum()
                || signed.unsigned_abs() > holding.units.unsigned_abs()
            {
                return Err("unmatched or excess close".into());
            }
            holding.units = holding.units.checked_add(signed).ok_or("units overflow")?;
            output.realized_milli = output
                .realized_milli
                .checked_add(r.realized_pnl_jpy_ex_swap.ok_or("missing realized pnl")?.0)
                .ok_or("pnl overflow")?;
        }
    }
    for ((leg, event_date), h) in holdings {
        if h.units == 0 {
            continue;
        }
        let spec = plan
            .legs
            .iter()
            .find(|l| l.name == leg)
            .ok_or("open leg absent from plan")?;
        if spec.stop.enabled && !output.needs_broker_stop.contains(&leg) {
            output.needs_broker_stop.push(leg.clone());
        }
        let bid = *bids
            .get(&leg)
            .filter(|b| **b > 0)
            .ok_or("missing bid for open position")?;
        output.positions.push(Position {
            leg,
            event_date,
            units: h.units,
            entry_milli: h.entry_milli,
            bid_milli: bid,
        });
    }
    Ok(output)
}
impl Journal<'_> {
    pub fn replay(&self, bids: &HashMap<String, i64>) -> Result<Replay, String> {
        replay(&self.read()?, self.plan(), self.environment(), bids)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        decide::{self, Action as Decision},
        journal::Milli,
        plan::tests::{VALID, read},
    };
    use serde_json::json;
    use time::format_description::well_known::Rfc3339;
    fn plan() -> Plan {
        read(
            &VALID
                .replace("enabled = true", "enabled = false")
                .replace("stop = 1", "stop = 0"),
            0o600,
        )
        .expect("plan")
    }
    fn now() -> OffsetDateTime {
        OffsetDateTime::parse("2098-01-01T03:15:00+01:00", &Rfc3339).expect("time")
    }
    fn row() -> Row {
        serde_json::from_value(json!({"version":1,"env":"SIM","event_id":"A-20980101","leg":"A","tokyo_date":"2098-01-01","action":"entry","idem_key":"A-20980101-ENTRY-SIM","status":"filled","recorded_at":"2098-01-01T03:15:00+01:00","order_id":"synthetic","side":"Buy","units":10,"fill_time":"2098-01-01T03:15:00+01:00","fill_price":12.345,"source_sha256":"a".repeat(64)})).expect("row")
    }
    fn close(pnl: i64) -> Row {
        let mut r = row();
        r.action = Action::Exit;
        r.side = Some(Side::Sell);
        r.idem_key = format!("{}-EXIT-SIM-1", r.event_id);
        r.realized_pnl_jpy_ex_swap = Some(Milli(pnl));
        r
    }
    fn evaluate(rows: &[Row], bid: i64) -> Replay {
        replay(
            rows,
            &plan(),
            Environment::Sim,
            &HashMap::from([("A".into(), bid)]),
        )
        .expect("replay")
    }
    fn entry_decision(plan: &Plan, r: &Replay, ask: i64) -> Decision {
        let mut input = r.input(
            now(),
            "A",
            Some(Quote {
                bid_milli: ask - 1,
                ask_milli: ask,
            }),
            None,
            plan,
        );
        input.positions = &[];
        input.entry_attempted = false;
        input.orders = &[];
        decide::next(plan, &input)
    }
    #[test]
    fn test_size_under_minimum_none() {
        let p = plan();
        let r = evaluate(&[], 1);
        assert_eq!(entry_decision(&p, &r, 1_000_000), Decision::SkippedSize);
    }
    #[test]
    fn test_size_cap() {
        let p = plan();
        let r = evaluate(&[], 1);
        assert!(matches!(
            entry_decision(&p, &r, 1005),
            Decision::Submit { units: 100, .. }
        ));
    }
    #[test]
    fn test_halted_boundary() {
        let p = plan();
        let mut entry = row();
        entry.units = Some(10);
        let exit = close(p.halt_threshold_milli - p.base_capital_milli);
        let r = evaluate(&[entry, exit], 1);
        assert_eq!(entry_decision(&p, &r, 1005), Decision::Halted);
    }
    #[test]
    fn test_open_mtm_long_short() {
        let p = plan();
        let r = evaluate(&[row()], 12_445);
        let input = r.input(now(), "A", None, None, &p);
        let gain = (input.positions[0].bid_milli - input.positions[0].entry_milli)
            * input.positions[0].units;
        assert_eq!(gain, 1000);
        let mut short = row();
        short.side = Some(Side::Sell);
        let r = evaluate(&[short], 12_245);
        let input = r.input(now(), "A", None, None, &p);
        assert_eq!(
            (input.positions[0].bid_milli - input.positions[0].entry_milli)
                * input.positions[0].units,
            1000
        );
    }
    #[test]
    fn test_sizing_equity() {
        let p = plan();
        let r = evaluate(&[row()], 12_445);
        let position = &r.positions[0];
        assert_eq!(
            i128::from(p.base_capital_milli)
                + i128::from(r.realized_milli)
                + i128::from(position.units)
                    * i128::from(position.bid_milli - position.entry_milli),
            1_001_000
        );
    }
    #[test]
    fn test_correction_replaces_pnl() {
        let first = close(1000);
        let mut correction = close(2500);
        correction.correction_of = Some(first.idem_key.clone());
        let r = evaluate(&[row(), first, correction], 1);
        assert_eq!(r.realized_milli, 2500);
        assert!(r.positions.is_empty());
    }
    #[test]
    fn replay_uses_keys_and_preserves_unknown() {
        let mut r = row();
        r.status = Status::Scheduled;
        let result = evaluate(&[r], 1);
        assert_eq!(result.orders[0].state, OrderState::Unknown);
        assert!(result.used_keys.contains(&result.orders[0].key));
        assert_eq!(
            decide::next(&plan(), &result.input(now(), "A", None, None, &plan())),
            Decision::Wait
        );
    }
    #[test]
    fn replay_requests_stop_from_broker() {
        let p = read(VALID, 0o600).expect("plan");
        let restored = replay(
            &[row()],
            &p,
            Environment::Sim,
            &HashMap::from([("A".into(), 12345)]),
        )
        .expect("replay");
        assert_eq!(restored.needs_broker_stop, vec!["A"]);
        assert_eq!(
            decide::next(&p, &restored.input(now(), "A", None, None, &p)),
            Decision::Human
        );
        let stop = StopSnapshot {
            key: "stop".into(),
            event_date: now().date(),
            state: OrderState::Working,
            quantity: 10,
            filled_units: 0,
            fill_price_milli: None,
            trigger_milli: 12_100,
            entry_filled_units: 10,
            exit_filled_units: 0,
            move_state: crate::decide::MoveState::NotAttempted,
        };
        assert_eq!(
            decide::next(&p, &restored.input(now(), "A", None, Some(&stop), &p)),
            Decision::Wait
        );
        let mut changed = stop.clone();
        changed.trigger_milli -= 5;
        assert!(matches!(
            decide::next(&p, &restored.input(now(), "A", None, Some(&changed), &p)),
            Decision::MoveStop {
                trigger_milli: 12_100,
                ..
            }
        ));
    }
    #[test]
    fn journal_round_trip_applies_later_entry_correction_at_original_position() {
        let p = plan();
        let dir = tempfile::tempdir().expect("dir");
        let j = Journal::new(
            dir.path().join("journal"),
            Environment::Sim,
            &p,
            HashSet::new(),
        );
        let entry = row();
        let exit = close(1250);
        j.append(&entry).expect("entry");
        j.append(&exit).expect("exit");
        let mut corrected = entry.clone();
        corrected.fill_price = Some(Milli(12_400));
        corrected.correction_of = Some(entry.idem_key.clone());
        j.append(&corrected).expect("correction");
        let state = j.replay(&HashMap::new()).expect("replay");
        assert_eq!(state.realized_milli, 1250);
        assert!(state.positions.is_empty());
        assert_eq!(state.orders.len(), 2);
    }
    #[test]
    fn durable_intent_replays_unknown_until_fill() {
        let p = plan();
        let dir = tempfile::tempdir().expect("dir");
        let j = Journal::new(
            dir.path().join("journal"),
            Environment::Sim,
            &p,
            HashSet::new(),
        );
        let fill = row();
        let mut intent = fill.clone();
        intent.status = Status::Scheduled;
        intent.order_id = None;
        intent.fill_time = None;
        intent.fill_price = None;
        intent.source_sha256 = None;
        let _permit = j.begin(intent).expect("permit");
        let pending = j.replay(&HashMap::new()).expect("pending");
        assert_eq!(pending.orders.len(), 1);
        assert_eq!(pending.orders[0].state, OrderState::Unknown);
        j.append(&fill).expect("fill");
        let restored = j
            .replay(&HashMap::from([("A".into(), 12_345)]))
            .expect("restored");
        assert_eq!(restored.orders.len(), 1);
        assert_eq!(restored.orders[0].state, OrderState::Done);
        assert_eq!(restored.positions[0].units, 10);
    }
}
