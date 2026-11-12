use crate::{
    h001::{checked_output_path, dates, floats, output_hashes, psr, publish_outputs, write_batch},
    tiingo::{DailyBar, parse_daily_bars},
};
use arrow_array::{ArrayRef, BooleanArray, Float64Array, RecordBatch, StringArray};
use arrow_schema::{Field, Schema};
use parquet::arrow::ArrowWriter;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, error::Error, fs, path::Path, sync::Arc};
use time::{Date, Month};

type Result<T> = std::result::Result<T, Box<dyn Error>>;
const TICKERS: [&str; 2] = ["SPY", "SSO"];
const COSTS: [f64; 2] = [0.0002, 0.0005];

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
    pub sso_share: f64,
    pub entry_count: usize,
    pub mean_holding_days: Option<f64>,
    pub sso_spy_mean: Option<f64>,
    pub sso_spy_std: Option<f64>,
    pub tax_by_year: BTreeMap<i32, f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Dev,
    Holdout,
}
impl Stage {
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "dev" => Ok(Self::Dev),
            "holdout" => Ok(Self::Holdout),
            _ => Err("stage must be dev or holdout".into()),
        }
    }
    fn end(self) -> Result<Date> {
        match self {
            Self::Dev => date(2024, Month::October, 31),
            Self::Holdout => date(2026, Month::October, 30),
        }
    }
}

#[derive(Debug)]
pub struct Indicator {
    pub date: Date,
    pub index: f64,
    pub m200: Option<f64>,
    pub m5: Option<f64>,
    pub rsi: Option<f64>,
    pub signal: Option<bool>,
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

fn next_signal(holding: bool, above200: bool, above5: bool, rsi: f64) -> bool {
    if holding {
        !above5
    } else {
        above200 && rsi <= 5.0
    }
}

fn above_average(index: f64, values: &[f64]) -> bool {
    values.iter().map(|value| index - value).sum::<f64>() > 0.0
}

pub fn indicators(spy: &[DailyBar]) -> Result<Vec<Indicator>> {
    let mut indices = Vec::with_capacity(spy.len());
    let mut result = Vec::with_capacity(spy.len());
    let (mut index, mut au, mut ad) = (1.0, 0.0, 0.0);
    let mut holding = false;
    for (i, bar) in spy.iter().enumerate() {
        if i > 0 {
            let previous = index;
            let r = (bar.close * bar.split_factor + bar.div_cash) / spy[i - 1].close - 1.0;
            index *= 1.0 + r;
            let delta = index - previous;
            if i <= 2 {
                au += delta.max(0.0) / 2.0;
                ad += (-delta).max(0.0) / 2.0;
            } else {
                au = (au + delta.max(0.0)) / 2.0;
                ad = (ad + (-delta).max(0.0)) / 2.0;
            }
        }
        if !index.is_finite() || index <= 0.0 {
            return Err("invalid signal index".into());
        }
        indices.push(index);
        let m200 = (i >= 199).then(|| indices[i - 199..=i].iter().sum::<f64>() / 200.0);
        let m5 = (i >= 4).then(|| indices[i - 4..=i].iter().sum::<f64>() / 5.0);
        let rsi = (i >= 2).then(|| {
            if au + ad == 0.0 {
                50.0
            } else {
                100.0 * au / (au + ad)
            }
        });
        if [Some(au), Some(ad), m200, m5, rsi]
            .into_iter()
            .flatten()
            .any(|value| !value.is_finite())
        {
            return Err("non-finite signal indicator".into());
        }
        if i > 199 {
            holding = next_signal(
                holding,
                above_average(index, &indices[i - 199..=i]),
                above_average(index, &indices[i - 4..=i]),
                rsi.ok_or("missing RSI")?,
            );
        }
        result.push(Indicator {
            date: bar.date,
            index,
            m200,
            m5,
            rsi,
            signal: (i >= 199).then_some(holding),
        });
    }
    Ok(result)
}

fn drawdown(a: f64, peak: &mut f64, maximum: &mut f64) {
    *peak = peak.max(a);
    *maximum = maximum.max(1.0 - a / *peak);
}

fn simulate(
    bars: &[Vec<DailyBar>; 2],
    aligned: &[[usize; 2]],
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
    let initial = if signals[first[0] - 1] { 1 } else { 0 };
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
    let (mut switches, mut sso_days) = (0, 0);
    let mut entries = usize::from(initial == 1);
    let mut held_returns = Vec::new();
    let mut days = Vec::with_capacity(rows.len());
    for (i, row) in rows.iter().enumerate() {
        let today = bars[0][row[0]].date;
        let signal = signals[row[0] - 1];
        let (v, u, bv) = if i == 0 {
            (first_v, first_v, first_b)
        } else {
            sso_days += usize::from(position.ticker == 1);
            if position.ticker == 1 {
                let current = &bars[0][row[0]];
                held_returns.push(
                    (current.close * current.split_factor + current.div_cash)
                        / bars[0][row[0] - 1].close
                        - 1.0,
                );
            }
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
                let target = if signal { 1 } else { 0 };
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
                    entries += usize::from(target == 1);
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
    let held_mean = if held_returns.is_empty() {
        0.0
    } else {
        held_returns.iter().sum::<f64>() / held_returns.len() as f64
    };
    let held_std = if held_returns.is_empty() {
        0.0
    } else {
        (held_returns
            .iter()
            .map(|x| (x - held_mean).powi(2))
            .sum::<f64>()
            / held_returns.len() as f64)
            .sqrt()
    };
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
            sso_share: sso_days as f64 / (rows.len() - 1) as f64,
            entry_count: entries,
            mean_holding_days: (entries > 0).then_some(sso_days as f64 / entries as f64),
            sso_spy_mean: (!held_returns.is_empty()).then_some(held_mean),
            sso_spy_std: (!held_returns.is_empty()).then_some(held_std),
            tax_by_year: taxes.by_year,
        },
    ))
}

