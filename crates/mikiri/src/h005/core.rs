use super::{Observation, Result, Stage};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use time::{Date, Month, Weekday};

const CURRENCIES: [&str; 9] = [
    "EUR", "JPY", "GBP", "CHF", "CAD", "AUD", "NZD", "NOK", "SEK",
];
const FX: [&str; 9] = [
    "DEXUSEU", "DEXJPUS", "DEXUSUK", "DEXSZUS", "DEXCAUS", "DEXUSAL", "DEXUSNZ", "DEXNOUS",
    "DEXSDUS",
];
const AREAS: [&str; 10] = [
    "EA20", "JPN", "GBR", "CHE", "CAN", "AUS", "NZL", "NOR", "SWE", "USA",
];
const FIRST_RATES: [&str; 10] = [
    "IR3TIB01EZM156N",
    "IR3TIB01JPM156N",
    "IR3TIB01GBM156N",
    "IR3TIB01CHM156N",
    "IR3TIB01CAM156N",
    "IR3TIB01AUM156N",
    "IR3TIB01NZM156N",
    "IR3TIB01NOM156N",
    "IR3TIB01SEM156N",
    "IR3TIB01USM156N",
];
const SCENARIOS: [(&str, f64, f64, f64); 8] = [
    ("verdict", 1.0, 0.0001, 0.0025),
    ("k1_5", 1.5, 0.0001, 0.0025),
    ("k1_8", 1.8, 0.0001, 0.0025),
    ("k2", 2.0, 0.0001, 0.0025),
    ("k3", 3.0, 0.0001, 0.0025),
    ("zero", 1.0, 0.0, 0.0),
    ("double", 1.0, 0.0002, 0.005),
    ("high", 1.0, 0.0001, 0.01),
];

#[derive(Debug)]
pub struct Computed {
    pub summary: Value,
    pub indicators: Vec<Value>,
    pub days: Vec<Value>,
}

type Series = BTreeMap<Date, Option<f64>>;
#[derive(Default)]
struct Sources {
    fx: [Series; 9],
    rates: [Series; 10],
    first_fx: [Series; 9],
    first_rates: [Series; 10],
    spy: BTreeMap<Date, (f64, f64, f64)>,
}
#[derive(Clone)]
struct Day {
    date: Date,
    spot: [f64; 9],
    rates: [f64; 10],
    interest: Option<[f64; 9]>,
    x: Option<[f64; 9]>,
    mean: Option<[f64; 9]>,
    sigma: Option<[f64; 9]>,
    cumulative: Option<[f64; 9]>,
    signal: Option<[i8; 9]>,
    month_end: bool,
    close: f64,
    dividend: f64,
    split: f64,
}

