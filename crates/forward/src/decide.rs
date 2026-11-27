use time::{Date, OffsetDateTime};

use crate::{
    journal::Environment,
    plan::{Direction, Leg, Plan},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderState {
    Working,
    Done,
    Refused,
    Unknown,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Entry,
    Exit,
}
#[derive(Debug)]
pub struct Order {
    pub leg: String,
    pub event_date: Date,
    pub key: String,
    pub kind: Kind,
    pub state: OrderState,
    pub filled_units: i64,
}
#[derive(Debug)]
pub struct Cancellation {
    pub leg: String,
    pub event_date: Date,
    pub order_key: String,
    pub state: OrderState,
}
#[derive(Debug)]
pub struct Position {
    pub leg: String,
    pub event_date: Date,
    pub units: i64,
    pub entry_milli: i64,
    pub bid_milli: i64,
}
#[derive(Debug, Clone, Copy)]
pub struct Quote {
    pub bid_milli: i64,
    pub ask_milli: i64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoveState {
    NotAttempted,
    Working,
    Done,
    Refused,
    Unknown,
}
#[derive(Debug, Clone)]
pub struct StopSnapshot {
    pub key: String,
    pub event_date: Date,
    pub state: OrderState,
    pub quantity: i64,
    pub filled_units: i64,
    pub fill_price_milli: Option<i64>,
    pub trigger_milli: i64,
    pub entry_filled_units: i64,
    pub exit_filled_units: i64,
    pub move_state: MoveState,
}
#[derive(Debug)]
pub struct Input<'a> {
    pub now: OffsetDateTime,
    pub env: Environment,
    pub leg: &'a str,
    pub quote: Option<Quote>,
    pub positions: &'a [Position],
    pub realized_milli: i64,
    pub halted: bool,
    pub kill: bool,
    pub orders: &'a [Order],
    pub cancellations: &'a [Cancellation],
    pub entry_attempted: bool,
    pub stop: Option<&'a StopSnapshot>,
}
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    Wait,
    Human,
    Killed,
    Alert,
    Open {
        key: String,
        direction: Direction,
        units: i64,
        stop_trigger_milli: i64,
    },
    MoveStop {
        key: String,
        trigger_milli: i64,
    },
    CancelStop {
        key: String,
    },
    Cancel {
        key: String,
    },
    MissedLate,
    SkippedSpread,
    SkippedSize,
    Halted,
    Submit {
        key: String,
        kind: Kind,
        direction: Direction,
        units: i64,
        exit_late: bool,
    },
}
fn direction_opposite(d: &Direction) -> Direction {
    match d {
        Direction::Buy => Direction::Sell,
        Direction::Sell => Direction::Buy,
    }
}
pub fn equity_milli(plan: &Plan, input: &Input<'_>) -> Option<i128> {
    let mut result =
        i128::from(plan.base_capital_milli).checked_add(i128::from(input.realized_milli))?;
    for p in input.positions {
        if p.entry_milli <= 0 || p.bid_milli <= 0 {
            return None;
        }
        let delta = i128::from(p.bid_milli).checked_sub(i128::from(p.entry_milli))?;
        result = result.checked_add(delta.checked_mul(i128::from(p.units))?)?;
    }
    Some(result)
}
fn key(leg: &Leg, date: Date, kind: Kind, sequence: usize, env: Environment) -> String {
    let day = format!(
        "{:04}{:02}{:02}",
        date.year(),
        u8::from(date.month()),
        date.day()
    );
    match kind {
        Kind::Entry => format!("{}-{day}-ENTRY-{}", leg.name, env.name()),
        Kind::Exit => format!("{}-{day}-EXIT-{}-{sequence}", leg.name, env.name()),
    }
}

