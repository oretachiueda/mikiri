use crate::{
    h001::{checked_output_path, dates, floats, output_hashes, psr, publish_outputs, write_batch},
    tiingo::{DailyBar, parse_daily_bars},
};
use arrow_array::{BooleanArray, StringArray};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, error::Error, fs, path::Path, sync::Arc};
use time::{Date, Month};

type Result<T> = std::result::Result<T, Box<dyn Error>>;
const TICKERS: [&str; 3] = ["SPY", "SPXL", "BIL"];
const COSTS: [f64; 3] = [0.0002, 0.0005, 0.0002];

#[derive(Debug)]
pub struct Day {
    pub date: Date,
    pub signal: bool,
    pub holding: &'static str,
    pub v: f64,
    pub u: f64,
    pub r: f64,
    pub q: f64,
    pub d: f64,
    pub a: f64,
}

#[derive(Debug, Serialize)]
pub struct Window {
    #[serde(rename = "W")]
    pub w: f64,
    #[serde(rename = "B")]
    pub b: f64,
    pub strategy_cagr_pretax: f64,
    pub strategy_cagr_aftertax: f64,
    pub benchmark_cagr_pretax: f64,
    pub benchmark_cagr_aftertax: f64,
    pub strategy_drawdown: f64,
    pub benchmark_drawdown: f64,
    pub switch_count: usize,
    pub spxl_share: f64,
    pub tax_by_year: BTreeMap<i32, f64>,
}

#[derive(Debug, Serialize)]
pub struct Summary {
    pub input_sha256: BTreeMap<&'static str, String>,
    pub n_cumulative: usize,
    pub main_n: usize,
    pub recent_n: usize,
    pub psr_t: usize,
    pub psr_main: f64,
    pub psr_threshold: f64,
    pub gate_a: bool,
    pub gate_b: bool,
    pub gate_c: bool,
    pub gate_a_cost2x: bool,
    pub main: Window,
    pub recent: Window,
}

#[derive(Default)]
struct Taxes {
    losses: BTreeMap<i32, f64>,
    by_year: BTreeMap<i32, f64>,
}
impl Taxes {
    fn close_year(&mut self, year: i32, income: f64) -> f64 {
        self.losses.retain(|&vintage, _| vintage + 3 >= year);
        let mut taxable = income.max(0.0);
        if income < 0.0 {
            self.losses.insert(year, -income);
        } else {
            for loss in self.losses.values_mut() {
                let used = taxable.min(*loss);
                taxable -= used;
                *loss -= used;
            }
        }
        let tax = 0.20315 * taxable;
        self.by_year.insert(year, tax);
        tax
    }
}

struct Position {
    ticker: usize,
    shares: f64,
    basis: f64,
}
impl Position {
    fn buy(ticker: usize, cash: f64, close: f64, cost: f64) -> Self {
        Self {
            ticker,
            shares: cash / (1.0 + cost) / close,
            basis: cash,
        }
    }
    fn distributions(&mut self, bar: &DailyBar) -> f64 {
        let dividend = self.shares * bar.div_cash;
        self.shares = self.shares * bar.split_factor + dividend / bar.close;
        self.basis += dividend;
        dividend
    }
    fn pay_tax(&mut self, tax: f64, close: f64, cost: f64) -> Result<(f64, f64)> {
        let before = self.shares * close;
        let gross = tax / (1.0 - cost);
        if gross >= before {
            return Err("tax sale exhausts holding".into());
        }
        let fraction = gross / before;
        let realized = tax - self.basis * fraction;
        self.shares *= 1.0 - fraction;
        self.basis *= 1.0 - fraction;
        Ok((realized, self.shares * close + tax))
    }
}

fn date(year: i32, month: Month, day: u8) -> Result<Date> {
    Ok(Date::from_calendar_date(year, month, day)?)
}