fn date(s: &str) -> Result<Date> {
    if !s.is_ascii()
        || s.len() != 10
        || &s[4..5] != "-"
        || &s[7..8] != "-"
        || !s
            .bytes()
            .enumerate()
            .all(|(i, b)| i == 4 || i == 7 || b.is_ascii_digit())
    {
        return Err(format!("invalid ISO date: {s}").into());
    }
    Ok(Date::from_calendar_date(
        s[..4].parse()?,
        Month::try_from(s[5..7].parse::<u8>()?)?,
        s[8..].parse()?,
    )?)
}
fn next(d: Date) -> Result<Date> {
    d.next_day().ok_or_else(|| "date overflow".into())
}
fn month(d: Date) -> (i32, u8) {
    (d.year(), d.month() as u8)
}
fn month_start(y: i32, m: u8) -> Result<Date> {
    Ok(Date::from_calendar_date(y, Month::try_from(m)?, 1)?)
}
fn previous_month(d: Date) -> Result<Date> {
    let (y, m) = month(d);
    month_start(
        if m == 1 { y - 1 } else { y },
        if m == 1 { 12 } else { m - 1 },
    )
}
fn business(d: Date) -> bool {
    !matches!(d.weekday(), Weekday::Saturday | Weekday::Sunday)
}
fn business_count(mut first: Date, last: Date) -> Result<usize> {
    let mut count = 0;
    while first <= last {
        count += usize::from(business(first));
        first = next(first)?;
    }
    Ok(count)
}
fn finite(value: Option<f64>, label: &str) -> Result<f64> {
    value
        .filter(|v| v.is_finite())
        .ok_or_else(|| format!("missing or non-finite {label}").into())
}
fn sources(rows: &[Observation], stage: Stage) -> Result<Sources> {
    let mut source = Sources::default();
    let mut seen = BTreeSet::new();
    for row in rows {
        let monthly = matches!(row.kind.as_str(), "rate" | "first_rate");
        if stage == Stage::Dev
            && if monthly {
                row.date.as_str() >= "2024-10-01"
            } else {
                row.date.as_str() > "2024-10-31"
            }
        {
            continue;
        }
        let day = date(&row.date)?;
        if monthly && day.day() != 1 {
            return Err("monthly observations must be dated first day".into());
        }
        if !seen.insert((&row.kind, &row.series, day)) {
            return Err("duplicate observation".into());
        }
        let target = match row.kind.as_str() {
            "fx" | "first_fx" => {
                let j = FX
                    .iter()
                    .position(|s| *s == row.series)
                    .ok_or("unknown FX series")?;
                if row.value.is_some_and(|v| !v.is_finite() || v <= 0.0) {
                    return Err("FX must be positive and finite".into());
                }
                if row.kind == "fx" {
                    &mut source.fx[j]
                } else {
                    &mut source.first_fx[j]
                }
            }
            "rate" | "first_rate" => {
                let names = if row.kind == "rate" {
                    &AREAS
                } else {
                    &FIRST_RATES
                };
                let j = names
                    .iter()
                    .position(|s| *s == row.series)
                    .ok_or("unknown rate series")?;
                if row.value.is_some_and(|v| !v.is_finite()) {
                    return Err("rate must be finite".into());
                }
                if row.kind == "rate" {
                    &mut source.rates[j]
                } else {
                    &mut source.first_rates[j]
                }
            }
            "spy" => {
                if row.series != "SPY" {
                    return Err("unknown SPY series".into());
                }
                let close = finite(row.close, "SPY close")?;
                let div = finite(row.div_cash, "SPY dividend")?;
                let split = finite(row.split_factor, "SPY split")?;
                if close <= 0.0 || split <= 0.0 || div < 0.0 || !business(day) {
                    return Err("invalid SPY observation".into());
                }
                source.spy.insert(day, (close, div, split));
                continue;
            }
            _ => return Err("unknown observation kind".into()),
        };
        target.insert(day, row.value);
    }
    if source.fx.iter().any(BTreeMap::is_empty) || source.rates.iter().any(BTreeMap::is_empty) {
        return Err("all nine FX and ten rate series required".into());
    }
    Ok(source)
}
fn spot(series: &Series, day: Date, j: usize) -> Result<Option<f64>> {
    if let Some((&observed, Some(value))) = series.range(..=day).rev().find(|(_, v)| v.is_some())
        && business_count(next(observed)?, day)? <= 5
    {
        return Ok(Some(if matches!(j, 1 | 3 | 4 | 7 | 8) {
            1.0 / value
        } else {
            *value
        }));
    }
    Ok(None)
}
fn rate(series: &Series, day: Date, area: usize, current_us: &Series) -> Result<Option<f64>> {
    let wanted = previous_month(day)?;
    if area == 9 && wanted == date("2020-04-01")? {
        return Ok(current_us
            .get(&date("2020-03-01")?)
            .copied()
            .flatten()
            .map(|v| v / 100.0));
    }
    if let Some(Some(value)) = series.get(&wanted) {
        return Ok(Some(*value / 100.0));
    }
    if business_count(month_start(day.year(), day.month() as u8)?, day)? > 5 {
        return Ok(None);
    }
    Ok(series
        .range(..wanted)
        .rev()
        .find_map(|(_, v)| *v)
        .map(|v| v / 100.0))
}
fn integrated(
    rates: &[Series; 10],
    current_us: &Series,
    u: Date,
    t: Date,
    j: usize,
) -> Result<Option<f64>> {
    let mut value = 0.0;
    let mut a = u;
    while a < t {
        let (Some(foreign), Some(us)) = (
            rate(&rates[j], a, j, current_us)?,
            rate(&rates[9], a, 9, current_us)?,
        ) else {
            return Ok(None);
        };
        value += (foreign - us) / 365.0;
        a = next(a)?;
    }
    Ok(Some(value))
}
fn sign(v: f64) -> i8 {
    if v > 0.0 {
        1
    } else if v < 0.0 {
        -1
    } else {
        0
    }
}

