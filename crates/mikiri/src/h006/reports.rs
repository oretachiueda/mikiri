use super::{
    daily::{self, Daily, Mode, Order, WEIGHTS},
    input::{Bar, Day, SeasonClock, Stage},
    random::RandomSummary,
    segments::{self, Computed, Segment},
    splitmix64,
};
use crate::tiingo::DailyBar;
use jiff::civil::{Weekday, date};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fs,
    path::Path,
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;
const TAX: f64 = 0.20315;
const CAP: f64 = 43.27239725805739;

#[derive(Debug, Serialize, PartialEq)]
pub struct Common {
    pub ruined: bool,
    pub ruin_date: Option<String>,
    pub w: Option<f64>,
    pub b: f64,
    pub cagr_pretax: Option<f64>,
    pub cagr_aftertax: Option<f64>,
    pub drawdown_pretax: Option<f64>,
    pub drawdown_aftertax: Option<f64>,
    pub max_leverage: f64,
    pub max_leg_notional_to_equity: f64,
}

fn segment_days(days: &[Daily], segment: Segment) -> &[Daily] {
    let (start, end) = segment.bounds();
    let first = days.partition_point(|d| d.date < start);
    let last = days.partition_point(|d| d.date <= end);
    &days[first..last]
}
fn common(segment: Segment, days: &[Daily], spy: &[DailyBar], rate: f64) -> Result<Common> {
    let first = days.first().ok_or("empty report segment")?;
    let last = days.last().ok_or("empty report segment")?;
    let (rows, ruin) = segments::account_report(segment, days, rate)?;
    let b = segments::baseline(spy, segment.bounds().0, segment.bounds().1)?;
    let max_leverage = days.iter().filter_map(|d| d.leverage).fold(0.0, f64::max);
    let mut out = Common {
        ruined: ruin.is_some(),
        ruin_date: ruin.map(|d| d.to_string()),
        w: None,
        b,
        cagr_pretax: None,
        cagr_aftertax: None,
        drawdown_pretax: None,
        drawdown_aftertax: None,
        max_leverage,
        max_leg_notional_to_equity: max_leverage * WEIGHTS[0],
    };
    if ruin.is_none() {
        let end = rows.last().ok_or("empty report accounts")?;
        let span = last.date.duration_since(first.date).as_secs() as f64 / 86400.0;
        if span <= 0.0 {
            return Err("report needs positive calendar span".into());
        }
        let pretax = end.equity_pretax.powf(365.0 / span) - 1.0;
        let aftertax = end.equity_end.powf(365.0 / span) - 1.0;
        if !pretax.is_finite() || !aftertax.is_finite() {
            return Err("nonfinite report CAGR".into());
        }
        out.w = Some(end.equity_end);
        out.cagr_pretax = Some(pretax);
        out.cagr_aftertax = Some(aftertax);
        out.drawdown_pretax = Some(segments::drawdown(rows.iter().map(|r| r.equity_pretax)));
        out.drawdown_aftertax = Some(segments::drawdown(rows.iter().map(|r| r.equity_end)));
    }
    Ok(out)
}
fn scenario(days: &[Daily], pip: f64, target: f64, included: [bool; 2]) -> Result<Vec<Daily>> {
    let mut out = days.to_vec();
    for d in &mut out {
        let sigma = d.sigma_hat.ok_or("missing report volatility")?;
        let leverage = (target / sigma).min(CAP);
        for leg in &mut d.legs {
            leg.c = pip * 0.5 * 0.0001 * (1.0 / leg.entry_price + 1.0 / leg.walk.exit_price);
        }
        let r = leverage
            * (0..2)
                .filter(|&i| included[i])
                .map(|i| WEIGHTS[i] * (d.legs[i].a - d.legs[i].c))
                .sum::<f64>();
        if !r.is_finite() || !leverage.is_finite() {
            return Err("invalid report scenario".into());
        }
        d.leverage = Some(leverage);
        d.daily_return = Some(r);
    }
    Ok(out)
}
fn breakeven(days: &[Daily]) -> Option<(f64, f64)> {
    let mut gross = 0.0;
    let mut cost = 0.0;
    for d in days {
        let l = d.leverage?;
        for (leg, weight) in d.legs.iter().zip(WEIGHTS) {
            gross += l * weight * leg.a;
            cost += l * weight * leg.c;
        }
    }
    if cost == 0.0 || !gross.is_finite() || !cost.is_finite() {
        return None;
    }
    let k = gross / cost;
    Some((k, k * 0.4))
}
fn tails(days: &[Daily]) -> Value {
    let mut values: Vec<f64> = days
        .iter()
        .flat_map(|d| d.legs.iter().map(|l| l.a - l.c))
        .collect();
    if values.is_empty() {
        return Value::Null;
    }
    values.sort_unstable_by(f64::total_cmp);
    let n = values.len();
    let wins: Vec<_> = values.iter().copied().filter(|v| *v > 0.0).collect();
    let losses: Vec<_> = values.iter().copied().filter(|v| *v < 0.0).collect();
    let ratio = if wins.is_empty() || losses.is_empty() {
        None
    } else {
        Some(
            (wins.iter().sum::<f64>() / wins.len() as f64)
                / (-(losses.iter().sum::<f64>() / losses.len() as f64)),
        )
    };
    json!({"p01":values[n.div_ceil(100)-1],"p05":values[(n*5).div_ceil(100)-1],"worst":values[0],"win_loss_ratio":ratio,"win_rate":wins.len() as f64/n as f64})
}
fn bootstrap_index(round: u64, index: usize, n: usize) -> usize {
    (splitmix64((round << 32) + index as u64) % n as u64) as usize
}
fn bootstrap(days: &[Daily]) -> Result<Value> {
    let returns: Vec<_> = days
        .iter()
        .map(|d| d.daily_return.ok_or("missing bootstrap return"))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if returns.is_empty() {
        return Err("empty bootstrap input".into());
    }
    let n = returns.len();
    let mut mdds = Vec::with_capacity(1000);
    for d in 0..1000_u64 {
        let mut equity: f64 = 1.0;
        let mut peak: f64 = 1.0;
        let mut mdd: f64 = 0.0;
        for i in 0..n {
            let index = bootstrap_index(d, i, n);
            equity *= 1.0 + returns[index];
            if equity <= 0.0 {
                mdd = 1.0;
                break;
            }
            if !equity.is_finite() {
                return Err("nonfinite bootstrap equity".into());
            }
            peak = peak.max(equity);
            mdd = mdd.max(1.0 - equity / peak);
        }
        mdds.push(mdd);
    }
    mdds.sort_unstable_by(f64::total_cmp);
    Ok(json!({"p05":mdds[49],"p50":mdds[499],"p95":mdds[949]}))
}
fn median(mut values: Vec<f64>) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_unstable_by(f64::total_cmp);
    let n = values.len();
    Some((values[(n - 1) / 2] + values[n / 2]) / 2.0)
}
fn annual(days: &[Daily]) -> Value {
    let mut years: BTreeMap<i16, Vec<f64>> = BTreeMap::new();
    let mut seasons: [Vec<f64>; 2] = Default::default();
    for d in days {
        if let Some(r) = d.daily_return {
            years.entry(d.date.year()).or_default().push(r);
            seasons[usize::from(d.us_dst)].push(r);
        }
    }
    let yearly: Vec<_> = years
        .iter()
        .map(|(year, rs)| json!({"year":year,"days":rs.len(),"sr":segments::sharpe(rs).ok()}))
        .collect();
    let med = median(
        years
            .values()
            .filter_map(|v| segments::sharpe(v).ok())
            .collect(),
    );
    json!({"years":yearly,"median_sr":med,"winter":{"days":seasons[0].len(),"sr":segments::sharpe(&seasons[0]).ok()},"summer":{"days":seasons[1].len(),"sr":segments::sharpe(&seasons[1]).ok()}})
}
fn flip_warning(original: Option<f64>, reverse: Option<f64>) -> bool {
    original.is_some_and(|v| v > 0.0) && reverse.is_some_and(|v| v > 0.0)
}