fn signals(spy: &[DailyBar]) -> Result<Vec<bool>> {
    let mut indices = Vec::with_capacity(spy.len());
    let mut result = Vec::with_capacity(spy.len());
    let mut index = 1.0;
    for (i, bar) in spy.iter().enumerate() {
        if i > 0 {
            let r = (bar.close * bar.split_factor + bar.div_cash) / spy[i - 1].close - 1.0;
            index *= 1.0 + r;
        }
        if !index.is_finite() || index <= 0.0 {
            return Err("invalid signal index".into());
        }
        indices.push(index);
        result.push(
            i >= 199
                && indices[i - 199..=i]
                    .iter()
                    .map(|value| index - value)
                    .sum::<f64>()
                    > 0.0,
        );
    }
    Ok(result)
}

fn drawdown(a: f64, peak: &mut f64, maximum: &mut f64) {
    *peak = peak.max(a);
    *maximum = maximum.max(1.0 - a / *peak);
}

fn simulate(
    bars: &[Vec<DailyBar>; 3],
    aligned: &[[usize; 3]],
    signals: &[bool],
    start: Date,
    multiplier: f64,
) -> Result<(Vec<Day>, Window)> {
    let offset = aligned
        .iter()
        .position(|row| bars[0][row[0]].date == start)
        .ok_or("missing window start")?;
    let rows = &aligned[offset..];
    let first = rows.first().ok_or("empty window")?;
    let last = rows.last().ok_or("empty window")?;
    let costs = COSTS.map(|c| c * multiplier);
    let initial = if signals[first[0] - 1] { 1 } else { 2 };
    let mut position = Position::buy(
        initial,
        1.0,
        bars[initial][first[initial]].close,
        costs[initial],
    );
    let mut benchmark = Position::buy(0, 1.0, bars[0][first[0]].close, costs[0]);
    let first_v = position.shares * bars[initial][first[initial]].close;
    let first_b = benchmark.shares * bars[0][first[0]].close;
    let (mut previous_u, mut previous_b) = (first_v, first_b);
    let (mut a, mut benchmark_a) = (first_v, first_b);
    let (mut peak, mut bpeak) = (a, benchmark_a);
    let (mut dd, mut bdd) = (0.0, 0.0);
    let (mut income, mut pending_tax) = (0.0, 0.0);
    let mut year = start.year();
    let mut taxes = Taxes::default();
    let (mut switches, mut spxl_days) = (0, 0);
    let mut days = Vec::with_capacity(rows.len());
    for (i, row) in rows.iter().enumerate() {
        let today = bars[0][row[0]].date;
        let signal = signals[row[0] - 1];
        let (v, u, bv) = if i == 0 {
            (first_v, first_v, first_b)
        } else {
            spxl_days += usize::from(position.ticker == 1);
            if today.year() != year {
                pending_tax = taxes.close_year(year, income);
                income = 0.0;
                year = today.year();
            }
            income += position.distributions(&bars[position.ticker][row[position.ticker]]);
            benchmark.distributions(&bars[0][row[0]]);
            let mut v = position.shares * bars[position.ticker][row[position.ticker]].close;
            let mut bv = benchmark.shares * bars[0][row[0]].close;
            let mut paid = 0.0;
            if i + 1 == rows.len() {
                v *= 1.0 - costs[position.ticker];
                income += v - position.basis;
                paid = taxes.close_year(year, income);
                bv *= 1.0 - costs[0];
            } else {
                let target = if signal { 1 } else { 2 };
                if target != position.ticker {
                    let proceeds = v * (1.0 - costs[position.ticker]);
                    income += proceeds - position.basis;
                    position = Position::buy(
                        target,
                        proceeds,
                        bars[target][row[target]].close,
                        costs[target],
                    );
                    v = position.shares * bars[target][row[target]].close;
                    switches += 1;
                }
                if pending_tax > 0.0 {
                    let (gain, before_payment) = position.pay_tax(
                        pending_tax,
                        bars[position.ticker][row[position.ticker]].close,
                        costs[position.ticker],
                    )?;
                    income += gain;
                    v = before_payment;
                    paid = pending_tax;
                    pending_tax = 0.0;
                }
            }
            (v, v - paid, bv)
        };
        let r = if i == 0 { 0.0 } else { v / previous_u - 1.0 };
        let q = if i == 0 { 0.0 } else { bv / previous_b - 1.0 };
        a *= 1.0 + r;
        benchmark_a *= 1.0 + q;
        if [v, u, bv, r, q, a, benchmark_a, income]
            .iter()
            .any(|x| !x.is_finite())
            || u <= 0.0
            || bv <= 0.0
        {
            return Err("invalid portfolio state".into());
        }
        drawdown(a, &mut peak, &mut dd);
        drawdown(benchmark_a, &mut bpeak, &mut bdd);
        days.push(Day {
            date: today,
            signal,
            holding: if i + 1 == rows.len() {
                "CASH"
            } else {
                TICKERS[position.ticker]
            },
            v,
            u,
            r,
            q,
            d: r - q,
            a,
        });
        previous_u = u;
        previous_b = bv;
    }
    let years = f64::from(bars[0][last[0]].date.to_julian_day() - start.to_julian_day()) / 365.25;
    let cagr = |x: f64| x.powf(1.0 / years) - 1.0;
    for year in start.year()..=bars[0][last[0]].date.year() {
        taxes.by_year.entry(year).or_insert(0.0);
    }
    Ok((
        days,
        Window {
            w: previous_u,
            b: previous_b,
            strategy_cagr_pretax: cagr(a / first_v),
            strategy_cagr_aftertax: cagr(previous_u),
            benchmark_cagr_pretax: cagr(benchmark_a / first_b),
            benchmark_cagr_aftertax: cagr(previous_b),
            strategy_drawdown: dd,
            benchmark_drawdown: bdd,
            switch_count: switches,
            spxl_share: spxl_days as f64 / (rows.len() - 1) as f64,
            tax_by_year: taxes.by_year,
        },
    ))
}