fn history(source: &Sources, stage: Stage) -> Result<Vec<Day>> {
    let end = date(if stage == Stage::Dev {
        "2024-10-31"
    } else {
        "2026-10-30"
    })?;
    let start = date("2008-01-01")?;
    let mut days: Vec<Day> = Vec::new();
    let mut weight = 0.0;
    let mut mean = [0.0; 9];
    let mut m2 = [0.0; 9];
    for (&day, &(close, dividend, split)) in source.spy.range(start..=end) {
        let mut prices = [0.0; 9];
        let mut rates = [0.0; 10];
        for (j, price) in prices.iter_mut().enumerate() {
            *price = spot(&source.fx[j], day, j)?.ok_or_else(|| {
                format!("FX gap exceeds five business days: {} {day}", CURRENCIES[j])
            })?;
        }
        for (j, r) in rates.iter_mut().enumerate() {
            *r = rate(&source.rates[j], day, j, &source.rates[9])?.ok_or_else(|| {
                format!(
                    "rate gap exceeds five business days: {} {day}; not confirmed",
                    AREAS[j]
                )
            })?;
        }
        let mut interest = None;
        let mut returns = None;
        let mut avg = None;
        let mut vol = None;
        if let Some(previous) = days.last() {
            let mut carry = [0.0; 9];
            let mut x = [0.0; 9];
            let mut sigma = [0.0; 9];
            weight = 60.0 / 61.0 * weight + 1.0;
            for (j, price) in prices.iter().enumerate() {
                carry[j] = integrated(&source.rates, &source.rates[9], previous.date, day, j)?
                    .ok_or("missing interval rate")?;
                x[j] = price / previous.spot[j] - 1.0 + carry[j];
                let delta = x[j] - mean[j];
                mean[j] += delta / weight;
                m2[j] = 60.0 / 61.0 * m2[j] + delta * (x[j] - mean[j]);
                sigma[j] = (261.0 * m2[j] / weight).sqrt();
                if !x[j].is_finite() || !sigma[j].is_finite() {
                    return Err("non-finite return or EWMA".into());
                }
            }
            interest = Some(carry);
            returns = Some(x);
            avg = Some(mean);
            vol = Some(sigma);
        }
        days.push(Day {
            date: day,
            spot: prices,
            rates,
            interest,
            x: returns,
            mean: avg,
            sigma: vol,
            cumulative: None,
            signal: None,
            month_end: false,
            close,
            dividend,
            split,
        });
    }
    if days.first().is_none_or(|d| month(d.date) != (2008, 1))
        || days.last().is_none_or(|d| d.date != end)
    {
        return Err("SPY must cover January 2008 through period end".into());
    }
    let mut ends = BTreeMap::new();
    for i in 0..days.len() {
        days[i].month_end = i + 1 == days.len() || month(days[i].date) != month(days[i + 1].date);
        if days[i].month_end {
            if days[i]
                .sigma
                .is_some_and(|sigma| sigma.iter().any(|v| *v <= 0.0 || !v.is_finite()))
            {
                return Err("volatility must be positive and finite".into());
            }
            ends.insert(month(days[i].date), i);
        }
    }
    for i in 0..days.len() {
        if !days[i].month_end {
            continue;
        }
        let (y, m) = month(days[i].date);
        let Some(&l) = ends.get(&(y - 1, m)) else {
            continue;
        };
        let mut cumulative = [0.0; 9];
        let mut signals = [0; 9];
        for (j, value) in cumulative.iter_mut().enumerate() {
            let mut product = 1.0;
            for d in &days[l + 1..=i] {
                product *= 1.0 + d.x.ok_or("missing preparation return")?[j];
            }
            *value = product - 1.0;
            if !value.is_finite() {
                return Err("non-finite twelve-month return".into());
            }
            signals[j] = sign(*value);
        }
        days[i].cumulative = Some(cumulative);
        days[i].signal = Some(signals);
    }
    Ok(days)
}

