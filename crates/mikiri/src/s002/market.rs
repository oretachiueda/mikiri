use super::{
    DEV_START, Result,
    input::{Inputs, LEGS, Series, days_between},
    mean, sample_variance,
    signals::{Action, Schedule, usd_base},
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

    pub fn recent_baseline(&self, start: usize) -> Result<f64> {
        if !(1..self.dates.len()).contains(&start) {
            return Err(
                "gate C requires a preceding calendar date and at least one recent return".into(),
            );
        }
        self.baseline(start - 1)
    }

    pub fn baseline(&self, start: usize) -> Result<f64> {
        let first = self
            .baseline
            .get(start)
            .copied()
            .flatten()
            .ok_or("missing starting JPY SPY value")?;
        let last = self
            .baseline
            .last()
            .copied()
            .flatten()
            .ok_or("missing ending JPY SPY value")?;
        let ratio = last / first;
        if !ratio.is_finite() || ratio <= 0.0 {
            return Err("invalid JPY SPY return".into());
        }
        Ok(ratio)
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
}
pub(super) struct Account {
    pub days: Vec<Daily>,
    pub equity: f64,
    pub fills: usize,
    pub max_gross: f64,
    pub skipped: usize,
}

#[derive(Default)]
struct TaxAccount {
    taxes: Taxes,
    income: f64,
}

impl TaxAccount {
    fn apply(&mut self, dates: &[Date], index: usize, equity: f64, pnl: f64) -> Result<f64> {
        let year = dates[index].year();
        let mut end = equity + pnl;
        self.income += pnl;
        if dates.get(index + 1).is_none_or(|day| day.year() != year) {
            end -= self
                .taxes
                .settle(i32::from(year), std::mem::take(&mut self.income));
        }
        if !end.is_finite() || end <= 0.0 {
            return Err(format!("invalid equity or bust on {}", dates[index]).into());
        }
        Ok(end)
    }
}

pub(super) fn retax(dates: &[Date], days: &[Daily]) -> Result<f64> {
    let mut account = TaxAccount::default();
    let mut equity = 1.0;
    for (index, day) in days.iter().enumerate() {
        equity = account.apply(dates, index, equity, equity * day.after)?;
    }
    Ok(equity)
}

fn pnl(
    leg: usize,
    fix: Fix,
    position: f64,
    previous_equity: f64,
    orders: &mut Vec<f64>,
) -> Result<(f64, f64, usize)> {
    let mut before = 0.0;
    let mut after = 0.0;
    let mut fills = 0;
    if position != 0.0 {
        let (carry, markup) = fix.carry.ok_or("missing rate while holding a position")?;
        before = position * (fix.spot + carry);
        after = before - position.abs() * markup;
    }
    let mut notional = position;
    for leverage in orders.drain(..) {
        let next = leverage * previous_equity;
        let delta = (next - notional).abs();
        if delta > 0.0 {
            after -= 0.5 * ROUND_TRIP[leg] / fix.price * delta;
            fills += if next * notional < 0.0 { 2 } else { 1 };
        }
        notional = next;
    }
    Ok((before, after, fills))
}

pub(super) fn simulate(
    market: &Market,
    sizing: &mut Sizing,
    schedule: &Schedule,
    shift: Option<usize>,
) -> Result<Account> {
    let t = market.dates.len() - market.start;
    let mut account = Account {
        days: Vec::with_capacity(t),
        equity: 1.0,
        fills: 0,
        max_gross: 0.0,
        skipped: 0,
    };
    let mut tax = TaxAccount::default();
    let mut held = [0.0; LEGS];
    let mut directions = [0; LEGS];
    let mut pending: [Vec<f64>; LEGS] = std::array::from_fn(|_| Vec::new());
    let offset = shift.unwrap_or(0);
    for index in market.start..market.dates.len() {
        let mut before = 0.0;
        let mut after = 0.0;
        let previous_equity = account.equity;
        for (leg, row) in market.legs[index].iter().enumerate() {
            if let Some(fix) = row.fix {
                let next = pending[leg].last().map(|l| l * previous_equity);
                let (gross, net, fills) =
                    pnl(leg, fix, held[leg], previous_equity, &mut pending[leg])?;
                before += gross;
                after += net;
                account.fills += fills;
                if let Some(next) = next {
                    held[leg] = next;
                }
            }
        }
        account.days.push(Daily {
            before: before / previous_equity,
            after: after / previous_equity,
        });
        account.equity = tax.apply(&market.dates, index, previous_equity, after)?;

        let source = market.start + (index - market.start + t - offset) % t;
        let actions = schedule.actions[source];
        if actions.iter().all(Option::is_none) {
            continue;
        }
        let rebalance = actions
            .iter()
            .any(|action| matches!(action, Some(Action::Report(_))));
        for (leg, action) in actions.into_iter().enumerate() {
            if let Some(action) = action {
                let direction = action.direction();
                let row = market.legs[index][leg];
                let disabled = shift.is_some()
                    && direction != 0
                    && !(schedule.cot_ready[index][leg] && row.rates_ready && row.sigma.is_some());
                account.skipped += usize::from(disabled);
                directions[leg] = if disabled { 0 } else { direction };
                if !rebalance {
                    pending[leg].push(0.0);
                }
            }
        }
        if !rebalance {
            continue;
        }
        let targets = sizing.targets(market, index, directions)?;
        account.max_gross = account.max_gross.max(targets.iter().map(|x| x.abs()).sum());
        for (leg, (action, target)) in actions.into_iter().zip(targets).enumerate() {
            if action.is_some() || directions[leg] != 0 {
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