fn stop_price(price: i64, leg: &Leg, increment: i64) -> Option<i64> {
    if price <= 0 || increment <= 0 {
        return None;
    }
    let factor = match leg.direction {
        Direction::Sell => 1_000_000_i128.checked_add(i128::from(leg.stop.width_ppm))?,
        Direction::Buy => 1_000_000_i128.checked_sub(i128::from(leg.stop.width_ppm))?,
    };
    if factor <= 0 {
        return None;
    }
    let numerator = i128::from(price).checked_mul(factor)?;
    let denominator = 1_000_000_i128.checked_mul(i128::from(increment))?;
    let ticks = match leg.direction {
        Direction::Sell => numerator / denominator,
        Direction::Buy => numerator.checked_add(denominator - 1)? / denominator,
    };
    let result = i64::try_from(ticks.checked_mul(i128::from(increment))?).ok()?;
    (result > 0).then_some(result)
}
fn stop_decision(
    plan: &Plan,
    leg: &Leg,
    input: &Input<'_>,
    stop: &StopSnapshot,
    now: OffsetDateTime,
) -> Action {
    let positions: Vec<_> = input
        .positions
        .iter()
        .filter(|p| p.leg == leg.name && p.units != 0)
        .collect();
    if positions.len() > 1
        || stop.entry_filled_units < 0
        || stop.exit_filled_units < 0
        || stop.filled_units < 0
        || stop.quantity < 0
        || stop.filled_units > stop.quantity
        || stop
            .filled_units
            .checked_add(stop.exit_filled_units)
            .is_none_or(|total| total > stop.entry_filled_units)
        || (stop.filled_units > 0 && stop.fill_price_milli.is_none_or(|p| p <= 0))
    {
        return Action::Human;
    }
    let remaining = stop.entry_filled_units - stop.filled_units - stop.exit_filled_units;
    if let Some(p) = positions.first() {
        let expected = match leg.direction {
            Direction::Buy => remaining,
            Direction::Sell => -remaining,
        };
        if p.event_date != stop.event_date
            || p.units != expected
            || p.entry_milli <= 0
            || p.bid_milli <= 0
        {
            return Action::Human;
        }
    } else if remaining != 0 {
        return Action::Human;
    }
    if remaining == 0 {
        return Action::Wait;
    }
    let late = now.date() > stop.event_date
        || (now.date() == stop.event_date && now.time() >= leg.exit.end);
    if stop.state == OrderState::Unknown {
        return if late { Action::Human } else { Action::Wait };
    }
    let cancel = input
        .cancellations
        .iter()
        .find(|c| c.leg == leg.name && c.order_key == stop.key);
    if let Some(c) = cancel {
        if matches!(c.state, OrderState::Unknown | OrderState::Working) {
            return if late { Action::Human } else { Action::Wait };
        }
        if c.state == OrderState::Refused {
            return Action::Human;
        }
    }
    let blackout = now.time() >= plan.blackout.start && now.time() < plan.blackout.end;
    let exit_due = now.date() > stop.event_date
        || (now.date() == stop.event_date && now.time() >= leg.exit.start);
    if now.date() < stop.event_date {
        return Action::Human;
    }
    if stop.state == OrderState::Working {
        if stop.quantity - stop.filled_units != remaining {
            return Action::Human;
        }
        if exit_due {
            if cancel.is_some() {
                return Action::Wait;
            }
            if blackout {
                return Action::Wait;
            }
            if input
                .cancellations
                .iter()
                .filter(|c| c.leg == leg.name && c.event_date == stop.event_date)
                .count()
                >= leg.order_limits.cancel as usize
            {
                return Action::Human;
            }
            return Action::CancelStop {
                key: stop.key.clone(),
            };
        }
        let Some(position) = positions.first() else {
            return Action::Human;
        };
        let Some(trigger) = stop_price(position.entry_milli, leg, plan.price_increment_milli)
        else {
            return Action::Human;
        };
        return match stop.move_state {
            MoveState::Refused => Action::Alert,
            MoveState::Working | MoveState::Unknown => Action::Wait,
            MoveState::NotAttempted if trigger != stop.trigger_milli && !blackout => {
                Action::MoveStop {
                    key: stop.key.clone(),
                    trigger_milli: trigger,
                }
            }
            _ => Action::Wait,
        };
    }
    if (!exit_due && stop.state != OrderState::Refused) || blackout {
        return Action::Wait;
    }
    if stop.state != OrderState::Refused && cancel.is_none_or(|c| c.state != OrderState::Done) {
        return Action::Human;
    }
    let relevant: Vec<_> = input
        .orders
        .iter()
        .filter(|o| o.leg == leg.name && o.event_date == stop.event_date && o.kind == Kind::Exit)
        .collect();
    if relevant
        .iter()
        .any(|o| matches!(o.state, OrderState::Working | OrderState::Unknown))
    {
        return Action::Wait;
    }
    if relevant.iter().any(|o| o.state == OrderState::Refused) {
        return Action::Human;
    }
    if relevant.len() >= leg.order_limits.exit.min(3) as usize {
        return Action::Human;
    }
    let key = key(
        leg,
        stop.event_date,
        Kind::Exit,
        relevant.len() + 2,
        input.env,
    );
    if input.orders.iter().any(|o| o.key == key) {
        return Action::Human;
    }
    Action::Submit {
        key,
        kind: Kind::Exit,
        direction: direction_opposite(&leg.direction),
        units: remaining,
        exit_late: late,
    }
}
pub fn next(plan: &Plan, input: &Input<'_>) -> Action {
    let Some(leg) = plan.legs.iter().find(|l| l.name == input.leg) else {
        return Action::Human;
    };
    if input.kill {
        return Action::Killed;
    }
    let now = input.now.to_offset(plan.utc_offset);
    if now.date() > plan.expires {
        return Action::Human;
    }
    let same_leg = input.orders.iter().filter(|o| o.leg == leg.name);
    if same_leg.clone().any(|o| {
        o.state == OrderState::Unknown
            && (now.date() > o.event_date
                || (now.date() == o.event_date && now.time() >= leg.exit.end))
    }) {
        return Action::Human;
    }
    if same_leg.clone().any(|o| o.state == OrderState::Unknown) {
        return Action::Wait;
    }
    let working: Vec<_> = same_leg
        .filter(|o| o.state == OrderState::Working)
        .collect();
    if working.len() > 1 {
        return Action::Human;
    }
    if let Some(order) = working.first() {
        if order.kind != Kind::Entry
            || order.filled_units <= 0
            || (now.date() == order.event_date && now.time() < leg.entry.end)
        {
            return Action::Wait;
        }
        if now.date() < order.event_date {
            return Action::Human;
        }
        let cancels: Vec<_> = input
            .cancellations
            .iter()
            .filter(|c| c.leg == leg.name && c.event_date == order.event_date)
            .collect();
        if let Some(cancel) = cancels.iter().find(|c| c.order_key == order.key) {
            return if cancel.state == OrderState::Refused {
                Action::Human
            } else {
                Action::Wait
            };
        }
        if cancels.len() >= leg.order_limits.cancel as usize {
            return Action::Human;
        }
        if now.time() >= plan.blackout.start && now.time() < plan.blackout.end {
            return Action::Wait;
        }
        return Action::Cancel {
            key: order.key.clone(),
        };
    }
    if leg.stop.enabled
        && let Some(stop) = input.stop
    {
        return stop_decision(plan, leg, input, stop, now);
    }
    if leg.stop.enabled
        && input
            .positions
            .iter()
            .any(|p| p.leg == leg.name && p.units != 0)
    {
        return Action::Human;
    }
    if input
        .cancellations
        .iter()
        .any(|c| c.leg == leg.name && matches!(c.state, OrderState::Working | OrderState::Unknown))
    {
        return Action::Wait;
    }
    let mut positions = input
        .positions
        .iter()
        .filter(|p| p.leg == leg.name && p.units != 0);
    let position = positions.next();
    if positions.next().is_some() {
        return Action::Human;
    }
    if let Some(p) = position {
        let valid_sign = match leg.direction {
            Direction::Buy => p.units > 0,
            Direction::Sell => p.units < 0,
        };
        if !valid_sign || p.event_date > now.date() || p.entry_milli <= 0 || p.bid_milli <= 0 {
            return Action::Human;
        }
        if now.date() == p.event_date && now.time() < leg.exit.start {
            return Action::Wait;
        }
        if now.time() >= plan.blackout.start && now.time() < plan.blackout.end {
            return Action::Wait;
        }
        let relevant = input
            .orders
            .iter()
            .filter(|o| o.leg == leg.name && o.event_date == p.event_date && o.kind == Kind::Exit);
        if relevant.clone().any(|o| o.state == OrderState::Refused) {
            return Action::Human;
        }
        let count = relevant.count();
        if count >= leg.order_limits.exit.min(3) as usize {
            return Action::Human;
        }
        let key = key(leg, p.event_date, Kind::Exit, count + 1, input.env);
        if input.orders.iter().any(|o| o.key == key) {
            return Action::Human;
        }
        let Some(units) = p.units.checked_abs() else {
            return Action::Human;
        };
        return Action::Submit {
            key,
            kind: Kind::Exit,
            direction: direction_opposite(&leg.direction),
            units,
            exit_late: now.date() > p.event_date || now.time() >= leg.exit.end,
        };
    }
    if !plan.event_dates.contains(&now.date()) || now.time() < leg.entry.start {
        return Action::Wait;
    }
    if input.entry_attempted
        || input
            .orders
            .iter()
            .any(|o| o.leg == leg.name && o.event_date == now.date() && o.kind == Kind::Entry)
    {
        return Action::Wait;
    }
    if now.time() >= leg.entry.end {
        return Action::MissedLate;
    }
    if now.time() >= plan.blackout.start && now.time() < plan.blackout.end {
        return Action::Wait;
    }
    if input.halted {
        return Action::Halted;
    }
    let Some(capital) = equity_milli(plan, input) else {
        return Action::Human;
    };
    if capital <= i128::from(plan.halt_threshold_milli) {
        return Action::Halted;
    }
    let Some(q) = input.quote else {
        return Action::Wait;
    };
    if q.bid_milli <= 0 || q.ask_milli < q.bid_milli {
        return Action::Human;
    }
    if q.ask_milli - q.bid_milli > leg.max_spread_milli {
        return Action::SkippedSpread;
    }
    let s = &plan.sizing;
    if s.lot_size <= 0 || s.multiplier <= 0 {
        return Action::Human;
    }
    let Some(amount) = capital.checked_mul(i128::from(s.multiplier)) else {
        return Action::Human;
    };
    let lots = amount / i128::from(q.ask_milli) / i128::from(s.lot_size);
    let Some(units) = lots.checked_mul(i128::from(s.lot_size)) else {
        return Action::Human;
    };
    let units = units
        .min(i128::from(s.unit_cap))
        .min(i128::from(s.absolute_cap));
    if units < i128::from(s.minimum_units) {
        return Action::SkippedSize;
    }
    let Ok(units) = i64::try_from(units) else {
        return Action::Human;
    };
    let count = input
        .orders
        .iter()
        .filter(|o| o.leg == leg.name && o.event_date == now.date() && o.kind == Kind::Entry)
        .count();
    if count >= leg.order_limits.entry as usize {
        return Action::Human;
    }
    let key = key(leg, now.date(), Kind::Entry, count + 1, input.env);
    if input.orders.iter().any(|o| o.key == key) {
        return Action::Human;
    }
    if leg.stop.enabled {
        let reference = match leg.direction {
            Direction::Sell => q.bid_milli,
            Direction::Buy => q.ask_milli,
        };
        let Some(stop_trigger_milli) = stop_price(reference, leg, plan.price_increment_milli)
        else {
            return Action::Human;
        };
        return Action::Open {
            key,
            direction: match leg.direction {
                Direction::Buy => Direction::Buy,
                Direction::Sell => Direction::Sell,
            },
            units,
            stop_trigger_milli,
        };
    }
    Action::Submit {
        key,
        kind: Kind::Entry,
        direction: match leg.direction {
            Direction::Buy => Direction::Buy,
            Direction::Sell => Direction::Sell,
        },
        units,
        exit_late: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::tests::{VALID, read};
    use time::{Duration, UtcOffset, format_description::well_known::Rfc3339};

    fn setup() -> (Plan, OffsetDateTime) {
        let text = VALID
            .replace("enabled = true", "enabled = false")
            .replace("stop = 1", "stop = 0")
            .replace("exit = 1", "exit = 3");
        (
            read(&text, 0o600).expect("plan"),
            OffsetDateTime::parse("2098-01-01T03:15:00+01:00", &Rfc3339).expect("time"),
        )
    }
    fn input(now: OffsetDateTime) -> Input<'static> {
        Input {
            now,
            env: Environment::Sim,
            leg: "A",
            quote: Some(Quote {
                bid_milli: 1000,
                ask_milli: 1005,
            }),
            positions: &[],
            realized_milli: 0,
            halted: false,
            kill: false,
            orders: &[],
            cancellations: &[],
            entry_attempted: false,
            stop: None,
        }
    }
    fn order(
        now: OffsetDateTime,
        kind: Kind,
        state: OrderState,
        key: &str,
        filled_units: i64,
    ) -> Order {
        Order {
            leg: "A".into(),
            event_date: now.date(),
            key: key.into(),
            kind,
            state,
            filled_units,
        }
    }
    fn position(now: OffsetDateTime, units: i64) -> Position {
        Position {
            leg: "A".into(),
            event_date: now.date(),
            units,
            entry_milli: 1000,
            bid_milli: 1000,
        }
    }
    #[test]
    fn test_ban_0800() {
        let (plan, now) = setup();
        let mut i = input(now);
        i.now -= Duration::minutes(100);
        assert_eq!(next(&plan, &i), Action::Wait);
        let orders = [order(now, Kind::Entry, OrderState::Working, "entry", 20)];
        i.orders = &orders;
        i.now += Duration::days(1);
        assert_eq!(next(&plan, &i), Action::Wait);
    }
    #[test]
    fn test_late_entry() {
        let (plan, now) = setup();
        let mut i = input(now);
        i.now += Duration::minutes(5);
        assert_eq!(next(&plan, &i), Action::MissedLate);
    }
    #[test]
    fn test_spread_guard() {
        let (plan, now) = setup();
        let mut i = input(now);
        i.quote = Some(Quote {
            bid_milli: 1000,
            ask_milli: 1009,
        });
        assert!(matches!(next(&plan, &i), Action::Submit { .. }));
        i.quote = Some(Quote {
            bid_milli: 1000,
            ask_milli: 1010,
        });
        assert_eq!(next(&plan, &i), Action::SkippedSpread);
        i.entry_attempted = true;
        i.quote = Some(Quote {
            bid_milli: 1000,
            ask_milli: 1001,
        });
        assert_eq!(next(&plan, &i), Action::Wait);
        i.entry_attempted = false;
        i.quote = None;
        assert_eq!(next(&plan, &i), Action::Wait);
    }
    #[test]
    fn test_size_limits() {
        let (plan, now) = setup();
        let mut i = input(now);
        assert!(matches!(next(&plan, &i), Action::Submit { units: 100, .. }));
        i.quote = Some(Quote {
            bid_milli: 49_998,
            ask_milli: 50_002,
        });
        assert!(matches!(next(&plan, &i), Action::Submit { units: 30, .. }));
        i.quote = Some(Quote {
            bid_milli: 999_999,
            ask_milli: 1_000_000,
        });
        assert_eq!(next(&plan, &i), Action::SkippedSize);
    }
    #[test]
    fn test_size_limits_low_equity() {
        let (plan, now) = setup();
        let mut i = input(now);
        i.realized_milli = -500_000;
        i.quote = Some(Quote {
            bid_milli: 49_998,
            ask_milli: 50_002,
        });
        assert!(matches!(next(&plan, &i), Action::Submit { units: 10, .. }));
        let other = [Position {
            leg: "B".into(),
            event_date: now.date(),
            units: 100,
            entry_milli: 2000,
            bid_milli: 1000,
        }];
        i.positions = &other;
        assert!(matches!(next(&plan, &i), Action::Submit { units: 10, .. }));
    }
    #[test]
    fn test_idempotency_retry_restart() {
        let (plan, now) = setup();
        let mut i = input(now);
        let orders = [order(now, Kind::Entry, OrderState::Working, "entry", 0)];
        i.orders = &orders;
        assert_eq!(next(&plan, &i), Action::Wait);
        assert_eq!(next(&plan, &i), Action::Wait);
        let done = [order(now, Kind::Entry, OrderState::Done, "entry", 100)];
        i.orders = &done;
        assert_eq!(next(&plan, &i), Action::Wait);
        let unknown = [order(now, Kind::Entry, OrderState::Unknown, "entry", 0)];
        i.orders = &unknown;
        assert_eq!(next(&plan, &i), Action::Wait);
        i.now += Duration::hours(2);
        assert_eq!(next(&plan, &i), Action::Human);
    }
    #[test]
    fn test_partial_entry() {
        let (plan, now) = setup();
        let orders = [order(now, Kind::Entry, OrderState::Working, "entry", 20)];
        let mut i = input(now);
        i.orders = &orders;
        assert_eq!(next(&plan, &i), Action::Wait);
        i.now += Duration::minutes(5);
        assert_eq!(
            next(&plan, &i),
            Action::Cancel {
                key: "entry".into()
            }
        );
        let cancels = [Cancellation {
            leg: "A".into(),
            event_date: now.date(),
            order_key: "entry".into(),
            state: OrderState::Unknown,
        }];
        i.cancellations = &cancels;
        assert_eq!(next(&plan, &i), Action::Wait);
        let finished = [order(now, Kind::Entry, OrderState::Done, "entry", 20)];
        let confirmed = [Cancellation {
            leg: "A".into(),
            event_date: now.date(),
            order_key: "entry".into(),
            state: OrderState::Done,
        }];
        let positions = [position(now, 20)];
        i.orders = &finished;
        i.cancellations = &confirmed;
        i.positions = &positions;
        i.now += Duration::minutes(50);
        assert!(matches!(
            next(&plan, &i),
            Action::Submit {
                kind: Kind::Exit,
                units: 20,
                ..
            }
        ));
    }
    #[test]
    fn test_exit_residual() {
        let (plan, now) = setup();
        let positions = [position(now, 30)];
        let orders = [order(
            now,
            Kind::Exit,
            OrderState::Done,
            "A-20980101-EXIT-SIM-1",
            20,
        )];
        let mut i = input(now);
        i.now += Duration::hours(1);
        i.positions = &positions;
        i.orders = &orders;
        assert!(
            matches!(next(&plan, &i), Action::Submit {key, kind: Kind::Exit, units: 30, ..} if key.ends_with("EXIT-SIM-2"))
        );
    }
    #[test]
    fn test_daily_order_cap() {
        let (plan, now) = setup();
        let positions = [position(now, 10)];
        let orders = [
            order(now, Kind::Exit, OrderState::Done, "one", 10),
            order(now, Kind::Exit, OrderState::Done, "two", 10),
            order(now, Kind::Exit, OrderState::Done, "three", 10),
        ];
        let mut i = input(now);
        i.now += Duration::hours(1);
        i.positions = &positions;
        i.orders = &orders;
        assert_eq!(next(&plan, &i), Action::Human);
    }
    #[test]
    fn test_kill_switch() {
        let (plan, now) = setup();
        let mut i = input(now);
        i.kill = true;
        assert_eq!(next(&plan, &i), Action::Killed);
        let positions = [position(now, 20)];
        i.positions = &positions;
        i.now += Duration::hours(1);
        assert_eq!(next(&plan, &i), Action::Killed);
    }
    #[test]
    fn test_halt_sticky() {
        let (plan, now) = setup();
        let mut i = input(now);
        i.halted = true;
        assert_eq!(next(&plan, &i), Action::Halted);
        let positions = [position(now, 20)];
        i.positions = &positions;
        i.now += Duration::hours(1);
        assert!(matches!(
            next(&plan, &i),
            Action::Submit {
                kind: Kind::Exit,
                ..
            }
        ));
    }
    #[test]
    fn test_fix_1_late_exit_next_day() {
        let (plan, now) = setup();
        let positions = [position(now, 20)];
        let mut i = input(now);
        i.positions = &positions;
        i.now += Duration::days(1);
        assert!(matches!(
            next(&plan, &i),
            Action::Submit {
                kind: Kind::Exit,
                units: 20,
                exit_late: true,
                ..
            }
        ));
    }
    #[test]
    fn test_fix_2_position_lookup() {
        let (plan, now) = setup();
        let positions = [position(now, 20)];
        let done = [order(now, Kind::Entry, OrderState::Done, "recovered", 20)];
        let mut i = input(now);
        i.positions = &positions;
        i.orders = &done;
        i.now += Duration::hours(1);
        assert!(matches!(
            next(&plan, &i),
            Action::Submit {
                kind: Kind::Exit,
                units: 20,
                ..
            }
        ));
    }
    #[test]
    fn test_fix_2_position_without_price_needs_human_once() {
        let (plan, now) = setup();
        let mut p = position(now, 20);
        p.entry_milli = 0;
        let positions = [p];
        let mut i = input(now);
        i.positions = &positions;
        assert_eq!(next(&plan, &i), Action::Human);
        assert_eq!(next(&plan, &i), Action::Human);
    }
    #[test]
    fn test_holiday_no_event() {
        let (plan, now) = setup();
        let mut i = input(now);
        i.now += Duration::days(1);
        assert_eq!(next(&plan, &i), Action::Wait);
    }
    #[test]
    fn test_jst_on_utc_host() {
        let (plan, now) = setup();
        let mut i = input(now);
        let original = next(&plan, &i);
        i.now = now.to_offset(UtcOffset::UTC);
        assert_eq!(next(&plan, &i), original);
        assert!(matches!(original, Action::Submit { .. }));
    }
}