pub fn compute(bars: &[Vec<DailyBar>; 3]) -> Result<(Vec<Day>, Summary)> {
    let start = date(2015, Month::November, 2)?;
    let recent_start = date(2021, Month::November, 1)?;
    let end = date(2026, Month::October, 30)?;
    for input in bars {
        if input.is_empty()
            || input.windows(2).any(|p| p[0].date >= p[1].date)
            || input.iter().any(|b| {
                !b.close.is_finite()
                    || b.close <= 0.0
                    || !b.div_cash.is_finite()
                    || b.div_cash < 0.0
                    || !b.split_factor.is_finite()
                    || b.split_factor <= 0.0
            })
        {
            return Err("invalid daily bars".into());
        }
    }
    let spy = &bars[0];
    let first = spy
        .iter()
        .position(|b| b.date == start)
        .ok_or("missing main start")?;
    if first < 201 || spy[first - 1].date != date(2015, Month::October, 30)? {
        return Err("need 200 SPY days before first signal day".into());
    }
    if !spy.iter().any(|b| b.date == end) {
        return Err("missing main end".into());
    }
    let maps: Vec<BTreeMap<Date, usize>> = bars
        .iter()
        .map(|input| input.iter().enumerate().map(|(i, b)| (b.date, i)).collect())
        .collect();
    let mut aligned = Vec::new();
    for (i, b) in spy
        .iter()
        .enumerate()
        .filter(|(_, b)| b.date >= start && b.date <= end)
    {
        aligned.push([
            i,
            *maps[1].get(&b.date).ok_or("missing main SPXL date")?,
            *maps[2].get(&b.date).ok_or("missing main BIL date")?,
        ]);
    }
    let history_end = spy.partition_point(|b| b.date <= end);
    let signals = signals(&spy[..history_end])?;
    let (days, main) = simulate(bars, &aligned, &signals, start, 1.0)?;
    let (recent_days, recent) = simulate(bars, &aligned, &signals, recent_start, 1.0)?;
    let (_, cost2x) = simulate(bars, &aligned, &signals, start, 2.0)?;
    let ds = days[1..].iter().map(|d| d.d).collect::<Vec<_>>();
    if ds.iter().all(|&d| d == ds[0]) {
        return Err("PSR variance must be positive and finite".into());
    }
    let psr_main = psr(&ds)?;
    let threshold = 1.0 - 0.05 / 3.0;
    let summary = Summary {
        input_sha256: BTreeMap::new(),
        n_cumulative: 3,
        main_n: days.len(),
        recent_n: recent_days.len(),
        psr_t: ds.len(),
        psr_main,
        psr_threshold: threshold,
        gate_a: main.w > main.b,
        gate_b: psr_main >= threshold,
        gate_c: recent.w > recent.b,
        gate_a_cost2x: cost2x.w > cost2x.b,
        main,
        recent,
    };
    Ok((days, summary))
}

