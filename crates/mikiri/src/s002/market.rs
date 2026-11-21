use super::{
    DEV_START, Result,
    input::{Inputs, LEGS, Series, days_between},
    mean, sample_variance,
    signals::{Schedule, usd_base},
    trace::TraceRow,
};
use crate::h005::core::Taxes;
use jiff::{Span, civil::Date};
use std::collections::{BTreeMap, BTreeSet};

const VOL_WINDOW: usize = 60;
const TARGET_VOL: f64 = 0.30;
const MAX_GROSS: f64 = 10.0;
pub(super) const DAYS_PER_YEAR: f64 = 252.0;
pub(super) const ROUND_TRIP: [f64; LEGS] = [
    0.002, 0.00004, 0.0003, 0.0003, 0.0003, 0.00006, 0.0003, 0.003,
];

fn markup(leg: usize) -> f64 {
    if leg == 7 { 0.020 } else { 0.0135 }
}

#[derive(Clone, Copy)]
struct Fix {
    price: f64,
    spot: f64,
    log_return: Option<f64>,
    carry: Option<(f64, f64)>,
}

#[derive(Clone, Copy, Default)]
struct LegDay {
    fix: Option<Fix>,
    sigma: Option<f64>,
    rates_ready: bool,
}

pub(super) struct Market {
    pub dates: Vec<Date>,
    pub start: usize,
    legs: Vec<[LegDay; LEGS]>,
    baseline: Vec<Option<f64>>,
}

fn known_rate(series: &Series, day: Date) -> Result<Option<f64>> {
    let observation = Date::new(day.year(), day.month(), 1)?.checked_sub(Span::new().months(2))?;
    Ok(series
        .range(..=observation)
        .next_back()
        .map(|(_, &rate)| rate))
}

fn carry(
    leg: usize,
    rates: &[Series; LEGS + 1],
    previous_fix: Date,
    fix: Date,
) -> Result<Option<(f64, f64)>> {
    let days = days_between(previous_fix, fix);
    let mut quote_long = 0.0;
    for offset in 1..=days {
        let day = previous_fix.checked_add(Span::new().days(offset))?;
        let (Some(foreign), Some(usd)) = (
            known_rate(&rates[leg], day)?,
            known_rate(&rates[LEGS], day)?,
        ) else {
            return Ok(None);
        };
        quote_long += if usd_base(leg) {
            usd - foreign
        } else {
            foreign - usd
        };
    }
    Ok(Some((
        quote_long / 365.0,
        markup(leg) * days as f64 / 365.0,
    )))
}

impl Market {
    pub fn new(input: &Inputs) -> Result<Self> {
        let dates: Vec<_> = input
            .fixes
            .iter()
            .flat_map(|f| f.keys().copied())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let start = dates.partition_point(|&day| day < DEV_START);
        if dates.len() - start < 2 {
            return Err("insufficient dev FX fixes".into());
        }
        let mut legs = vec![[LegDay::default(); LEGS]; dates.len()];
        let mut baseline = Vec::with_capacity(dates.len());
        for &day in &dates {
            let spy = input.spy.range(..=day).next_back();
            let jpy = input.fixes[0].range(..=day).next_back();
            let value = spy.zip(jpy).map(|((_, spy), (_, jpy))| spy * jpy);
            if value.is_some_and(|value| !value.is_finite() || value <= 0.0) {
                return Err("invalid JPY SPY baseline".into());
            }
            baseline.push(value);
        }
        for (leg, prices) in input.fixes.iter().enumerate() {
            let mut previous = None;
            let mut history = Vec::new();
            for (index, &day) in dates.iter().enumerate() {
                let row = &mut legs[index][leg];
                if history.len() >= VOL_WINDOW {
                    row.sigma = Some(
                        (DAYS_PER_YEAR * sample_variance(&history[history.len() - VOL_WINDOW..]))
                            .sqrt(),
                    );
                }
                row.rates_ready = known_rate(&input.rates[leg], day)?.is_some()
                    && known_rate(&input.rates[LEGS], day)?.is_some();
                let Some(&price) = prices.get(&day) else {
                    continue;
                };
                let mut fix = Fix {
                    price,
                    spot: 0.0,
                    log_return: None,
                    carry: None,
                };
                if let Some((prior_day, prior_price)) = previous {
                    let ratio: f64 = price / prior_price;
                    let log_return = ratio.ln();
                    if !log_return.is_finite() {
                        return Err("invalid FX fix return".into());
                    }
                    history.push(log_return);
                    fix.spot = ratio - 1.0;
                    fix.log_return = Some(log_return);
                    fix.carry = carry(leg, &input.rates, prior_day, day)?;
                }
                row.fix = Some(fix);
                previous = Some((day, price));
            }
        }
        Ok(Self {
            dates,
            start,
            legs,
            baseline,
        })
    }