pub fn compute(bars: &[Vec<DailyBar>; 2], stage: Stage) -> Result<(Vec<Day>, serde_json::Value)> {
    let start = if stage == Stage::Dev {
        date(2008, Month::January, 2)?
    } else {
        date(2024, Month::November, 1)?
    };
    let end = stage.end()?;
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
    let signal_day = date(2007, Month::December, 31)?;
    let first_signal = spy
        .iter()
        .position(|b| b.date == signal_day)
        .ok_or("missing first signal day")?;
    if first_signal < 200 {
        return Err("need 200 SPY days before first signal day".into());
    }
    let first = spy
        .iter()
        .position(|b| b.date == start)
        .ok_or("missing window start")?;
    if first == 0 || (stage == Stage::Dev && spy[first - 1].date != signal_day) {
        return Err("missing preceding signal session".into());
    }
    if !spy.iter().any(|b| b.date == end) {
        return Err("missing window end".into());
    }
    let map: BTreeMap<Date, usize> = bars[1]
        .iter()
        .enumerate()
        .map(|(i, b)| (b.date, i))
        .collect();
    let mut aligned = Vec::new();
    for (i, b) in spy
        .iter()
        .enumerate()
        .filter(|(_, b)| b.date >= start && b.date <= end)
    {
        aligned.push([i, *map.get(&b.date).ok_or("missing SSO window date")?]);
    }
    let history_end = spy.partition_point(|b| b.date <= end);
    let signals = indicators(&spy[..history_end])?
        .iter()
        .map(|x| x.signal.unwrap_or(false))
        .collect::<Vec<_>>();
    let (days, main) = simulate(bars, &aligned, &signals, start, 1.0)?;
    if stage == Stage::Holdout {
        return Ok((
            days,
            serde_json::json!({"stage":"holdout", "n_cumulative":4, "holdout_n":aligned.len(), "gate_h":main.w > main.b, "holdout":main}),
        ));
    }
    let (recent_days, recent) = simulate(
        bars,
        &aligned,
        &signals,
        date(2019, Month::November, 1)?,
        1.0,
    )?;
    let (_, cost2x) = simulate(bars, &aligned, &signals, start, 2.0)?;
    let ds = days[1..].iter().map(|d| d.d).collect::<Vec<_>>();
    if ds.iter().all(|&d| d == ds[0]) {
        return Err("PSR variance must be positive and finite".into());
    }
    let probability = psr(&ds)?;
    let threshold = 1.0 - 0.05 / 4.0;
    let summary = serde_json::json!({"stage":"dev", "n_cumulative":4, "main_n":days.len(), "recent_n":recent_days.len(), "psr_t":ds.len(), "psr_main":probability, "psr_threshold":threshold, "gate_a":main.w > main.b, "gate_b":probability >= threshold, "gate_c":recent.w > recent.b, "gate_a_cost2x":cost2x.w > cost2x.b, "main":main, "recent":recent});
    Ok((days, summary))
}