pub fn run(inputs: [&Path; 3], out: &Path) -> Result<String> {
    checked_output_path(out)?;
    let raw = inputs.map(fs::read);
    let raw = raw
        .into_iter()
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let bars: [Vec<DailyBar>; 3] = raw
        .iter()
        .map(|r| parse_daily_bars(r))
        .collect::<std::result::Result<Vec<_>, _>>()?
        .try_into()
        .map_err(|_| "expected three inputs")?;
    let (days, mut summary) = compute(&bars)?;
    summary.input_sha256 = TICKERS
        .into_iter()
        .zip(raw.iter().map(|r| format!("{:x}", Sha256::digest(r))))
        .collect();
    publish_outputs(out, |tmp| {
        let flat: Vec<_> = bars
            .iter()
            .enumerate()
            .flat_map(|(i, rows)| rows.iter().map(move |b| (TICKERS[i], b)))
            .collect();
        write_batch(
            &tmp.join("bars.parquet"),
            vec![
                (
                    "ticker",
                    Arc::new(StringArray::from_iter_values(flat.iter().map(|(t, _)| *t))),
                ),
                ("date", dates(flat.iter().map(|(_, b)| b.date))?),
                ("close", floats(flat.iter().map(|(_, b)| b.close))),
                ("div_cash", floats(flat.iter().map(|(_, b)| b.div_cash))),
                (
                    "split_factor",
                    floats(flat.iter().map(|(_, b)| b.split_factor)),
                ),
            ],
        )?;
        write_batch(
            &tmp.join("days.parquet"),
            vec![
                ("date", dates(days.iter().map(|d| d.date))?),
                (
                    "signal",
                    Arc::new(BooleanArray::from(
                        days.iter().map(|d| d.signal).collect::<Vec<_>>(),
                    )),
                ),
                (
                    "holding",
                    Arc::new(StringArray::from_iter_values(
                        days.iter().map(|d| d.holding),
                    )),
                ),
                ("V", floats(days.iter().map(|d| d.v))),
                ("U", floats(days.iter().map(|d| d.u))),
                ("R", floats(days.iter().map(|d| d.r))),
                ("Q", floats(days.iter().map(|d| d.q))),
                ("d", floats(days.iter().map(|d| d.d))),
                ("A", floats(days.iter().map(|d| d.a))),
            ],
        )?;
        fs::write(
            tmp.join("summary.json"),
            serde_json::to_vec_pretty(&summary)?,
        )?;
        Ok(())
    })?;
    Ok(format!(
        "{} main_n={} recent_n={}",
        output_hashes(out, &["bars.parquet", "days.parquet", "summary.json"])?,
        summary.main_n,
        summary.recent_n
    ))
}

#[cfg(test)]
mod tests;