    fn benchmark_base(&self, start: usize) -> Result<f64> {
        if !(1..self.dates.len()).contains(&start) {
            return Err(
                "benchmark requires a preceding calendar date and at least one return".into(),
            );
        }
        self.baseline[start - 1].ok_or_else(|| "missing starting JPY SPY value".into())
    }

    fn benchmark_ratio(value: Option<f64>, base: f64) -> Result<f64> {
        let ratio = value.ok_or("missing JPY SPY value")? / base;
        if !ratio.is_finite() || ratio <= 0.0 {
            return Err("invalid JPY SPY return".into());
        }
        Ok(ratio)
    }

    pub fn benchmark_wealth(&self, start: usize) -> Result<f64> {
        let base = self.benchmark_base(start)?;
        Self::benchmark_ratio(self.baseline.last().copied().flatten(), base)
    }

    pub fn benchmark_path(&self, start: usize) -> Result<Vec<f64>> {
        let base = self.benchmark_base(start)?;
        self.baseline[start..]
            .iter()
            .map(|&value| Self::benchmark_ratio(value, base))
            .collect()
    }
}

type JointWindows = BTreeMap<(usize, u8), Option<[[f64; LEGS]; LEGS]>>;

fn joint_covariance(
    market: &Market,
    index: usize,
    included: &[usize],
) -> Option<[[f64; LEGS]; LEGS]> {
    let mut window = [[0.0; VOL_WINDOW]; LEGS];
    let mut n = 0;
    for day in market.legs[..index].iter().rev() {
        if included
            .iter()
            .all(|&leg| day[leg].fix.and_then(|fix| fix.log_return).is_some())
        {
            for &leg in included {
                window[leg][n] = day[leg].fix?.log_return?;
            }
            n += 1;
            if n == VOL_WINDOW {
                break;
            }
        }
    }
    if n < VOL_WINDOW {
        return None;
    }
    let mut centered = window;
    for &i in included {
        let mean = mean(&window[i]);
        for value in &mut centered[i] {
            *value -= mean;
        }
    }
    let mut covariance = [[0.0; LEGS]; LEGS];
    for (position, &i) in included.iter().enumerate() {
        for &j in &included[position..] {
            let sum = centered[i]
                .iter()
                .zip(centered[j])
                .map(|(a, b)| a * b)
                .sum::<f64>();
            let value = sum / (VOL_WINDOW - 1) as f64;
            covariance[i][j] = value;
            covariance[j][i] = value;
        }
    }
    Some(covariance)
}

#[derive(Default)]
pub(super) struct Sizing {
    joint: JointWindows,
}