pub struct SummaryInput<'a> {
    pub stage: Stage,
    pub input_hashes: &'a BTreeMap<String, String>,
    pub raw_bar_count: usize,
    pub removed: &'a [jiff::civil::DateTime],
    pub clock: &'a [SeasonClock; 2],
    pub bars: &'a [Bar],
    pub boundaries: &'a [Day],
    pub daily: &'a [Daily],
    pub spy: &'a [DailyBar],
    pub judgment: &'a Computed,
    pub random: &'a RandomSummary,
}
fn excluded_days(stage: Stage, bars: &[Bar], days: &[Day]) -> Result<Vec<Value>> {
    let all: BTreeSet<_> = bars.iter().map(|b| b.timestamp.date()).collect();
    let valid: BTreeSet<_> = days.iter().map(|d| d.date).collect();
    let end = if stage == Stage::Dev {
        date(2024, 10, 31)
    } else {
        date(2026, 10, 30)
    };
    let mut date = date(2007, 1, 1);
    let mut rows = Vec::new();
    while date <= end {
        if !matches!(date.weekday(), Weekday::Saturday | Weekday::Sunday) && !valid.contains(&date)
        {
            rows.push(json!({"date":date.to_string(),"reason":if all.contains(&date) {"missing_boundary"} else {"no_bars"}}));
        }
        date = date.tomorrow()?;
    }
    Ok(rows)
}
pub fn assemble(input: &SummaryInput<'_>) -> Result<Value> {
    let s0 = daily::compute(
        input.boundaries,
        input.bars,
        Mode::NoStop,
        false,
        Order::Conservative,
        0.4,
        0.3,
    )?;
    let reversed = daily::compute(
        input.boundaries,
        input.bars,
        Mode::Stop,
        true,
        Order::Conservative,
        0.4,
        0.3,
    )?;
    let optimistic = daily::compute(
        input.boundaries,
        input.bars,
        Mode::Stop,
        false,
        Order::Optimistic,
        0.4,
        0.3,
    )?;
    if input.daily.len() != optimistic.len()
        || input.daily.iter().zip(&optimistic).any(|(a, b)| {
            a.date != b.date
                || a.daily_return.map(f64::to_bits) != b.daily_return.map(f64::to_bits)
                || a.legs.iter().zip(&b.legs).any(|(x, y)| {
                    x.walk.exit_min != y.walk.exit_min
                        || x.walk.exit_price.to_bits() != y.walk.exit_price.to_bits()
                })
        })
    {
        return Err("optimistic and conservative values differ".into());
    }
    let segments = if input.stage == Stage::Dev {
        vec![Segment::Dev, Segment::Recent]
    } else {
        vec![Segment::Dev, Segment::Recent, Segment::Holdout]
    };
    let mut report = serde_json::Map::new();
    let mut combinations = Vec::new();
    for segment in segments {
        let days = segment_days(input.daily, segment);
        let main = common(segment, days, input.spy, TAX)?;
        if main.ruined {
            return Err("main scenario ruined".into());
        }
        let mut tax = Vec::new();
        for rate in [0.0, 0.05] {
            tax.push(json!({"rate":rate,"common":common(segment,days,input.spy,rate)?}));
            combinations.push(json!({"segment":segment,"table":"tax","rate":rate}));
        }
        let mut cost_target = Vec::new();
        for pip in [0.0, 0.4, 0.8, 1.5] {
            for target in [0.3, 0.4] {
                let changed = scenario(days, pip, target, [true, true])?;
                let row = common(segment, &changed, input.spy, TAX)?;
                if pip == 0.4
                    && target == 0.3
                    && (row != main
                        || changed.iter().zip(days).any(|(a, b)| {
                            a.daily_return.map(f64::to_bits) != b.daily_return.map(f64::to_bits)
                        }))
                {
                    return Err("main report scenario mismatch".into());
                }
                cost_target.push(json!({"pip":pip,"target":target,"common":row}));
                combinations.push(
                    json!({"segment":segment,"table":"cost_target","pip":pip,"target":target}),
                );
            }
        }
        let mut ablations = Vec::new();
        for (name, included) in [("eu_only", [true, false]), ("us_only", [false, true])] {
            let changed = scenario(days, 0.4, 0.3, included)?;
            ablations.push(json!({"name":name,"common":common(segment,&changed,input.spy,TAX)?}));
            combinations.push(json!({"segment":segment,"table":"ablation","name":name}));
        }
        ablations.push(
            json!({"name":"s0","common":common(segment,segment_days(&s0,segment),input.spy,TAX)?}),
        );
        combinations.push(json!({"segment":segment,"table":"ablation","name":"s0"}));
        let original_sr = segments::sharpe(
            &days
                .iter()
                .filter_map(|d| d.daily_return)
                .collect::<Vec<_>>(),
        )
        .ok();
        let flipped = segment_days(&reversed, segment);
        let reversed_sr = segments::sharpe(
            &flipped
                .iter()
                .filter_map(|d| d.daily_return)
                .collect::<Vec<_>>(),
        )
        .ok();
        let (k, pip) = breakeven(days)
            .map(|(k, p)| (Some(k), Some(p)))
            .unwrap_or((None, None));
        let double_touches: usize = days
            .iter()
            .flat_map(|d| d.legs.iter())
            .map(|l| l.walk.double_touches)
            .sum();
        let data = json!({
            "main":main,"tax_rates":tax,"cost_target":cost_target,
            "breakeven":{"common":common(segment,days,input.spy,TAX)?,"multiple":k,"pip":pip},
            "tails":{"common":common(segment,days,input.spy,TAX)?,"values":tails(days)},
            "bootstrap":{"common":common(segment,days,input.spy,TAX)?,"values":bootstrap(days)?},
            "ablations":ablations,
            "same_bar":{"common":common(segment,days,input.spy,TAX)?,"optimistic_common":common(segment,segment_days(&optimistic,segment),input.spy,TAX)?,"double_touches":double_touches,"optimistic_matches":true},
            "flip":{"common":common(segment,flipped,input.spy,TAX)?,"original_sr":original_sr,"reversed_sr":reversed_sr,"warning":flip_warning(original_sr,reversed_sr)},
            "yearly":{"common":common(segment,days,input.spy,TAX)?,"values":annual(days)}
        });
        for name in [
            "breakeven",
            "tails",
            "bootstrap",
            "same_bar",
            "flip",
            "yearly",
        ] {
            combinations.push(json!({"segment":segment,"table":name}));
        }
        report.insert(segment.name().into(), data);
    }
    let excluded = excluded_days(input.stage, input.bars, input.boundaries)?;
    let stage = if input.stage == Stage::Dev {
        "dev"
    } else {
        "holdout"
    };
    Ok(json!({
        "stage":stage,"input_sha256":input.input_hashes,
        "counts":{"bars_input":input.raw_bar_count,"bars_clean":input.bars.len(),"removed_bars":input.removed.len(),"calculation_days":input.daily.len(),"excluded_days":excluded.len()},
        "first_calculation_day":input.daily.first().map(|d|d.date.to_string()),"excluded_days":excluded,
        "clock_check":{"winter":&input.clock[0],"summer":&input.clock[1]},
        "segments":&input.judgment.segments,"psr":input.judgment.psr,"psr_threshold":1.0-0.05/6.0,
        "gates":{"A":input.judgment.gates.a,"B":input.judgment.gates.b,"C":input.judgment.gates.c,"D":input.random.gate_d,"H":input.judgment.gates.h},
        "random_control":input.random,"reports":report,"report_combinations":combinations,
        "plateau_in_sample":{"A":{"S0":1.369,"S1":1.350,"S2":1.043,"S3":1.452},"B":{"S0":1.245,"S1":1.353,"S2":1.236,"S3":1.484},"C":{"S0":1.453,"S1":1.510,"S2":1.207,"S3":1.584}},
        "holm":"B equals the first Holm step for one setting"
    }))
}
pub fn write_summary(path: &Path, summary: &Value) -> Result<()> {
    let mut content = serde_json::to_vec_pretty(summary)?;
    content.push(b'\n');
    fs::write(path, content)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        h006::{
            daily::{Leg, Walk},
            segments,
        },
        tiingo,
    };
    use jiff::civil::Date;
    fn day(date: Date, r: f64) -> Daily {
        let leg = Leg {
            entry_price: 0.02,
            entry_fallback: false,
            boundary_exit_price: 0.02,
            sigma: Some(0.01),
            walk: Walk {
                initial_stop: Some(0.02),
                flag_min: None,
                stopped: false,
                exit_min: 480,
                exit_price: 0.02,
                double_touches: 0,
            },
            r_nostop: 0.,
            r: 0.,
            a: 0.01,
            c: 0.002,
        };
        Daily {
            date,
            us_dst: false,
            eu_entry_min: 120,
            us_exit_min_boundary: 825,
            legs: [leg; 2],
            u: 0.,
            sigma_hat: Some(0.15),
            leverage: Some(2.),
            daily_return: Some(r),
        }
    }
    fn spy() -> Vec<DailyBar> {
        tiingo::parse_daily_bars(br#"[{"date":"2007-07-02T00:00:00Z","close":100,"divCash":0,"splitFactor":1},{"date":"2008-01-02T00:00:00Z","close":100,"divCash":0,"splitFactor":1},{"date":"2019-11-01T00:00:00Z","close":100,"divCash":0,"splitFactor":1},{"date":"2020-11-01T00:00:00Z","close":100,"divCash":0,"splitFactor":1}]"#).unwrap()
    }
    fn near(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-12, "{a} != {b}");
    }
    #[test]
    fn nearest_rank_tails_and_trade_statistics() {
        let mut days = Vec::new();
        for i in 0..100 {
            let mut d = day(date(2020, 1, 1), 0.);
            d.legs[0].a = (2 * i) as f64 - 100.;
            d.legs[1].a = (2 * i + 1) as f64 - 100.;
            d.legs[0].c = 0.;
            d.legs[1].c = 0.;
            days.push(d);
        }
        let t = tails(&days);
        assert_eq!(t["p01"], -99.);
        assert_eq!(t["p05"], -91.);
        assert_eq!(t["worst"], -100.);
        near(t["win_rate"].as_f64().unwrap(), 99. / 200.);
        near(t["win_loss_ratio"].as_f64().unwrap(), 50. / 50.5);
    }
    #[test]
    fn bootstrap_seed_indices_and_single_return() {
        assert_eq!(bootstrap_index(0, 0, 7), 0);
        assert_eq!(bootstrap_index(0, 1, 7), 4);
        assert_eq!(bootstrap_index(1, 0, 7), 5);
        let values = bootstrap(&[day(date(2020, 1, 1), -0.1)]).unwrap();
        near(values["p05"].as_f64().unwrap(), 0.1);
        assert_eq!(values["p05"], values["p50"]);
        assert_eq!(values["p50"], values["p95"]);
    }
    #[test]
    fn break_even_and_cost_linear_and_ablation_identity() {
        let days = [day(date(2020, 1, 1), 0.)];
        let (multiple, pip) = breakeven(&days).unwrap();
        near(multiple, 5.);
        near(pip, 2.);
        let flat = scenario(&days, pip, 0.3, [true, true]).unwrap();
        near(flat[0].daily_return.unwrap(), 0.);
        let all = scenario(&days, 0., 0.3, [true, true]).unwrap();
        let eu = scenario(&days, 0., 0.3, [true, false]).unwrap();
        let us = scenario(&days, 0., 0.3, [false, true]).unwrap();
        near(
            all[0].daily_return.unwrap(),
            eu[0].daily_return.unwrap() + us[0].daily_return.unwrap(),
        );
        let high = scenario(&days, 0., 0.4, [true, true]).unwrap();
        near(high[0].leverage.unwrap(), 0.4 / 0.15);
    }
    #[test]
    fn tax_rows_and_ruined_row_are_reported() {
        let days = [day(date(2019, 11, 1), 0.1), day(date(2020, 11, 1), 0.1)];
        let free = common(Segment::Recent, &days, &spy(), 0.).unwrap();
        let taxed = common(Segment::Recent, &days, &spy(), 0.05).unwrap();
        assert!(free.w.unwrap() > taxed.w.unwrap());
        let bad = [day(date(2019, 11, 1), -1.1), day(date(2020, 11, 1), 0.1)];
        let row = common(Segment::Recent, &bad, &spy(), TAX).unwrap();
        assert!(row.ruined);
        assert_eq!(row.ruin_date.as_deref(), Some("2019-11-01"));
        assert_eq!(row.w, None);
    }
    #[test]
    fn flip_warning_requires_both_positive() {
        assert!(flip_warning(Some(0.1), Some(0.2)));
        assert!(!flip_warning(Some(0.0), Some(0.2)));
        assert!(!flip_warning(Some(0.1), Some(-0.2)));
        assert!(!flip_warning(None, Some(0.1)));
    }
    #[test]
    fn yearly_even_median_and_seasons() {
        let mut a = day(date(2019, 1, 1), 0.01);
        let mut b = day(date(2019, 7, 1), 0.03);
        let c = day(date(2020, 1, 1), -0.01);
        let mut d = day(date(2020, 7, 1), 0.03);
        a.us_dst = false;
        b.us_dst = true;
        d.us_dst = true;
        let row = annual(&[a, b, c, d]);
        let s1 = segments::sharpe(&[0.01, 0.03]).unwrap();
        let s2 = segments::sharpe(&[-0.01, 0.03]).unwrap();
        near(row["median_sr"].as_f64().unwrap(), (s1 + s2) / 2.);
        assert_eq!(row["summer"]["days"], 2);
        assert_eq!(row["winter"]["days"], 2);
    }
    #[test]
    fn summary_assembles_all_tables_and_plateau() {
        let mut dates: Vec<_> = (0..130)
            .map(|i| {
                date(2007, 1, 2)
                    .checked_add(jiff::Span::new().days(i))
                    .unwrap()
            })
            .collect();
        dates.extend([
            date(2007, 7, 2),
            date(2008, 1, 2),
            date(2019, 11, 1),
            date(2020, 11, 1),
        ]);
        let mut boundaries = Vec::new();
        let mut bars = Vec::new();
        for (i, date) in dates.into_iter().enumerate() {
            let x = (i as f64 * 0.41).sin() * 0.002;
            let y = (i as f64 * 0.73).cos() * 0.001;
            let prices = [1.0, 1.0 + x, 1.0 + x + y];
            boundaries.push(Day {
                date,
                eu_min: 120,
                us_exit_min: 825,
                prices,
                fallback: [false; 3],
            });
            for (minute, price) in [(120_i16, prices[0]), (480, prices[1]), (825, prices[2])] {
                bars.push(Bar {
                    timestamp: date.at((minute / 60) as i8, (minute % 60) as i8, 0, 0),
                    open: price,
                    high: price * 1.0001,
                    low: price * 0.9999,
                    close: price,
                });
            }
        }
        let daily = daily::compute(
            &boundaries,
            &bars,
            Mode::Stop,
            false,
            Order::Conservative,
            0.4,
            0.3,
        )
        .unwrap();
        let spy = spy();
        let judgment = segments::compute(&daily, &spy, Stage::Dev).unwrap();
        let random = RandomSummary {
            actual_sr: Some(0.),
            round_sr: vec![Some(0.); 1000],
            excluded_days: vec![0; 1000],
            percentile: Some(1.),
            gate_d: true,
        };
        let clock = [
            SeasonClock {
                top: vec![(510, 0.1)],
                sunday_median: 1020.,
                sundays: 1,
            },
            SeasonClock {
                top: vec![(510, 0.1)],
                sunday_median: 1020.,
                sundays: 1,
            },
        ];
        let hashes = BTreeMap::from([("synthetic.zip".into(), "0".repeat(64))]);
        let input = SummaryInput {
            stage: Stage::Dev,
            input_hashes: &hashes,
            raw_bar_count: bars.len(),
            removed: &[],
            clock: &clock,
            bars: &bars,
            boundaries: &boundaries,
            daily: &daily,
            spy: &spy,
            judgment: &judgment,
            random: &random,
        };
        let summary = assemble(&input).unwrap();
        assert_eq!(summary["plateau_in_sample"]["C"]["S3"], 1.584);
        assert_eq!(summary["gates"]["D"], true);
        assert_eq!(
            summary["reports"]["dev"]["cost_target"]
                .as_array()
                .unwrap()
                .len(),
            8
        );
        assert_eq!(
            summary["reports"]["recent"]["ablations"]
                .as_array()
                .unwrap()
                .len(),
            3
        );
        assert_eq!(
            summary["reports"]["dev"]["same_bar"]["optimistic_matches"],
            true
        );
        assert_eq!(
            summary["holm"],
            "B equals the first Holm step for one setting"
        );
        let path =
            std::env::temp_dir().join(format!("h006-report-test-{}.json", std::process::id()));
        write_summary(&path, &summary).unwrap();
        let disk: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(disk["stage"], summary["stage"]);
        assert_eq!(disk["gates"], summary["gates"]);
        near(
            disk["segments"][0]["w"].as_f64().unwrap(),
            summary["segments"][0]["w"].as_f64().unwrap(),
        );
        fs::remove_file(path).unwrap();
    }
}