#[cfg(test)]
mod stop_tests {
    use super::*;
    use crate::plan::tests::{VALID, read};
    use time::{Duration, format_description::well_known::Rfc3339};
    fn setup() -> (Plan, OffsetDateTime) {
        let text = VALID
            .replace("direction = \"buy\"", "direction = \"sell\"")
            .replace("exit = 1", "exit = 2");
        (
            read(&text, 0o600).expect("plan"),
            OffsetDateTime::parse("2098-01-01T03:15:00+01:00", &Rfc3339).expect("time"),
        )
    }
    fn input(now: OffsetDateTime) -> Input<'static> {
        Input {
            now,
            env: Environment::Sim,
            leg: "A",
            quote: Some(Quote {
                bid_milli: 1001,
                ask_milli: 1005,
            }),
            positions: &[],
            realized_milli: 0,
            halted: false,
            kill: false,
            orders: &[],
            cancellations: &[],
            entry_attempted: false,
            stop: None,
        }
    }
    fn stop(now: OffsetDateTime) -> StopSnapshot {
        StopSnapshot {
            key: "stop".into(),
            event_date: now.date(),
            state: OrderState::Working,
            quantity: 100,
            filled_units: 0,
            fill_price_milli: None,
            trigger_milli: 1020,
            entry_filled_units: 100,
            exit_filled_units: 0,
            move_state: MoveState::Done,
        }
    }
    fn position(now: OffsetDateTime, units: i64) -> Position {
        Position {
            leg: "A".into(),
            event_date: now.date(),
            units,
            entry_milli: 1000,
            bid_milli: 1000,
        }
    }
    fn cancel(now: OffsetDateTime, state: OrderState) -> Cancellation {
        Cancellation {
            leg: "A".into(),
            event_date: now.date(),
            order_key: "stop".into(),
            state,
        }
    }
    fn order(now: OffsetDateTime, kind: Kind, state: OrderState, filled: i64) -> Order {
        Order {
            leg: "A".into(),
            event_date: now.date(),
            key: "order".into(),
            kind,
            state,
            filled_units: filled,
        }
    }
    #[test]
    fn test_t1_entry_window_and_stop_price() {
        let (mut plan, now) = setup();
        let i = input(now);
        assert!(matches!(
            next(&plan, &i),
            Action::Open {
                units: 100,
                stop_trigger_milli: 1020,
                direction: Direction::Sell,
                ..
            }
        ));
        plan.legs[0].direction = Direction::Buy;
        assert!(matches!(
            next(&plan, &i),
            Action::Open {
                stop_trigger_milli: 985,
                direction: Direction::Buy,
                ..
            }
        ));
        let p = [position(now, 100)];
        let mut st = stop(now);
        st.trigger_milli = 980;
        st.move_state = MoveState::NotAttempted;
        let mut i = input(now);
        i.positions = &p;
        i.stop = Some(&st);
        assert_eq!(next(&plan, &i), Action::Wait);
        let mut st2 = st.clone();
        st2.trigger_milli = 975;
        i.stop = Some(&st2);
        assert!(matches!(
            next(&plan, &i),
            Action::MoveStop {
                trigger_milli: 980,
                ..
            }
        ));
    }
    #[test]
    fn test_t2_close_cancels_stop_then_buys() {
        let (plan, now) = setup();
        let p = [position(now, -100)];
        let st = stop(now);
        let mut i = input(now + Duration::hours(1));
        i.positions = &p;
        i.stop = Some(&st);
        assert!(matches!(next(&plan, &i), Action::CancelStop { .. }));
        let mut st2 = st.clone();
        st2.state = OrderState::Done;
        let c = [cancel(now, OrderState::Done)];
        i.stop = Some(&st2);
        i.cancellations = &c;
        assert!(
            matches!(next(&plan, &i), Action::Submit { key, kind: Kind::Exit, units: 100, direction: Direction::Buy, .. } if key.ends_with("EXIT-SIM-2"))
        );
    }
    #[test]
    fn test_t3_stop_filled_before_close_no_buy() {
        let (plan, now) = setup();
        let mut st = stop(now);
        st.state = OrderState::Done;
        st.filled_units = 100;
        st.fill_price_milli = Some(1020);
        let mut i = input(now + Duration::hours(1));
        i.stop = Some(&st);
        assert_eq!(next(&plan, &i), Action::Wait);
    }
    #[test]
    fn test_t4_race_stop_fills_during_cancel() {
        let (plan, now) = setup();
        let mut st = stop(now);
        st.state = OrderState::Done;
        st.filled_units = 100;
        st.fill_price_milli = Some(1020);
        let c = [cancel(now, OrderState::Done)];
        let mut i = input(now + Duration::hours(1));
        i.stop = Some(&st);
        i.cancellations = &c;
        assert_eq!(next(&plan, &i), Action::Wait);
    }
    #[test]
    fn test_t5_stop_rejected_closes_immediately() {
        let (plan, now) = setup();
        let p = [position(now, -100)];
        let mut st = stop(now);
        st.state = OrderState::Refused;
        let mut i = input(now);
        i.positions = &p;
        i.stop = Some(&st);
        assert!(matches!(
            next(&plan, &i),
            Action::Submit {
                kind: Kind::Exit,
                units: 100,
                ..
            }
        ));
        i.now -= Duration::minutes(100);
        assert_eq!(next(&plan, &i), Action::Wait);
    }
    #[test]
    fn test_t6_late_entry_missed() {
        let (plan, now) = setup();
        assert_eq!(
            next(&plan, &input(now + Duration::minutes(5))),
            Action::MissedLate
        );
    }
    #[test]
    fn test_t7_spread_and_size_skips() {
        let (plan, now) = setup();
        let mut i = input(now);
        i.quote = Some(Quote {
            bid_milli: 1000,
            ask_milli: 1010,
        });
        assert_eq!(next(&plan, &i), Action::SkippedSpread);
        i.quote = Some(Quote {
            bid_milli: 999_999,
            ask_milli: 1_000_000,
        });
        assert_eq!(next(&plan, &i), Action::SkippedSize);
    }
    #[test]
    fn test_t8_restart_no_duplicate_keys() {
        let (plan, now) = setup();
        let mut i = input(now);
        let orders = [order(now, Kind::Entry, OrderState::Done, 100)];
        i.orders = &orders;
        i.entry_attempted = true;
        assert_eq!(next(&plan, &i), Action::Wait);
        assert_eq!(next(&plan, &i), Action::Wait);
    }
    #[test]
    fn test_t9_kill_shared() {
        let (plan, now) = setup();
        let mut i = input(now);
        i.kill = true;
        assert_eq!(next(&plan, &i), Action::Killed);
    }
    #[test]
    fn test_t10_halt_blocks_entry() {
        let (plan, now) = setup();
        let mut i = input(now);
        i.halted = true;
        assert_eq!(next(&plan, &i), Action::Halted);
    }
    #[test]
    fn test_t12_other_open_sizes_with_bid() {
        let (plan, now) = setup();
        let p = [Position {
            leg: "B".into(),
            event_date: now.date(),
            units: 100,
            entry_milli: 2000,
            bid_milli: 1000,
        }];
        let mut i = input(now);
        i.realized_milli = -400_000;
        i.positions = &p;
        i.quote = Some(Quote {
            bid_milli: 49_998,
            ask_milli: 50_002,
        });
        assert!(matches!(next(&plan, &i), Action::Open { units: 10, .. }));
    }
    #[test]
    fn test_t13_entry_timeout_then_lookup_places_stop() {
        let (plan, now) = setup();
        let unknown = [order(now, Kind::Entry, OrderState::Unknown, 0)];
        let mut i = input(now);
        i.orders = &unknown;
        assert_eq!(next(&plan, &i), Action::Wait);
        let done = [order(now, Kind::Entry, OrderState::Done, 100)];
        let p = [position(now, -100)];
        let mut st = stop(now);
        st.trigger_milli = 1010;
        st.move_state = MoveState::NotAttempted;
        i.orders = &done;
        i.positions = &p;
        i.stop = Some(&st);
        assert!(matches!(
            next(&plan, &i),
            Action::MoveStop {
                trigger_milli: 1020,
                ..
            }
        ));
    }
    #[test]
    fn test_t14_stop_timeout_no_double_buy() {
        let (plan, now) = setup();
        let p = [position(now, -100)];
        let mut st = stop(now);
        st.state = OrderState::Unknown;
        let mut i = input(now + Duration::hours(1));
        i.positions = &p;
        i.stop = Some(&st);
        assert_eq!(next(&plan, &i), Action::Wait);
        i.now += Duration::minutes(10);
        assert_eq!(next(&plan, &i), Action::Human);
    }
    #[test]
    fn test_t17_shared_halt_after_loss() {
        let (plan, now) = setup();
        let mut i = input(now);
        i.halted = true;
        assert_eq!(next(&plan, &i), Action::Halted);
    }
    #[test]
    fn test_grok_b2_1_partial_stop_cancelled() {
        let (plan, now) = setup();
        let p = [position(now, -60)];
        let mut st = stop(now);
        st.filled_units = 40;
        st.fill_price_milli = Some(1020);
        let mut i = input(now + Duration::hours(1));
        i.positions = &p;
        i.stop = Some(&st);
        assert!(matches!(next(&plan, &i), Action::CancelStop { .. }));
    }
    #[test]
    fn test_grok_b2_2_missing_price_needs_human() {
        let (plan, now) = setup();
        let mut st = stop(now);
        st.filled_units = 100;
        st.state = OrderState::Done;
        let mut i = input(now);
        i.stop = Some(&st);
        assert_eq!(next(&plan, &i), Action::Human);
    }
    #[test]
    fn test_grok_b2_3_rejected_unprotected() {
        let (plan, now) = setup();
        let p = [position(now, -100)];
        let mut st = stop(now);
        st.state = OrderState::Refused;
        let mut i = input(now);
        i.positions = &p;
        i.stop = Some(&st);
        let rejected = [order(now, Kind::Exit, OrderState::Refused, 0)];
        i.orders = &rejected;
        assert_eq!(next(&plan, &i), Action::Human);
    }
    #[test]
    fn test_grok_b2_4_halt_with_open_other() {
        let (plan, now) = setup();
        let p = [position(now, -100)];
        let st = stop(now);
        let mut i = input(now + Duration::hours(1));
        i.positions = &p;
        i.stop = Some(&st);
        i.halted = true;
        assert!(matches!(next(&plan, &i), Action::CancelStop { .. }));
    }
    #[test]
    fn test_grok_b2_6_send_in_blackout_refused() {
        let (plan, now) = setup();
        let i = input(now - Duration::minutes(100));
        assert_eq!(next(&plan, &i), Action::Wait);
    }
    #[test]
    fn test_g3_partial_waits_cancel() {
        let (plan, now) = setup();
        let orders = [order(now, Kind::Entry, OrderState::Working, 20)];
        let p = [position(now, -20)];
        let mut st = stop(now);
        st.entry_filled_units = 20;
        st.quantity = 20;
        let mut i = input(now);
        i.orders = &orders;
        i.positions = &p;
        i.stop = Some(&st);
        assert_eq!(next(&plan, &i), Action::Wait);
        i.now += Duration::minutes(5);
        assert!(matches!(next(&plan, &i), Action::Cancel { .. }));
    }
    #[test]
    fn test_g3_restart_unprotected_short() {
        let (plan, now) = setup();
        let p = [position(now, -100)];
        let mut i = input(now);
        i.positions = &p;
        assert_eq!(next(&plan, &i), Action::Human);
    }
    #[test]
    fn test_grok_c_1_cancelled_partial_fill_accounted() {
        let (plan, now) = setup();
        let p = [position(now, -60)];
        let mut st = stop(now);
        st.state = OrderState::Done;
        st.filled_units = 40;
        st.fill_price_milli = Some(1020);
        let c = [cancel(now, OrderState::Done)];
        let mut i = input(now + Duration::hours(1));
        i.positions = &p;
        i.stop = Some(&st);
        i.cancellations = &c;
        assert!(matches!(next(&plan, &i), Action::Submit { units: 60, .. }));
    }
    #[test]
    fn test_grok_c_2_missing_order_is_unknown() {
        let (plan, now) = setup();
        let p = [position(now, -100)];
        let mut i = input(now);
        i.positions = &p;
        assert_eq!(next(&plan, &i), Action::Human);
    }
    #[test]
    fn test_m1_partial_exit_waits_for_cancel_confirmation() {
        let (plan, now) = setup();
        let p = [position(now, -60)];
        let mut st = stop(now);
        st.state = OrderState::Done;
        st.exit_filled_units = 40;
        let c = [cancel(now, OrderState::Done)];
        let orders = [order(now, Kind::Exit, OrderState::Working, 40)];
        let mut i = input(now + Duration::hours(1));
        i.positions = &p;
        i.stop = Some(&st);
        i.cancellations = &c;
        i.orders = &orders;
        assert_eq!(next(&plan, &i), Action::Wait);
    }
    #[test]
    fn test_m1_cancel_cap_blocks_retry() {
        let (plan, now) = setup();
        let p = [position(now, -100)];
        let st = stop(now);
        let mut c = cancel(now, OrderState::Done);
        c.order_key = "prior".into();
        let cancels = [c];
        let mut i = input(now + Duration::hours(1));
        i.positions = &p;
        i.stop = Some(&st);
        i.cancellations = &cancels;
        assert_eq!(next(&plan, &i), Action::Human);
    }
    #[test]
    fn test_grok_d_1_late_fill_blocks_next_entry() {
        let (plan, now) = setup();
        let p = [position(now, 20)];
        let mut st = stop(now);
        st.filled_units = 100;
        st.fill_price_milli = Some(1020);
        let mut i = input(now + Duration::days(1));
        i.positions = &p;
        i.stop = Some(&st);
        assert_eq!(next(&plan, &i), Action::Human);
    }
    #[test]
    fn test_grok_d_2_stop_refusal_returns_needs_human() {
        let (plan, now) = setup();
        let p = [position(now, -100)];
        let mut st = stop(now);
        st.state = OrderState::Refused;
        let mut i = input(now);
        i.positions = &p;
        i.stop = Some(&st);
        let rejected = [order(now, Kind::Exit, OrderState::Refused, 0)];
        i.orders = &rejected;
        assert_eq!(next(&plan, &i), Action::Human);
    }
    #[test]
    fn test_grok_d_3_no_stop_cancel_in_blackout() {
        let (plan, now) = setup();
        let p = [position(now, -100)];
        let st = stop(now);
        let mut i = input(now + Duration::days(1) - Duration::minutes(100));
        i.positions = &p;
        i.stop = Some(&st);
        assert_eq!(next(&plan, &i), Action::Wait);
    }
    #[test]
    fn test_grok_d_1b_partial_stop_then_close() {
        let (plan, now) = setup();
        let p = [position(now, -60)];
        let mut st = stop(now);
        st.state = OrderState::Done;
        st.filled_units = 40;
        st.fill_price_milli = Some(1020);
        let c = [cancel(now, OrderState::Done)];
        let mut i = input(now + Duration::hours(1));
        i.positions = &p;
        i.stop = Some(&st);
        i.cancellations = &c;
        assert!(matches!(next(&plan, &i), Action::Submit { units: 60, .. }));
    }
    #[test]
    fn test_grok_d_1b_overfill_or_missing_price_needs_human() {
        let (plan, now) = setup();
        let mut st = stop(now);
        st.filled_units = 101;
        st.fill_price_milli = Some(1020);
        let mut i = input(now);
        i.stop = Some(&st);
        assert_eq!(next(&plan, &i), Action::Human);
        let mut st2 = st.clone();
        st2.filled_units = 100;
        st2.fill_price_milli = None;
        i.stop = Some(&st2);
        assert_eq!(next(&plan, &i), Action::Human);
    }
    #[test]
    fn stop_move_refusal_alerts_and_size_mismatch_requires_human() {
        let (plan, now) = setup();
        let p = [position(now, -100)];
        let mut st = stop(now);
        st.move_state = MoveState::Refused;
        let mut i = input(now);
        i.positions = &p;
        i.stop = Some(&st);
        assert_eq!(next(&plan, &i), Action::Alert);
        let mut mismatch = stop(now);
        mismatch.quantity = 90;
        i.stop = Some(&mismatch);
        assert_eq!(next(&plan, &i), Action::Human);
    }
    #[test]
    fn unknown_stop_cancel_escalates_after_exit_window() {
        let (plan, now) = setup();
        let p = [position(now, -100)];
        let st = stop(now);
        let c = [cancel(now, OrderState::Unknown)];
        let mut i = input(now + Duration::hours(1));
        i.positions = &p;
        i.stop = Some(&st);
        i.cancellations = &c;
        assert_eq!(next(&plan, &i), Action::Wait);
        i.now += Duration::minutes(10);
        assert_eq!(next(&plan, &i), Action::Human);
    }
}