impl Sizing {
    fn targets(
        &mut self,
        market: &Market,
        index: usize,
        direction: [i8; LEGS],
    ) -> Result<[f64; LEGS]> {
        let mut u = [0.0; LEGS];
        let mut included = Vec::new();
        for (leg, direction) in direction.into_iter().enumerate() {
            let row = market.legs[index][leg];
            if direction != 0
                && row.rates_ready
                && let Some(sigma) = row.sigma
            {
                if !sigma.is_finite() || sigma <= 0.0 {
                    return Err(format!(
                        "invalid leg volatility for leg {leg} on {}",
                        market.dates[index]
                    )
                    .into());
                }
                u[leg] = f64::from(direction) / sigma;
                included.push(leg);
            }
        }
        match included.as_slice() {
            [] => return Ok(u),
            &[leg] => {
                u[leg] = u[leg].signum() * (TARGET_VOL * u[leg].abs()).min(MAX_GROSS);
                return Ok(u);
            }
            _ => {}
        }
        let mask = included.iter().fold(0_u8, |mask, &leg| mask | (1 << leg));
        let Some(covariance) = self
            .joint
            .entry((index, mask))
            .or_insert_with(|| joint_covariance(market, index, &included))
        else {
            return Ok([0.0; LEGS]);
        };
        let variance: f64 = included
            .iter()
            .map(|&i| {
                included
                    .iter()
                    .map(|&j| u[i] * u[j] * covariance[i][j])
                    .sum::<f64>()
            })
            .sum();
        let sigma = (DAYS_PER_YEAR * variance).sqrt();
        if !sigma.is_finite() || sigma <= 0.0 {
            return Err(format!("invalid joint volatility on {}", market.dates[index]).into());
        }
        let gross: f64 = u.iter().map(|u| u.abs()).sum();
        let scale = (TARGET_VOL / sigma).min(MAX_GROSS / gross);
        Ok(u.map(|value| value * scale))
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Daily {
    pub before: f64,
    pub after: f64,
    pub equity_after_tax: f64,
    pub tax_paid: f64,
}
pub(super) struct Account {
    pub days: Vec<Daily>,
    pub equity: f64,
    entries: usize,
    pub max_gross: f64,
    pub skipped: usize,
}

impl Account {
    pub fn entries_per_year(&self) -> f64 {
        self.entries as f64 / (self.days.len() as f64 / DAYS_PER_YEAR)
    }
}

#[derive(Default)]
struct TaxAccount {
    taxes: Taxes,
    income: f64,
}

impl TaxAccount {
    fn apply(&mut self, dates: &[Date], index: usize, equity: f64, pnl: f64) -> Result<(f64, f64)> {
        let year = dates[index].year();
        let end = equity + pnl;
        self.income += pnl;
        let tax_paid = if dates.get(index + 1).is_none_or(|day| day.year() != year) {
            self.taxes
                .settle(i32::from(year), std::mem::take(&mut self.income))
        } else {
            0.0
        };
        let equity_after_tax = end - tax_paid;
        if !equity_after_tax.is_finite() || equity_after_tax <= 0.0 {
            return Err(format!("invalid equity or bust on {}", dates[index]).into());
        }
        Ok((equity_after_tax, tax_paid))
    }
}

pub(super) fn retax(dates: &[Date], days: &[Daily]) -> Result<f64> {
    let mut account = TaxAccount::default();
    let mut equity = 1.0;
    for (index, day) in days.iter().enumerate() {
        (equity, _) = account.apply(dates, index, equity, equity * day.after)?;
    }
    Ok(equity)
}

#[derive(Default)]
struct LegPnl {
    before: f64,
    after: f64,
    entries: usize,
    carry_markup_amount: f64,
    spread_cost_amount: f64,
    filled: bool,
}

fn pnl(
    leg: usize,
    fix: Fix,
    position: f64,
    previous_equity: f64,
    orders: &mut Vec<f64>,
) -> Result<LegPnl> {
    let mut pnl = LegPnl::default();
    if position != 0.0 {
        let (carry, markup) = fix.carry.ok_or("missing rate while holding a position")?;
        let markup_amount = position.abs() * markup;
        pnl.before = position * (fix.spot + carry);
        pnl.after = pnl.before - markup_amount;
        pnl.carry_markup_amount = position * carry - markup_amount;
    }
    let mut notional = position;
    for leverage in orders.drain(..) {
        let next = leverage * previous_equity;
        let delta = (next - notional).abs();
        if delta > 0.0 {
            let spread = 0.5 * ROUND_TRIP[leg] / fix.price * delta;
            pnl.after -= spread;
            pnl.spread_cost_amount += spread;
            pnl.filled = true;
            if next != 0.0
                && (notional == 0.0 || next.is_sign_positive() != notional.is_sign_positive())
            {
                pnl.entries += 1;
            }
        }
        notional = next;
    }
    Ok(pnl)
}

pub(super) fn simulate(
    market: &Market,
    sizing: &mut Sizing,
    schedule: &Schedule,
    shift: Option<usize>,
) -> Result<Account> {
    simulate_with_trace(market, sizing, schedule, shift, None)
}

pub(super) fn simulate_with_trace(
    market: &Market,
    sizing: &mut Sizing,
    schedule: &Schedule,
    shift: Option<usize>,
    mut trace: Option<&mut Vec<TraceRow>>,
) -> Result<Account> {
    let t = market.dates.len() - market.start;
    let mut account = Account {
        days: Vec::with_capacity(t),
        equity: 1.0,
        entries: 0,
        max_gross: 0.0,
        skipped: 0,
    };
    let mut tax = TaxAccount::default();
    let mut held = [0.0; LEGS];
    let mut held_leverage = [0.0; LEGS];
    let mut pending: [Vec<f64>; LEGS] = std::array::from_fn(|_| Vec::new());
    let offset = shift.unwrap_or(0);
    for index in market.start..market.dates.len() {
        let mut before = 0.0;
        let mut after = 0.0;
        let previous_equity = account.equity;
        let mut filled_target = false;
        let source = market.start + (index - market.start + t - offset) % t;
        let actions = schedule.actions[source];
        let trace_start = trace.as_ref().map_or(0, |rows| rows.len());
        for (leg, row) in market.legs[index].iter().enumerate() {
            let previous_notional = held[leg];
            let mut leg_pnl = LegPnl::default();
            if let Some(fix) = row.fix {
                let filled_leverage = pending[leg].last().copied();
                leg_pnl = pnl(
                    leg,
                    fix,
                    previous_notional,
                    previous_equity,
                    &mut pending[leg],
                )?;
                before += leg_pnl.before;
                after += leg_pnl.after;
                account.entries += leg_pnl.entries;
                if let Some(leverage) = filled_leverage {
                    filled_target = true;
                    held[leg] = leverage * previous_equity;
                    held_leverage[leg] = leverage.abs();
                }
            }
            if let Some(trace) = trace.as_deref_mut() {
                let signal_day = actions[leg].is_some();
                let report_as_of = schedule.report_as_of[source][leg];
                if previous_notional != 0.0
                    || held[leg] != 0.0
                    || leg_pnl.filled
                    || signal_day
                    || report_as_of.is_some()
                {
                    trace.push(TraceRow {
                        date: market.dates[index],
                        leg,
                        signal_day,
                        report_as_of,
                        fill: leg_pnl.filled,
                        leverage: held_leverage[leg],
                        notional: held[leg],
                        prev_equity: previous_equity,
                        spot_ret: row.fix.map_or(0.0, |fix| fix.spot),
                        carry_markup_amount: leg_pnl.carry_markup_amount,
                        spread_cost_amount: leg_pnl.spread_cost_amount,
                        leg_pnl: leg_pnl.after,
                        daily_return: 0.0,
                    });
                }
            }
        }
        if filled_target {
            account.max_gross = account.max_gross.max(held_leverage.iter().sum());
        }
        let daily_return = after / previous_equity;
        if let Some(trace) = trace.as_deref_mut() {
            for row in &mut trace[trace_start..] {
                row.daily_return = daily_return;
            }
        }
        let (equity_after_tax, tax_paid) =
            tax.apply(&market.dates, index, previous_equity, after)?;
        account.days.push(Daily {
            before: before / previous_equity,
            after: daily_return,
            equity_after_tax,
            tax_paid,
        });
        account.equity = equity_after_tax;

        if actions.iter().all(Option::is_none) {
            continue;
        }
        let mut directions = [0; LEGS];
        for (leg, action) in actions.into_iter().enumerate() {
            if let Some(action) = action {
                let direction = action.direction();
                let row = market.legs[index][leg];
                let disabled = shift.is_some()
                    && direction != 0
                    && !(schedule.cot_ready[index][leg] && row.rates_ready && row.sigma.is_some());
                account.skipped += usize::from(disabled);
                directions[leg] = if disabled { 0 } else { direction };
            }
        }
        let targets = sizing.targets(market, index, directions)?;
        for (leg, (action, target)) in actions.into_iter().zip(targets).enumerate() {
            if action.is_some() {
                pending[leg].push(target);
            }
        }
    }
    account.skipped += pending
        .iter()
        .flatten()
        .filter(|&&target| target != 0.0)
        .count();
    Ok(account)
}

#[cfg(test)]
mod tests;
