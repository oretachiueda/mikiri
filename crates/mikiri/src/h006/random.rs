use super::{
    daily::{self, Daily, Direction, Order, WEIGHTS},
    input::Bar,
    segments::sharpe,
    splitmix64,
};
use arrow_array::{ArrayRef, BooleanArray, Float64Array, Int64Array, RecordBatch, StringArray};
use arrow_schema::{Field, Schema};
use jiff::civil::{Weekday, date};
use parquet::arrow::ArrowWriter;
use serde::Serialize;
use std::{error::Error, fs::File, path::Path, sync::Arc};

type Result<T> = std::result::Result<T, Box<dyn Error>>;
const ROUNDS: usize = 1000;

#[derive(Debug, Serialize)]
pub struct RandomSummary {
    pub actual_sr: Option<f64>,
    pub round_sr: Vec<Option<f64>>,
    pub excluded_days: Vec<usize>,
    pub percentile: Option<f64>,
    pub gate_d: bool,
}

#[derive(Clone, Debug)]
struct Row {
    round: i64,
    day_index: i64,
    leg: usize,
    k: i16,
    start: Option<i16>,
    exit: Option<i16>,
    stopped: Option<bool>,
    a: Option<f64>,
    c: Option<f64>,
    valid: bool,
}

struct Market<'a> {
    bars: &'a [Bar],
    prices: Vec<f64>,
    offsets: Vec<usize>,
}
impl<'a> Market<'a> {
    fn new(bars: &'a [Bar]) -> Self {
        let mut prices = vec![f64::NAN; 1440];
        let mut offsets = vec![0; 1441];
        let mut at = 0;
        let mut last: Option<(usize, f64)> = None;
        for minute in 0..1440 {
            offsets[minute] = at;
            if at < bars.len()
                && usize::from(bars[at].timestamp.hour() as u8) * 60
                    + usize::from(bars[at].timestamp.minute() as u8)
                    == minute
            {
                last = Some((minute, bars[at].open));
                at += 1;
            }
            if let Some((m, p)) = last
                && minute - m <= 5
            {
                prices[minute] = p;
            }
        }
        offsets[1440] = at;
        Self {
            bars,
            prices,
            offsets,
        }
    }
    fn price(&self, minute: i16) -> Option<f64> {
        let p = *self.prices.get(usize::try_from(minute).ok()?)?;
        if p.is_finite() { Some(p) } else { None }
    }
    fn window(&self, start: i16, end: i16) -> &[Bar] {
        &self.bars[self.offsets[start as usize]..self.offsets[end as usize]]
    }
}

fn window_count(friday: bool, len: i16) -> i16 {
    (if friday { 1018 } else { 1438 }) - len + 1
}
fn start_min(round: usize, day: usize, leg: usize, k: i16) -> i16 {
    let n = ((round as u64) << 32) + (day as u64) * 2 + leg as u64;
    (splitmix64(n) % k as u64) as i16
}
fn overlap(a: (i16, i16), b: (i16, i16)) -> bool {
    a.0.max(b.0) < a.1.min(b.1)
}
fn percentile(actual: f64, rounds: &[Option<f64>]) -> Option<f64> {
    if !actual.is_finite() || rounds.len() != ROUNDS || rounds.iter().any(Option::is_none) {
        return None;
    }
    Some(rounds.iter().flatten().filter(|&&v| v <= actual).count() as f64 / ROUNDS as f64)
}

fn batch(rows: &[Row]) -> Result<RecordBatch> {
    let mut fields = Vec::new();
    let mut arrays: Vec<ArrayRef> = Vec::new();
    macro_rules! col {
        ($name:expr,$array:expr) => {{
            let a: ArrayRef = Arc::new($array);
            fields.push(Field::new($name, a.data_type().clone(), true));
            arrays.push(a);
        }};
    }
    col!(
        "round",
        Int64Array::from(rows.iter().map(|r| r.round).collect::<Vec<_>>())
    );
    col!(
        "day_index",
        Int64Array::from(rows.iter().map(|r| r.day_index).collect::<Vec<_>>())
    );
    col!(
        "leg",
        StringArray::from(
            rows.iter()
                .map(|r| if r.leg == 0 { "eu" } else { "us" })
                .collect::<Vec<_>>()
        )
    );
    col!(
        "K",
        Int64Array::from(rows.iter().map(|r| i64::from(r.k)).collect::<Vec<_>>())
    );
    col!(
        "start_min",
        Int64Array::from(
            rows.iter()
                .map(|r| r.start.map(i64::from))
                .collect::<Vec<_>>()
        )
    );
    col!(
        "exit_min",
        Int64Array::from(
            rows.iter()
                .map(|r| r.exit.map(i64::from))
                .collect::<Vec<_>>()
        )
    );
    col!(
        "stopped",
        BooleanArray::from(rows.iter().map(|r| r.stopped).collect::<Vec<_>>())
    );
    col!(
        "a",
        Float64Array::from(rows.iter().map(|r| r.a).collect::<Vec<_>>())
    );
    col!(
        "c",
        Float64Array::from(rows.iter().map(|r| r.c).collect::<Vec<_>>())
    );
    col!(
        "valid",
        BooleanArray::from(rows.iter().map(|r| r.valid).collect::<Vec<_>>())
    );
    Ok(RecordBatch::try_new(Arc::new(Schema::new(fields)), arrays)?)
}