#[derive(Default)]
struct Taxes {
    losses: BTreeMap<i32, f64>,
}
impl Taxes {
    fn settle(&mut self, year: i32, income: f64) -> f64 {
        self.losses.retain(|v, _| *v + 3 >= year);
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
        0.20315 * taxable
    }
    fn rows(&self) -> Vec<Value> {
        self.losses
            .iter()
            .map(|(y, v)| json!({"year": y, "amount": v}))
            .collect()
    }
}
fn target(day: &Day, index: usize, equity: f64, k: f64) -> Result<[f64; 9]> {
    if index < 261 {
        return Err("signal needs at least 261 returns".into());
    }
    let signals = day
        .signal
        .ok_or("twelve-month signal preparation insufficient")?;
    let sigma = day.sigma.ok_or("volatility preparation insufficient")?;
    let mut amounts = [0.0; 9];
    for (j, amount) in amounts.iter_mut().enumerate() {
        if sigma[j] <= 0.0 || !sigma[j].is_finite() {
            return Err("volatility must be positive and finite".into());
        }
        *amount = f64::from(signals[j]) * equity * k * 0.40 / sigma[j] / 9.0;
        if !amount.is_finite() {
            return Err("non-finite target".into());
        }
    }
    Ok(amounts)
}
fn benchmark(days: &[Day]) -> Result<(Vec<f64>, Value)> {
    let first = days.first().ok_or("empty benchmark")?;
    let last = days.last().ok_or("empty benchmark")?;
    let mut shares = 1.0 / 1.0002 / first.close;
    let mut values = Vec::new();
    let mut peak: f64 = 1.0;
    let mut drawdown: f64 = 0.0;
    let mut index = 1.0 / 1.0002;
    for (i, day) in days.iter().enumerate() {
        if i > 0 {
            let dividend = shares * day.dividend;
            shares = shares * day.split + dividend / day.close;
            index *= (day.close * day.split + day.dividend) / days[i - 1].close;
        }
        let value = shares * day.close * if i + 1 == days.len() { 0.9998 } else { 1.0 };
        let checked = index * if i + 1 == days.len() { 0.9998 } else { 1.0 };
        if !value.is_finite()
            || value <= 0.0
            || (value - checked).abs() > 1e-10 * value.abs().max(1.0)
        {
            return Err("SPY total-return reconciliation failed".into());
        }
        peak = peak.max(value);
        drawdown = drawdown.max(1.0 - value / peak);
        values.push(value);
    }
    let years = f64::from((last.date - first.date).whole_days() as i32) / 365.0;
    if years <= 0.0 {
        return Err("period needs positive calendar span".into());
    }
    let end = *values.last().ok_or("empty benchmark values")?;
    let cagr = end.powf(1.0 / years) - 1.0;
    Ok((
        values,
        json!({"cagr_pretax":cagr,"cagr_aftertax":cagr,"drawdown":drawdown}),
    ))
}
fn alive(e: f64) -> bool {
    e > 0.0 && e.is_finite()
}
fn array_sum(a: &[f64; 9]) -> f64 {
    a.iter().sum()
}
struct Simulated {
    report: Value,
    days: Vec<Value>,
    z: Vec<f64>,
    ruined: bool,
}