#[cfg(test)]
mod properties {
    use super::*;
    use crate::plan::tests::{VALID, read};
    use proptest::prelude::*;
    use time::{Duration, format_description::well_known::Rfc3339};
    fn setup() -> (Plan, OffsetDateTime) {
        let text = VALID
            .replace("direction = \"buy\"", "direction = \"sell\"")
            .replace("exit = 1", "exit = 2");
        (
            read(&text, 0o600).expect("plan"),
            OffsetDateTime::parse("2098-01-01T03:15:00+01:00", &Rfc3339).expect("time"),
        )
    }
    fn input<'a>(
        now: OffsetDateTime,
        orders: &'a [Order],
        positions: &'a [Position],
        stop: Option<&'a StopSnapshot>,
    ) -> Input<'a> {
        Input {
            now,
            env: Environment::Sim,
            leg: "A",
            quote: Some(Quote {
                bid_milli: 1000,
                ask_milli: 1005,
            }),
            positions,
            realized_milli: 0,
            halted: false,
            kill: false,
            orders,
            cancellations: &[],
            entry_attempted: false,
            stop,
        }
    }
    proptest! {
        #[test]
        fn daily_limits_hold_after_repeated_actions(fills in proptest::collection::vec(1_i64..30, 1..12)) {
            let (plan, now) = setup(); let mut orders = Vec::new(); let mut remaining = 100_i64; let mut sold = 0;
            for fill in fills {
                let p = [Position { leg: "A".into(), event_date: now.date(), units: -remaining, entry_milli: 1000, bid_milli: 1000 }];
                let st = StopSnapshot { key:"s".into(), event_date:now.date(), state:OrderState::Refused, quantity:100, filled_units:0, fill_price_milli:None, trigger_milli:1020, entry_filled_units:100, exit_filled_units:sold, move_state:MoveState::Done };
                let action = next(&plan, &input(now, &orders, &p, Some(&st)));
                if let Action::Submit { key, units, .. } = action {
                    prop_assert!(orders.len() < plan.legs[0].order_limits.exit as usize);
                    let actual = fill.min(units); remaining -= actual; sold += actual;
                    orders.push(Order {leg:"A".into(), event_date:now.date(), key, kind:Kind::Exit,state:OrderState::Done,filled_units:actual});
                } else { break; }
            }
            prop_assert!(orders.len() <= plan.legs[0].order_limits.exit as usize);
        }
        #[test]
        fn blackout_never_requests(minutes in 0_i64..60, steps in 1_usize..8) {
            let (plan, now) = setup(); let p=[Position {leg:"A".into(),event_date:now.date(),units:-100,entry_milli:1000,bid_milli:1000}];
            let st=StopSnapshot {key:"s".into(),event_date:now.date(),state:OrderState::Refused,quantity:100,filled_units:0,fill_price_milli:None,trigger_milli:1020,entry_filled_units:100,exit_filled_units:0,move_state:MoveState::Done};
            let mut orders=Vec::new();
            for _ in 0..steps { let action=next(&plan,&input(now-Duration::minutes(135)+Duration::minutes(minutes),&orders,&p,Some(&st))); prop_assert!(!matches!(action, Action::Submit{..}|Action::Open{..}|Action::Cancel{..}|Action::CancelStop{..}|Action::MoveStop{..}), "request in blackout"); if let Action::Submit{key,..}=action { orders.push(Order{leg:"A".into(),event_date:now.date(),key,kind:Kind::Exit,state:OrderState::Done,filled_units:0}); } }
        }
        #[test]
        fn entry_size_is_capped_across_restart(ask in 10_i64..200_000, capital_loss in 0_i64..900_000) {
            let (plan,now)=setup(); let mut orders=Vec::new();
            for _ in 0..4 { let mut i=input(now,&orders,&[],None); i.quote=Some(Quote{bid_milli:ask-1,ask_milli:ask}); i.realized_milli = -capital_loss;
                if let Action::Open{key,units,..}=next(&plan,&i) { prop_assert!(units<=plan.sizing.unit_cap && units<=plan.sizing.absolute_cap); orders.push(Order{leg:"A".into(),event_date:now.date(),key,kind:Kind::Entry,state:OrderState::Done,filled_units:units}); } }
            prop_assert!(orders.len()<=1);
        }
        #[test]
        fn buys_never_exceed_confirmed_short(stop_fill in 0_i64..100, fills in proptest::collection::vec(1_i64..40, 1..10)) {
            let (plan,now)=setup(); let mut orders=Vec::new(); let mut closed=0_i64;
            for fill in fills {
                let remaining=100-stop_fill-closed; if remaining==0 { break; }
                let p=[Position{leg:"A".into(),event_date:now.date(),units:-remaining,entry_milli:1000,bid_milli:1000}];
                let st=StopSnapshot{key:"s".into(),event_date:now.date(),state:OrderState::Refused,quantity:100,filled_units:stop_fill,fill_price_milli:Some(1020),trigger_milli:1020,entry_filled_units:100,exit_filled_units:closed,move_state:MoveState::Done};
                if let Action::Submit{key,units,..}=next(&plan,&input(now,&orders,&p,Some(&st))) { prop_assert!(stop_fill+closed+units<=100); let actual=fill.min(units); closed+=actual; orders.push(Order{leg:"A".into(),event_date:now.date(),key,kind:Kind::Exit,state:OrderState::Done,filled_units:actual}); } else { break; }
            }
            prop_assert!(stop_fill+closed<=100);
        }
    }
}