pub fn compute(days: &[Daily], bars: &[Bar], path: &Path) -> Result<RandomSummary> {
    let days: Vec<_> = days
        .iter()
        .filter(|d| d.date >= date(2007, 7, 2) && d.date <= date(2024, 10, 31))
        .collect();
    if days.windows(2).any(|w| w[0].date >= w[1].date)
        || bars.windows(2).any(|w| w[0].timestamp >= w[1].timestamp)
    {
        return Err("random inputs must be ordered and unique".into());
    }
    let mut markets = Vec::with_capacity(days.len());
    for d in &days {
        if d.leverage.is_none()
            || d.daily_return.is_none()
            || d.legs.iter().any(|l| l.sigma.is_none())
        {
            return Err("random control requires prepared dev days".into());
        }
        let first = bars.partition_point(|b| b.timestamp.date() < d.date);
        let last = bars.partition_point(|b| b.timestamp.date() <= d.date);
        markets.push(Market::new(&bars[first..last]));
    }
    let actual = sharpe(
        &days
            .iter()
            .filter_map(|d| d.daily_return)
            .collect::<Vec<_>>(),
    )
    .ok();
    let empty = batch(&[])?;
    let mut writer = ArrowWriter::try_new(File::create(path)?, empty.schema(), None)?;
    let mut round_sr = Vec::with_capacity(ROUNDS);
    let mut excluded_days = Vec::with_capacity(ROUNDS);
    for round in 0..ROUNDS {
        let mut rows = Vec::with_capacity(days.len() * 2);
        let mut returns = Vec::with_capacity(days.len());
        for (i, (d, market)) in days.iter().zip(&markets).enumerate() {
            let entries = [d.eu_entry_min, 480];
            let mut pair = Vec::with_capacity(2);
            let mut windows = [(0_i16, 0_i16); 2];
            for leg in 0..2 {
                let len = d.legs[leg].walk.exit_min - entries[leg];
                if len < 0 {
                    return Err("actual exit precedes entry".into());
                }
                let k = window_count(d.date.weekday() == Weekday::Friday, len);
                let mut row = Row {
                    round: round as i64,
                    day_index: i as i64,
                    leg,
                    k,
                    start: None,
                    exit: None,
                    stopped: None,
                    a: None,
                    c: None,
                    valid: false,
                };
                if k > 0 {
                    let start = start_min(round, i, leg, k);
                    let end = start + len;
                    row.start = Some(start);
                    windows[leg] = (start, end);
                    if let (Some(entry), Some(exit)) = (market.price(start), market.price(end)) {
                        let direction = if leg == 0 {
                            Direction::Short
                        } else {
                            Direction::Long
                        };
                        let walk = daily::walk(
                            market.window(start, end),
                            start..end,
                            entry,
                            exit,
                            d.legs[leg].sigma,
                            direction,
                            Order::Conservative,
                        )?;
                        let a = if leg == 0 {
                            (entry - walk.exit_price) / entry
                        } else {
                            (walk.exit_price - entry) / entry
                        };
                        let c = 0.2 * 0.0001 * (1.0 / entry + 1.0 / walk.exit_price);
                        if !a.is_finite() || !c.is_finite() {
                            return Err("nonfinite random result".into());
                        }
                        row.exit = Some(walk.exit_min);
                        row.stopped = Some(walk.stopped);
                        row.a = Some(a);
                        row.c = Some(c);
                    }
                }
                pair.push(row);
            }
            let valid = pair.iter().all(|r| r.a.is_some()) && !overlap(windows[0], windows[1]);
            if valid {
                let r = d.leverage.ok_or("missing leverage")?
                    * (WEIGHTS[0]
                        * (pair[0].a.ok_or("missing random return")?
                            - pair[0].c.ok_or("missing random cost")?)
                        + WEIGHTS[1]
                            * (pair[1].a.ok_or("missing random return")?
                                - pair[1].c.ok_or("missing random cost")?));
                if !r.is_finite() {
                    return Err("nonfinite random day return".into());
                }
                returns.push(r);
            }
            for row in &mut pair {
                row.valid = valid;
            }
            rows.extend(pair);
        }
        excluded_days.push(days.len() - returns.len());
        round_sr.push(sharpe(&returns).ok());
        writer.write(&batch(&rows)?)?;
    }
    writer.close()?;
    let percentile = actual.and_then(|v| percentile(v, &round_sr));
    Ok(RandomSummary {
        actual_sr: actual,
        round_sr,
        excluded_days,
        percentile,
        gate_d: percentile.is_some_and(|p| p >= 0.95),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::h006::daily::{Leg, Walk};
    use jiff::civil::Date;
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

    fn day(date: Date, price: f64) -> Daily {
        let leg = |exit| Leg {
            entry_price: price,
            entry_fallback: false,
            boundary_exit_price: price,
            sigma: Some(0.01),
            walk: Walk {
                initial_stop: Some(price),
                flag_min: None,
                stopped: true,
                exit_min: exit,
                exit_price: price,
                double_touches: 0,
            },
            r_nostop: 0.0,
            r: 0.0,
            a: 0.0,
            c: 0.0,
        };
        let c = 0.2 * 0.0001 * (1.0 / price + 1.0 / price);
        let r = 2.0 * (WEIGHTS[0] * (0.0 - c) + WEIGHTS[1] * (0.0 - c));
        Daily {
            date,
            us_dst: false,
            eu_entry_min: 120,
            us_exit_min_boundary: 825,
            legs: [leg(120), leg(480)],
            u: 0.0,
            sigma_hat: Some(0.1),
            leverage: Some(2.0),
            daily_return: Some(r),
        }
    }
    fn minute_bar(date: Date, minute: i16, price: f64) -> Bar {
        Bar {
            timestamp: date.at((minute / 60) as i8, (minute % 60) as i8, 0, 0),
            open: price,
            high: price,
            low: price,
            close: price,
        }
    }
    fn path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("h006-random-{name}-{}.parquet", std::process::id()))
    }

    #[test]
    fn window_counts_and_seeded_starts() {
        assert_eq!(window_count(false, 300), 1139);
        assert_eq!(window_count(true, 0), 1019);
        assert_eq!(window_count(false, 0), 1439);
        assert_eq!(window_count(true, 1020), -1);
        assert_eq!(start_min(0, 0, 0, 1139), 116);
        assert_eq!(start_min(0, 0, 1, 1099), 207);
        assert_eq!(start_min(1, 0, 0, 1019), 945);
        assert_eq!(start_min(999, 4400, 1, 900), 520);
    }
    #[test]
    fn fallback_stays_inside_same_date() {
        let bars = [
            minute_bar(date(2007, 7, 2), 1, 1.0),
            minute_bar(date(2007, 7, 2), 10, 2.0),
        ];
        let market = Market::new(&bars);
        assert_eq!(market.price(0), None);
        assert_eq!(market.price(1), Some(1.0));
        assert_eq!(market.price(6), Some(1.0));
        assert_eq!(market.price(7), None);
        assert_eq!(market.price(10), Some(2.0));
    }
    #[test]
    fn overlap_excludes_nonempty_windows_only() {
        assert!(overlap((10, 20), (19, 30)));
        assert!(!overlap((10, 20), (20, 30)));
        assert!(!overlap((15, 15), (10, 20)));
        assert!(!overlap((10, 20), (15, 15)));
    }
    #[test]
    fn percentile_includes_ties_and_rejects_incomplete_rounds() {
        let mut rounds = vec![Some(1.0); 1000];
        rounds[0] = Some(2.0);
        assert_eq!(percentile(1.0, &rounds), Some(0.999));
        rounds[2] = None;
        assert_eq!(percentile(1.0, &rounds), None);
    }
    #[test]
    fn stopped_actual_leg_sets_shorter_window() {
        let mut d = day(date(2007, 7, 2), 1.0);
        d.legs[0].walk.exit_min = 125;
        let bars: Vec<_> = (0..1440).map(|i| minute_bar(d.date, i, 1.0)).collect();
        let path = path("short");
        let summary = compute(&[d], &bars, &path).unwrap();
        assert!(!summary.gate_d);
        assert_eq!(summary.percentile, None);
        let mut reader = ParquetRecordBatchReaderBuilder::try_new(File::open(&path).unwrap())
            .unwrap()
            .build()
            .unwrap();
        let batch = reader.next().unwrap().unwrap();
        let k = batch
            .column_by_name("K")
            .unwrap()
            .as_any()
            .downcast_ref::<Int64Array>()
            .unwrap();
        assert_eq!(k.value(0), 1438 - 5 + 1);
        let m = batch
            .column_by_name("start_min")
            .unwrap()
            .as_any()
            .downcast_ref::<Int64Array>()
            .unwrap();
        assert_eq!(m.value(0), i64::from(start_min(0, 0, 0, 1434)));
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn overlapping_random_windows_exclude_the_whole_day() {
        let mut d = day(date(2007, 7, 2), 1.0);
        d.legs[0].walk.exit_min = 420;
        d.legs[1].walk.exit_min = 780;
        let bars: Vec<_> = (0..1440).map(|m| minute_bar(d.date, m, 1.0)).collect();
        let path = path("overlap");
        let summary = compute(&[d], &bars, &path).unwrap();
        assert_eq!(summary.excluded_days[0], 1);
        let mut reader = ParquetRecordBatchReaderBuilder::try_new(File::open(&path).unwrap())
            .unwrap()
            .build()
            .unwrap();
        let batch = reader.next().unwrap().unwrap();
        let valid = batch
            .column_by_name("valid")
            .unwrap()
            .as_any()
            .downcast_ref::<BooleanArray>()
            .unwrap();
        assert!(!valid.value(0));
        assert!(!valid.value(1));
        let start = batch
            .column_by_name("start_min")
            .unwrap()
            .as_any()
            .downcast_ref::<Int64Array>()
            .unwrap();
        assert_eq!((start.value(0), start.value(1)), (116, 390));
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn impossible_window_and_missing_prices_exclude_the_day() {
        let mut d = day(date(2007, 7, 2), 1.0);
        d.legs[0].walk.exit_min = 1560;
        let path = path("impossible");
        let summary = compute(&[d], &[], &path).unwrap();
        assert!(summary.excluded_days.iter().all(|&n| n == 1));
        assert!(!summary.gate_d);
        let mut reader = ParquetRecordBatchReaderBuilder::try_new(File::open(&path).unwrap())
            .unwrap()
            .build()
            .unwrap();
        let batch = reader.next().unwrap().unwrap();
        let k = batch
            .column_by_name("K")
            .unwrap()
            .as_any()
            .downcast_ref::<Int64Array>()
            .unwrap();
        assert_eq!(k.value(0), -1);
        let valid = batch
            .column_by_name("valid")
            .unwrap()
            .as_any()
            .downcast_ref::<BooleanArray>()
            .unwrap();
        assert!(!valid.value(0));
        assert!(!valid.value(1));
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn zero_variance_round_is_not_computable() {
        let days = [day(date(2007, 7, 2), 1.0), day(date(2007, 7, 3), 1.0)];
        let mut bars = Vec::new();
        for d in &days {
            bars.extend((0..1440).map(|m| minute_bar(d.date, m, 1.0)));
        }
        let path = path("zero");
        let result = compute(&days, &bars, &path).unwrap();
        assert!(result.excluded_days.iter().all(|&n| n == 0));
        assert!(result.round_sr.iter().all(Option::is_none));
        assert!(result.percentile.is_none());
        assert!(!result.gate_d);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn constant_zero_length_windows_count_ties_and_write_all_rows() {
        let d1 = day(date(2007, 7, 2), 1.0);
        let d2 = day(date(2007, 7, 3), 2.0);
        let mut bars = Vec::new();
        for (d, p) in [(d1.date, 1.0), (d2.date, 2.0)] {
            bars.extend((0..1440).map(|m| minute_bar(d, m, p)));
        }
        let path = path("ties");
        let summary = compute(&[d1, d2], &bars, &path).unwrap();
        assert_eq!(summary.percentile, Some(1.0));
        assert!(summary.gate_d);
        assert!(summary.excluded_days.iter().all(|&n| n == 0));
        let reader = ParquetRecordBatchReaderBuilder::try_new(File::open(&path).unwrap())
            .unwrap()
            .build()
            .unwrap();
        let mut count = 0;
        for batch in reader {
            let batch = batch.unwrap();
            count += batch.num_rows();
            let names: Vec<_> = batch
                .schema()
                .fields()
                .iter()
                .map(|f| f.name().clone())
                .collect();
            assert_eq!(
                names,
                [
                    "round",
                    "day_index",
                    "leg",
                    "K",
                    "start_min",
                    "exit_min",
                    "stopped",
                    "a",
                    "c",
                    "valid"
                ]
            );
        }
        assert_eq!(count, 4000);
        std::fs::remove_file(path).unwrap();
    }
}