fn simulate(
    history: &[Day],
    start: &str,
    end: &str,
    period: &str,
    scenario: (&str, f64, f64, f64),
) -> Result<Simulated> {
    let start = date(start)?;
    let end = date(end)?;
    let begin = history
        .iter()
        .position(|d| d.date == start)
        .ok_or("missing exact period start SPY session")?;
    let finish = history
        .iter()
        .position(|d| d.date == end)
        .ok_or("missing exact period end SPY session")?;
    let signal_index = history[..begin]
        .iter()
        .rposition(|d| d.month_end)
        .ok_or("missing prior month end")?;
    let (id, k, c, f) = scenario;
    let slice = &history[begin..=finish];
    let (baseline, benchmark_report) = benchmark(slice)?;
    let b = *baseline.last().ok_or("empty baseline")?;
    let mut pending = Some(target(&history[signal_index], signal_index, 1.0, k)?);
    let mut q: [f64; 9] = [0.0; 9];
    let mut equity = 1.0;
    let mut asset = 1.0;
    let mut tax_state = Taxes::default();
    let mut income = 0.0;
    let mut total_tax = 0.0;
    let mut contributions = [0.0; 9];
    let mut turnover = 0.0;
    let mut peak_a: f64 = 1.0;
    let mut peak_e: f64 = 1.0;
    let mut draw_a: f64 = 0.0;
    let mut draw_e: f64 = 0.0;
    let mut max_ratio: f64 = 0.0;
    let mut min_inverse: Option<f64> = None;
    let mut month_denominator = 1.0;
    let mut worst_month: Option<f64> = None;
    let mut year_a = 1.0;
    let mut year_e = 1.0;
    let mut yearly = Vec::new();
    let mut output = Vec::new();
    let mut z = Vec::new();
    let mut ruin_date = None;
    let mut last_day = start;
    for (offset, day) in slice.iter().enumerate() {
        let i = begin + offset;
        let incoming = q;
        let previous_equity = equity;
        let mut profit = [0.0; 9];
        let mut gross = [0.0; 9];
        let mut financing = [0.0; 9];
        let mut transaction = [0.0; 9];
        let before: f64 = incoming
            .iter()
            .zip(day.spot)
            .map(|(q, s)| q.abs() * s)
            .sum();
        if alive(equity) && before.is_finite() {
            max_ratio = max_ratio.max(before / equity);
            if before > 0.0 {
                let ratio = equity / before;
                min_inverse = Some(min_inverse.map_or(ratio, |v| v.min(ratio)));
            }
        }
        let mut ruined = false;
        if offset > 0 {
            let previous = &history[i - 1];
            let x = day.x.ok_or("missing trading return")?;
            for (j, quantity) in incoming.iter().enumerate() {
                gross[j] = quantity.abs() * previous.spot[j];
                profit[j] = quantity * previous.spot[j] * x[j];
            }
            equity += array_sum(&profit);
            ruined = !alive(equity);
            if !ruined {
                let elapsed = (day.date - previous.date).whole_days() as f64;
                for (j, cost) in financing.iter_mut().enumerate() {
                    *cost = f * gross[j] * elapsed / 365.0;
                }
                equity -= array_sum(&financing);
                ruined = !alive(equity);
            }
        }
        let mut executed = None;
        if !ruined {
            let amount = if i == finish {
                Some([0.0; 9])
            } else {
                pending.take()
            };
            if let Some(amounts) = amount {
                let before_trade = equity;
                let mut traded = 0.0;
                for (j, quantity) in q.iter_mut().enumerate() {
                    *quantity = amounts[j] / day.spot[j];
                    let notional = (*quantity - incoming[j]).abs() * day.spot[j];
                    transaction[j] = c * notional;
                    traded += notional;
                }
                turnover += traded / before_trade;
                equity -= array_sum(&transaction);
                executed = Some(amounts);
                ruined = !alive(equity);
            }
        }
        let v = equity;
        let r = if offset > 0 {
            Some(v / previous_equity - 1.0)
        } else {
            None
        };
        if offset == 0 {
            asset = v;
        } else if let Some(ret) = r {
            asset *= 1.0 + ret;
        }
        if !asset.is_finite() || r.is_some_and(|ret| !ret.is_finite()) {
            ruined = true;
        }
        for (j, total) in contributions.iter_mut().enumerate() {
            let contribution = profit[j] - transaction[j] - financing[j];
            *total += contribution;
            income += contribution;
        }
        let mut tax = 0.0;
        let year_end =
            offset + 1 == slice.len() || slice[offset + 1].date.year() != day.date.year();
        if year_end && !ruined {
            tax = tax_state.settle(day.date.year(), income);
            equity -= tax;
            total_tax += tax;
            ruined = !alive(equity);
            yearly.push(json!({"year":day.date.year(),"pretax_return":asset/year_a-1.0,"aftertax_return":equity/year_e-1.0,"income":income,"tax":tax,"losses":tax_state.rows()}));
            year_a = asset;
            year_e = equity;
        }
        if asset.is_finite() {
            peak_a = peak_a.max(asset);
            draw_a = draw_a.max(1.0 - asset / peak_a);
        }
        if equity.is_finite() {
            peak_e = peak_e.max(equity);
            draw_e = draw_e.max(1.0 - equity / peak_e);
        }
        if day.month_end || i == finish || ruined {
            let ret = equity / month_denominator - 1.0;
            if ret.is_finite() {
                worst_month = Some(worst_month.map_or(ret, |v| v.min(ret)));
            }
            month_denominator = equity;
        }
        let after: f64 = (0..9).map(|j| q[j].abs() * day.spot[j]).sum();
        if alive(equity) && after.is_finite() {
            max_ratio = max_ratio.max(after / equity);
            if after > 0.0 {
                let ratio = equity / after;
                min_inverse = Some(min_inverse.map_or(ratio, |v| v.min(ratio)));
            }
        }
        let mut row = json!({"period":period,"scenario":id,"date":day.date.to_string(),"V":v,"E":equity,"R":r,"A":asset,"tax":tax,"B":baseline[offset],"year_income":income,"turnover":turnover,"gross_before":before,"gross_after":after});
        for (j, currency) in CURRENCIES.iter().enumerate() {
            for (name, value) in [
                ("q_before", Some(incoming[j])),
                ("q_after", Some(q[j])),
                ("N", executed.map(|n| n[j])),
                ("P", Some(profit[j])),
                ("G", Some(gross[j])),
                ("transaction", Some(transaction[j])),
                ("financing", Some(financing[j])),
            ] {
                row[format!("{name}_{currency}")] = json!(value);
            }
        }
        output.push(row);
        last_day = day.date;
        if ruined {
            ruin_date = Some(day.date.to_string());
            break;
        }
        if let Some(ret) = r {
            if !ret.is_finite() {
                return Err("non-finite daily return".into());
            }
            z.push(ret);
        }
        if year_end {
            income = 0.0;
        }
        if day.month_end && i < finish {
            pending = Some(target(day, i, equity, k)?);
        }
    }
    let ruined = ruin_date.is_some();
    let elapsed = (end - start).whole_days() as f64;
    let mut contribution = json!({});
    for (j, currency) in CURRENCIES.iter().enumerate() {
        contribution[*currency] = json!(contributions[j]);
    }
    let cagr_a = if ruined {
        None
    } else {
        Some(asset.powf(365.0 / elapsed) - 1.0)
    };
    let cagr_e = if ruined {
        None
    } else {
        Some(equity.powf(365.0 / elapsed) - 1.0)
    };
    let contribution_total = array_sum(&contributions);
    let net = contribution_total - total_tax;
    let gain = equity - 1.0;
    let reconciliation = net.is_finite()
        && gain.is_finite()
        && (net - gain).abs() <= 1e-10 * net.abs().max(gain.abs()).max(1.0);
    let report = json!({"scenario":id,"k":k,"c":c,"f":f,"status":if ruined {"ruined"} else {"ok"},"ruin_date":ruin_date,"start":start.to_string(),"end":last_day.to_string(),"W":equity,"B":b,"A":asset,"cagr_pretax":cagr_a,"cagr_aftertax":cagr_e,"drawdown_pretax":draw_a,"drawdown_aftertax":draw_e,"worst_month":worst_month,"max_gross_to_equity":max_ratio,"min_equity_to_gross":min_inverse,"turnover":turnover,"total_tax":total_tax,"contribution":contribution,"contribution_total":contribution_total,"reconciliation":reconciliation,"yearly":yearly,"benchmark":benchmark_report});
    Ok(Simulated {
        report,
        days: output,
        z,
        ruined,
    })
}