pub fn require_dev_verdict(path: Option<&Path>) -> Result<()> {
    let verdict: serde_json::Value =
        serde_json::from_slice(&fs::read(path.ok_or("holdout requires --dev-verdict")?)?)?;
    let summary = if verdict.get("python_summary").is_some() {
        if verdict.get("match").and_then(|v| v.as_bool()) != Some(true) {
            return Err("dev verdict comparison must match".into());
        }
        &verdict["python_summary"]
    } else {
        &verdict
    };
    if summary["stage"] != "dev"
        || ["gate_a", "gate_b", "gate_c"]
            .iter()
            .any(|k| summary[*k].as_bool() != Some(true))
    {
        return Err("holdout requires passing dev A, B, C verdict".into());
    }
    Ok(())
}

fn load(raw: &[u8], stage: Stage) -> Result<(Vec<DailyBar>, Vec<u8>)> {
    let value: serde_json::Value = serde_json::from_slice(raw)?;
    let rows = value.as_array().ok_or("expected Tiingo array")?;
    let cutoff = stage.end()?.to_string();
    let mut kept = Vec::new();
    for row in rows {
        let text = row["date"].as_str().ok_or("missing bar date")?;
        let day = text.get(..10).ok_or("invalid bar date")?;
        if day <= cutoff.as_str() {
            kept.push(row);
        }
    }
    let filtered = serde_json::to_vec(&kept)?;
    Ok((parse_daily_bars(&filtered)?, filtered))
}

fn indicator_batch(history: &[Indicator]) -> Result<RecordBatch> {
    let columns: Vec<(&str, ArrayRef)> = vec![
        ("date", dates(history.iter().map(|d| d.date))?),
        ("index", floats(history.iter().map(|d| d.index))),
        (
            "m200",
            Arc::new(Float64Array::from(
                history.iter().map(|d| d.m200).collect::<Vec<_>>(),
            )),
        ),
        (
            "m5",
            Arc::new(Float64Array::from(
                history.iter().map(|d| d.m5).collect::<Vec<_>>(),
            )),
        ),
        (
            "rsi",
            Arc::new(Float64Array::from(
                history.iter().map(|d| d.rsi).collect::<Vec<_>>(),
            )),
        ),
        (
            "signal",
            Arc::new(BooleanArray::from(
                history.iter().map(|d| d.signal).collect::<Vec<_>>(),
            )),
        ),
    ];
    let fields = columns
        .iter()
        .map(|(name, array)| {
            Field::new(
                *name,
                array.data_type().clone(),
                !matches!(*name, "date" | "index"),
            )
        })
        .collect::<Vec<_>>();
    Ok(RecordBatch::try_new(
        Arc::new(Schema::new(fields)),
        columns.into_iter().map(|(_, array)| array).collect(),
    )?)
}

pub fn run(
    inputs: [&Path; 2],
    out: &Path,
    stage: Stage,
    dev_verdict: Option<&Path>,
) -> Result<String> {
    checked_output_path(out)?;
    if stage == Stage::Holdout {
        require_dev_verdict(dev_verdict)?;
    }
    let loaded = inputs
        .into_iter()
        .map(|p| load(&fs::read(p)?, stage))
        .collect::<Result<Vec<_>>>()?;
    let hashes: BTreeMap<_, _> = TICKERS
        .into_iter()
        .zip(
            loaded
                .iter()
                .map(|(_, raw)| format!("{:x}", Sha256::digest(raw))),
        )
        .collect();
    let bars: [Vec<DailyBar>; 2] = loaded
        .into_iter()
        .map(|(bars, _)| bars)
        .collect::<Vec<_>>()
        .try_into()
        .map_err(|_| "expected two inputs")?;
    let (days, mut summary) = compute(&bars, stage)?;
    summary["input_sha256"] = serde_json::to_value(hashes)?;
    let history = indicators(&bars[0])?;
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
        let batch = indicator_batch(&history)?;
        let mut writer = ArrowWriter::try_new(
            fs::File::create(tmp.join("indicators.parquet"))?,
            batch.schema(),
            None,
        )?;
        writer.write(&batch)?;
        writer.close()?;
        fs::write(
            tmp.join("summary.json"),
            serde_json::to_vec_pretty(&summary)?,
        )?;
        Ok(())
    })?;
    Ok(format!(
        "{} stage={}",
        output_hashes(
            out,
            &[
                "bars.parquet",
                "days.parquet",
                "indicators.parquet",
                "summary.json"
            ]
        )?,
        if stage == Stage::Dev {
            "dev"
        } else {
            "holdout"
        }
    ))
}

#[cfg(test)]
mod tests;