fn available(values: &[Value], key: &str) -> (Option<String>, Option<String>) {
    let mut dates = values
        .iter()
        .filter(|v| !v[key].is_null())
        .filter_map(|v| v["date"].as_str());
    let first = dates.next().map(str::to_owned);
    let last = dates
        .next_back()
        .map(str::to_owned)
        .or_else(|| first.clone());
    (first, last)
}
fn alternate_signals(history: &[Day], returns: &[Option<f64>]) -> BTreeMap<Date, Option<i8>> {
    let ends: BTreeMap<_, _> = history
        .iter()
        .enumerate()
        .filter(|(_, d)| d.month_end)
        .map(|(i, d)| (month(d.date), i))
        .collect();
    let mut result = BTreeMap::new();
    for (i, d) in history.iter().enumerate().filter(|(_, d)| d.month_end) {
        let (y, m) = month(d.date);
        let value = ends.get(&(y - 1, m)).and_then(|&l| {
            let mut product = 1.0;
            for value in &returns[l + 1..=i] {
                product *= 1.0 + (*value)?;
            }
            if product.is_finite() {
                Some(sign(product - 1.0))
            } else {
                None
            }
        });
        result.insert(d.date, value);
    }
    result
}
fn vintage(source: &Sources, history: &[Day], stage: Stage) -> Result<Value> {
    let report_start = date(if stage == Stage::Dev {
        "2010-01-01"
    } else {
        "2024-11-01"
    })?;
    let mut spot_rows = Vec::new();
    let mut rate_signal_rows = Vec::new();
    let minimum_rate = date("2013-06-01")?;
    let mut first_rates = source.first_rates.clone();
    for series in &mut first_rates {
        series.retain(|d, _| *d >= minimum_rate);
    }
    for (j, currency) in CURRENCIES.iter().enumerate() {
        let mut first_spots = Vec::with_capacity(history.len());
        for d in history {
            first_spots.push(spot(&source.first_fx[j], d.date, j)?);
        }
        let mut returns = vec![None; history.len()];
        let mut rate_returns = vec![None; history.len()];
        for i in 1..history.len() {
            let day = &history[i];
            let prev = &history[i - 1];
            if let (Some(s), Some(p), Some(interest)) =
                (first_spots[i], first_spots[i - 1], day.interest)
            {
                let value = s / p - 1.0 + interest[j];
                if value.is_finite() {
                    returns[i] = Some(value);
                }
            }
            if let Some(carry) = integrated(&first_rates, &source.rates[9], prev.date, day.date, j)?
            {
                let value = day.spot[j] / prev.spot[j] - 1.0 + carry;
                if value.is_finite() {
                    rate_returns[i] = Some(value);
                }
            }
        }
        let signals = alternate_signals(history, &returns);
        let rate_signals = alternate_signals(history, &rate_returns);
        let mut values = Vec::new();
        let mut signal_values = Vec::new();
        let mut comparable = 0;
        let mut flips = 0;
        let mut rate_comparable = 0;
        let mut rate_flips = 0;
        for (i, d) in history
            .iter()
            .enumerate()
            .filter(|(_, d)| d.month_end && d.date >= report_start)
        {
            let current_signal = d.signal.map(|s| s[j]);
            let first_signal = signals.get(&d.date).copied().flatten();
            if let (Some(a), Some(b)) = (current_signal, first_signal) {
                comparable += 1;
                flips += usize::from(a != b);
            }
            values.push(json!({"date":d.date.to_string(),"current":d.spot[j],"first":first_spots[i],"difference":first_spots[i].map(|s|d.spot[j]-s),"current_signal":current_signal,"first_signal":first_signal}));
            if d.date >= minimum_rate {
                let first_signal = rate_signals.get(&d.date).copied().flatten();
                if let (Some(a), Some(b)) = (current_signal, first_signal) {
                    rate_comparable += 1;
                    rate_flips += usize::from(a != b);
                }
                signal_values.push(json!({"date":d.date.to_string(),"current_signal":current_signal,"first_signal":first_signal}));
            }
        }
        let (a, b) = available(&values, "first");
        spot_rows.push(json!({"currency":currency,"series":FX[j],"available_start":a,"available_end":b,"comparable_months":comparable,"signal_flips":flips,"values":values}));
        let (a, b) = available(&signal_values, "first_signal");
        rate_signal_rows.push(json!({"currency":currency,"available_start":a,"available_end":b,"comparable_months":rate_comparable,"signal_flips":rate_flips,"values":signal_values}));
    }
    let end = date(if stage == Stage::Dev {
        "2024-09-01"
    } else {
        "2026-09-01"
    })?;
    let mut rates = Vec::new();
    for (j, area) in AREAS.iter().enumerate() {
        let mut values = Vec::new();
        let mut m = minimum_rate;
        while m <= end {
            let excluded = j == 9 && m == date("2020-04-01")?;
            let current = if excluded {
                None
            } else {
                source.rates[j]
                    .get(&m)
                    .copied()
                    .flatten()
                    .map(|v| v / 100.0)
            };
            let first = if excluded {
                None
            } else {
                first_rates[j].get(&m).copied().flatten().map(|v| v / 100.0)
            };
            values.push(json!({"date":m.to_string(),"current":current,"first":first,"difference":current.zip(first).map(|(a,b)|a-b)}));
            m = month_start(
                if m.month() == Month::December {
                    m.year() + 1
                } else {
                    m.year()
                },
                if m.month() == Month::December {
                    1
                } else {
                    m.month() as u8 + 1
                },
            )?;
        }
        let (a, b) = available(&values, "difference");
        let comparable = values.iter().filter(|v| !v["difference"].is_null()).count();
        rates.push(json!({"area":area,"series":FIRST_RATES[j],"available_start":a,"available_end":b,"comparable_months":comparable,"values":values}));
    }
    Ok(
        json!({"spot":spot_rows,"rates":rates,"rate_signals":rate_signal_rows,"eur_note":"OECD EA20 versus ALFRED EA19: differences include aggregation definitions"}),
    )
}

pub fn compute(rows: &[Observation], stage: Stage) -> Result<Computed> {
    let source = sources(rows, stage)?;
    let history = history(&source, stage)?;
    let mut day = history.first().ok_or("empty history")?.date;
    let end = history.last().ok_or("empty history")?.date;
    while day <= end {
        if business(day) {
            for (j, currency) in CURRENCIES.iter().enumerate() {
                if spot(&source.fx[j], day, j)?.is_none() {
                    return Err(
                        format!("FX gap exceeds five business days: {} {day}", currency).into(),
                    );
                }
            }
        }
        day = next(day)?;
    }
    let mut indicators = Vec::with_capacity(history.len() * 9);
    for d in &history {
        for (j, currency) in CURRENCIES.iter().enumerate() {
            indicators.push(json!({"date":d.date.to_string(),"currency":currency,"S":d.spot[j],"r_foreign":d.rates[j],"r_us":d.rates[9],"interest":d.interest.map(|v|v[j]),"x":d.x.map(|v|v[j]),"mean":d.mean.map(|v|v[j]),"sigma":d.sigma.map(|v|v[j]),"X":d.cumulative.map(|v|v[j]),"signal":d.signal.map(|v|v[j]),"month_end":d.month_end}));
        }
    }
    let scenarios: Vec<_> = SCENARIOS
        .iter()
        .map(|&(scenario, k, c, f)| json!({"scenario":scenario,"k":k,"c":c,"f":f}))
        .collect();
    let mut summary = json!({"stage":stage.name(),"n_cumulative":5,"actual_scenarios":scenarios,"report_tables":{},"vintage":vintage(&source,&history,stage)?});
    let periods = if stage == Stage::Dev {
        vec![
            ("main", "2010-01-04", "2024-10-31"),
            ("recent", "2019-11-01", "2024-10-31"),
        ]
    } else {
        vec![("holdout", "2024-11-01", "2026-10-30")]
    };
    let mut days = Vec::new();
    for (period, start, end) in periods {
        let mut reports = Vec::new();
        for scenario in SCENARIOS {
            let result = simulate(&history, start, end, period, scenario)?;
            if scenario.0 == "verdict" {
                let passes = !result.ruined
                    && result.report["W"]
                        .as_f64()
                        .zip(result.report["B"].as_f64())
                        .is_some_and(|(w, b)| w > b);
                summary[period] = result.report.clone();
                match period {
                    "main" => {
                        let probability = if result.ruined {
                            None
                        } else {
                            Some(crate::h001::psr(&result.z)?)
                        };
                        summary["psr_main"] = json!(probability);
                        summary["psr_t"] = json!(result.z.len());
                        summary["psr_threshold"] = json!(0.99);
                        summary["gate_a"] = json!(passes);
                        summary["gate_b"] = json!(probability.is_some_and(|p| p >= 0.99));
                    }
                    "recent" => summary["gate_c"] = json!(passes),
                    _ => summary["gate_h"] = json!(passes),
                }
            }
            reports.push(result.report);
            days.extend(result.days);
        }
        summary["report_tables"][period] = json!({"multipliers":["verdict","k1_5","k1_8","k2","k3"],"costs":["zero","verdict","double","high"],"rows":reports});
    }
    Ok(Computed {
        summary,
        indicators,
        days,
    })
}

#[cfg(test)]
#[path = "core_tests.rs"]
mod tests;
