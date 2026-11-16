use super::input::{Bar, Day};
use arrow_array::{ArrayRef, BooleanArray, Float64Array, Int64Array, RecordBatch, StringArray};
use arrow_schema::{Field, Schema};
use jiff::{
    civil::{Date, date},
    tz::{Dst, TimeZone},
};
use parquet::arrow::ArrowWriter;
use std::{error::Error, fs::File, path::Path, sync::Arc};

type Result<T> = std::result::Result<T, Box<dyn Error>>;
pub const WEIGHTS: [f64; 2] = [0.5777354984728738, 0.42226450152712625];
const CAP: f64 = 43.27239725805739;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Long,
    Short,
}
impl Direction {
    fn sign(self) -> f64 {
        match self {
            Self::Long => 1.0,
            Self::Short => -1.0,
        }
    }
}
#[derive(Clone, Copy, Debug)]
pub enum Mode {
    Stop,
    NoStop,
}
#[derive(Clone, Copy, Debug)]
pub enum Order {
    Conservative,
    Optimistic,
}
#[derive(Clone, Copy, Debug)]
pub struct Walk {
    pub initial_stop: Option<f64>,
    pub flag_min: Option<i16>,
    pub stopped: bool,
    pub exit_min: i16,
    pub exit_price: f64,
    pub double_touches: usize,
}

pub fn walk(
    bars: &[Bar],
    window: std::ops::Range<i16>,
    entry: f64,
    boundary_exit: f64,
    sigma: Option<f64>,
    direction: Direction,
    order: Order,
) -> Result<Walk> {
    let start = window.start;
    let end = window.end;
    if !entry.is_finite()
        || entry <= 0.0
        || !boundary_exit.is_finite()
        || boundary_exit <= 0.0
        || start > end
    {
        return Err("invalid walk input".into());
    }
    let Some(sigma) = sigma else {
        return Ok(Walk {
            initial_stop: None,
            flag_min: None,
            stopped: false,
            exit_min: end,
            exit_price: boundary_exit,
            double_touches: 0,
        });
    };
    if !sigma.is_finite() || sigma <= 0.0 {
        return Err("invalid leg sigma".into());
    }
    let sign = direction.sign();
    let initial_stop = entry * (-sign * sigma).exp();
    let favorable = entry * (sign * sigma).exp();
    if !initial_stop.is_finite() || !favorable.is_finite() || initial_stop <= 0.0 {
        return Err("invalid stop price".into());
    }
    let mut result = Walk {
        initial_stop: Some(initial_stop),
        flag_min: None,
        stopped: false,
        exit_min: end,
        exit_price: boundary_exit,
        double_touches: 0,
    };
    let mut stop = initial_stop;
    for b in bars {
        let minute = i16::from(b.timestamp.hour()) * 60 + i16::from(b.timestamp.minute());
        if minute < start || minute >= end {
            continue;
        }
        let hit_stop = match direction {
            Direction::Long => b.low <= stop,
            Direction::Short => b.high >= stop,
        };
        let hit_favorable = match direction {
            Direction::Long => b.high >= favorable,
            Direction::Short => b.low <= favorable,
        };
        if hit_stop && hit_favorable {
            result.double_touches += 1;
        }
        if matches!(order, Order::Optimistic) && hit_favorable && result.flag_min.is_none() {
            result.flag_min = Some(minute);
        }
        if hit_stop {
            result.stopped = true;
            result.exit_min = minute;
            result.exit_price = match direction {
                Direction::Long => b.open.min(stop),
                Direction::Short => b.open.max(stop),
            };
            break;
        }
        if hit_favorable && result.flag_min.is_none() {
            result.flag_min = Some(minute);
        }
        if result.flag_min.is_some() {
            stop = entry;
        }
    }
    Ok(result)
}

#[derive(Clone, Copy, Debug)]
pub struct Leg {
    pub entry_price: f64,
    pub entry_fallback: bool,
    pub boundary_exit_price: f64,
    pub sigma: Option<f64>,
    pub walk: Walk,
    pub r_nostop: f64,
    pub r: f64,
    pub a: f64,
    pub c: f64,
}
#[derive(Clone, Debug)]
pub struct Daily {
    pub date: Date,
    pub us_dst: bool,
    pub eu_entry_min: i16,
    pub us_exit_min_boundary: i16,
    pub legs: [Leg; 2],
    pub u: f64,
    pub sigma_hat: Option<f64>,
    pub leverage: Option<f64>,
    pub daily_return: Option<f64>,
}

fn stdev(xs: &[f64]) -> Result<f64> {
    if xs.len() != 60 {
        return Err("requires 60 prior calculation days".into());
    }
    let mean = xs[0] + xs.iter().map(|x| x - xs[0]).sum::<f64>() / 60.0;
    let sd = (xs.iter().map(|x| (x - mean) * (x - mean)).sum::<f64>() / 59.0).sqrt();
    if !sd.is_finite() || sd <= 0.0 {
        return Err("nonpositive or nonfinite volatility".into());
    }
    Ok(sd)
}

pub fn compute(
    days: &[Day],
    bars: &[Bar],
    mode: Mode,
    reverse: bool,
    order: Order,
    pip: f64,
    target: f64,
) -> Result<Vec<Daily>> {
    if !pip.is_finite() || pip < 0.0 || !target.is_finite() || target <= 0.0 {
        return Err("invalid pip or volatility target".into());
    }
    let ny = TimeZone::get("America/New_York")?;
    let mut out: Vec<Daily> = Vec::with_capacity(days.len());
    for day in days {
        if out.last().is_some_and(|d| d.date >= day.date) {
            return Err("calculation days must increase".into());
        }
        let first = bars.partition_point(|b| b.timestamp.date() < day.date);
        let last = bars.partition_point(|b| b.timestamp.date() <= day.date);
        let day_bars = &bars[first..last];
        let starts = [day.eu_min, 480];
        let ends = [480, day.us_exit_min];
        let directions = if reverse {
            [Direction::Long, Direction::Short]
        } else {
            [Direction::Short, Direction::Long]
        };
        let mut legs = Vec::with_capacity(2);
        for i in 0..2 {
            let entry = day.prices[i];
            let exit = day.prices[i + 1];
            let direction = directions[i];
            if !entry.is_finite() || entry <= 0.0 || !exit.is_finite() || exit <= 0.0 {
                return Err("invalid boundary price".into());
            }
            let r_nostop = direction.sign() * (exit / entry).ln();
            let sigma = if out.len() >= 60 {
                Some(stdev(
                    &out[out.len() - 60..]
                        .iter()
                        .map(|d| d.legs[i].r_nostop)
                        .collect::<Vec<_>>(),
                )?)
            } else {
                None
            };
            let walk_sigma = if matches!(mode, Mode::Stop) {
                sigma
            } else {
                None
            };
            let walked = walk(
                day_bars,
                starts[i]..ends[i],
                entry,
                exit,
                walk_sigma,
                direction,
                order,
            )?;
            let r = direction.sign() * (walked.exit_price / entry).ln();
            let a = direction.sign() * (walked.exit_price - entry) / entry;
            let c = pip * 0.5 * 0.0001 * (1.0 / entry + 1.0 / walked.exit_price);
            if ![r_nostop, r, a, c].iter().all(|v| v.is_finite()) {
                return Err("nonfinite leg result".into());
            }
            legs.push(Leg {
                entry_price: entry,
                entry_fallback: day.fallback[i],
                boundary_exit_price: exit,
                sigma,
                walk: walked,
                r_nostop,
                r,
                a,
                c,
            });
        }
        let legs: [Leg; 2] = legs.try_into().map_err(|_| "missing leg")?;
        let u = WEIGHTS[0] * legs[0].r + WEIGHTS[1] * legs[1].r;
        let sigma_hat = if out.len() >= 60 {
            Some(
                252.0_f64.sqrt()
                    * stdev(
                        &out[out.len() - 60..]
                            .iter()
                            .map(|d| d.u)
                            .collect::<Vec<_>>(),
                    )?,
            )
        } else {
            None
        };
        if day.date >= date(2007, 7, 2) && (sigma_hat.is_none() || legs[0].sigma.is_none()) {
            return Err("insufficient preparation history for dev".into());
        }
        if sigma_hat.is_some_and(|v| !v.is_finite() || v <= 0.0) {
            return Err("invalid annualized volatility".into());
        }
        let leverage = sigma_hat.map(|v| (target / v).min(CAP));
        let daily_return = leverage.map(|l| {
            l * (WEIGHTS[0] * (legs[0].a - legs[0].c) + WEIGHTS[1] * (legs[1].a - legs[1].c))
        });
        if !u.is_finite() || daily_return.is_some_and(|r| !r.is_finite()) {
            return Err("nonfinite daily result".into());
        }
        let noon = ny.to_zoned(day.date.at(12, 0, 0, 0))?.timestamp();
        out.push(Daily {
            date: day.date,
            us_dst: ny.to_offset_info(noon).dst() == Dst::Yes,
            eu_entry_min: day.eu_min,
            us_exit_min_boundary: day.us_exit_min,
            legs,
            u,
            sigma_hat,
            leverage,
            daily_return,
        });
    }
    Ok(out)
}

pub fn write_days(path: &Path, days: &[Daily]) -> Result<()> {
    let mut fields = Vec::new();
    let mut arrays: Vec<ArrayRef> = Vec::new();
    macro_rules! col {
        ($name:expr, $array:expr) => {{
            let array: ArrayRef = Arc::new($array);
            fields.push(Field::new($name, array.data_type().clone(), true));
            arrays.push(array);
        }};
    }
    col!(
        "date",
        StringArray::from(days.iter().map(|d| d.date.to_string()).collect::<Vec<_>>())
    );
    col!(
        "us_dst",
        BooleanArray::from(days.iter().map(|d| d.us_dst).collect::<Vec<_>>())
    );
    col!(
        "eu_entry_min",
        Int64Array::from(
            days.iter()
                .map(|d| i64::from(d.eu_entry_min))
                .collect::<Vec<_>>()
        )
    );
    col!("us_entry_min", Int64Array::from(vec![480_i64; days.len()]));
    col!(
        "us_exit_min_boundary",
        Int64Array::from(
            days.iter()
                .map(|d| i64::from(d.us_exit_min_boundary))
                .collect::<Vec<_>>()
        )
    );
    for (i, prefix) in ["eu", "us"].into_iter().enumerate() {
        macro_rules! leg_f64 {
            ($name:literal, $f:expr) => {
                col!(
                    format!("{prefix}_{}", $name),
                    Float64Array::from(
                        days.iter()
                            .map(|d| $f(&d.legs[i]))
                            .collect::<Vec<Option<f64>>>()
                    )
                );
            };
        }
        macro_rules! leg_i64 {
            ($name:literal, $f:expr) => {
                col!(
                    format!("{prefix}_{}", $name),
                    Int64Array::from(
                        days.iter()
                            .map(|d| $f(&d.legs[i]))
                            .collect::<Vec<Option<i64>>>()
                    )
                );
            };
        }
        leg_f64!("entry_price", |l: &Leg| Some(l.entry_price));
        col!(
            format!("{prefix}_entry_fallback"),
            BooleanArray::from(
                days.iter()
                    .map(|d| d.legs[i].entry_fallback)
                    .collect::<Vec<_>>()
            )
        );
        leg_f64!("boundary_exit_price", |l: &Leg| Some(l.boundary_exit_price));
        leg_f64!("sigma", |l: &Leg| l.sigma);
        leg_f64!("stop_initial", |l: &Leg| l.walk.initial_stop);
        leg_i64!("flag_min", |l: &Leg| l.walk.flag_min.map(i64::from));
        col!(
            format!("{prefix}_stopped"),
            BooleanArray::from(
                days.iter()
                    .map(|d| d.legs[i].walk.stopped)
                    .collect::<Vec<_>>()
            )
        );
        leg_i64!("exit_min", |l: &Leg| Some(i64::from(l.walk.exit_min)));
        leg_f64!("exit_price", |l: &Leg| Some(l.walk.exit_price));
        leg_f64!("r_nostop", |l: &Leg| Some(l.r_nostop));
        leg_f64!("r", |l: &Leg| Some(l.r));
        leg_f64!("a", |l: &Leg| Some(l.a));
        leg_f64!("c", |l: &Leg| Some(l.c));
    }
    col!(
        "u",
        Float64Array::from(days.iter().map(|d| d.u).collect::<Vec<_>>())
    );
    col!(
        "sigma_hat",
        Float64Array::from(days.iter().map(|d| d.sigma_hat).collect::<Vec<_>>())
    );
    col!(
        "leverage",
        Float64Array::from(days.iter().map(|d| d.leverage).collect::<Vec<_>>())
    );
    col!(
        "R",
        Float64Array::from(days.iter().map(|d| d.daily_return).collect::<Vec<_>>())
    );
    let batch = RecordBatch::try_new(Arc::new(Schema::new(fields)), arrays)?;
    let mut writer = ArrowWriter::try_new(File::create(path)?, batch.schema(), None)?;
    writer.write(&batch)?;
    writer.close()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::date;
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

    fn bar(minute: i16, open: f64, high: f64, low: f64) -> Bar {
        Bar {
            timestamp: date(2007, 1, 2).at((minute / 60) as i8, (minute % 60) as i8, 0, 0),
            open,
            high,
            low,
            close: open,
        }
    }
    fn do_walk(bars: &[Bar], direction: Direction, order: Order) -> Walk {
        walk(bars, 120..480, 100.0, 101.0, Some(0.01), direction, order).unwrap()
    }
    #[test]
    fn long_and_short_gaps_fill_at_open() {
        let long = do_walk(
            &[bar(121, 98., 100., 97.)],
            Direction::Long,
            Order::Conservative,
        );
        let short = do_walk(
            &[bar(121, 102., 103., 100.)],
            Direction::Short,
            Order::Conservative,
        );
        assert_eq!(long.exit_price, 98.);
        assert_eq!(short.exit_price, 102.);
        assert!(long.stopped && short.stopped);
    }
    #[test]
    fn favorable_flag_applies_next_bar_for_both_directions() {
        let long = do_walk(
            &[bar(120, 100., 102., 99.5), bar(121, 100.5, 101., 99.9)],
            Direction::Long,
            Order::Conservative,
        );
        assert_eq!(long.flag_min, Some(120));
        assert_eq!(long.exit_min, 121);
        assert_eq!(long.exit_price, 100.);
        let short = do_walk(
            &[bar(120, 100., 100.5, 98.), bar(121, 99.5, 100.1, 99.)],
            Direction::Short,
            Order::Conservative,
        );
        assert_eq!(short.flag_min, Some(120));
        assert_eq!(short.exit_min, 121);
        assert_eq!(short.exit_price, 100.);
    }
    #[test]
    fn same_bar_order_and_time_exit() {
        let bars = [bar(120, 100., 102., 98.)];
        let a = do_walk(&bars, Direction::Long, Order::Conservative);
        let b = do_walk(&bars, Direction::Long, Order::Optimistic);
        assert_eq!(a.exit_price.to_bits(), b.exit_price.to_bits());
        assert_eq!(a.double_touches, 1);
        assert_eq!(b.double_touches, 1);
        let a = do_walk(
            &[bar(120, 100., 100.1, 99.9)],
            Direction::Long,
            Order::Conservative,
        );
        assert!(!a.stopped);
        assert_eq!(a.exit_min, 480);
        assert_eq!(a.exit_price, 101.);
        let b = walk(
            &bars,
            120..480,
            100.,
            101.,
            None,
            Direction::Long,
            Order::Conservative,
        )
        .unwrap();
        assert!(!b.stopped);
    }
    fn sample() -> (Vec<Day>, Vec<Bar>) {
        let mut days = Vec::new();
        let mut bars = Vec::new();
        for i in 0..130 {
            let day = date(2007, 1, 2)
                .checked_add(jiff::Span::new().days(i))
                .unwrap();
            let x = (i as f64 * 0.41).sin() * 0.002;
            let y = (i as f64 * 0.73).cos() * 0.001;
            let prices = [1.0, 1.0 + x, 1.0 + x + y];
            days.push(Day {
                date: day,
                eu_min: 120,
                us_exit_min: 825,
                prices,
                fallback: [true, false, false],
            });
            for (minute, price) in [
                (119, prices[0]),
                (120, prices[0]),
                (480, prices[1]),
                (825, prices[2]),
            ] {
                bars.push(Bar {
                    timestamp: day.at((minute / 60) as i8, (minute % 60) as i8, 0, 0),
                    open: price,
                    high: price * 1.0001,
                    low: price * 0.9999,
                    close: price,
                });
            }
        }
        (days, bars)
    }
    #[test]
    fn fallback_entry_price_walks_from_boundary() {
        let (days, mut bars) = sample();
        let day = days[60].date;
        bars.retain(|b| {
            !(b.timestamp.date() == day && b.timestamp.hour() == 2 && b.timestamp.minute() == 0)
        });
        let entry = days[60].prices[0];
        let timestamp = day.at(2, 1, 0, 0);
        bars.push(Bar {
            timestamp,
            open: entry * 1.1,
            high: entry * 1.11,
            low: entry,
            close: entry,
        });
        bars.sort_unstable_by_key(|b| b.timestamp);
        let rows = compute(
            &days,
            &bars,
            Mode::Stop,
            false,
            Order::Conservative,
            0.4,
            0.3,
        )
        .unwrap();
        let leg = rows[60].legs[0];
        assert!(leg.entry_fallback && leg.walk.stopped);
        assert_eq!(leg.walk.exit_min, 121);
        assert_eq!(leg.entry_price, entry);
        assert_eq!(leg.walk.exit_price, entry * 1.1);
        assert_eq!(leg.c, 0.2 * 0.0001 * (1.0 / entry + 1.0 / (entry * 1.1)));
    }
    #[test]
    fn prior_history_and_cost_use_fill_prices() {
        let (days, bars) = sample();
        let result = compute(
            &days,
            &bars,
            Mode::Stop,
            false,
            Order::Conservative,
            0.4,
            0.3,
        )
        .unwrap();
        assert!(result[59].legs[0].sigma.is_none());
        assert!(result[59].sigma_hat.is_none());
        let expected = stdev(
            &result[..60]
                .iter()
                .map(|d| d.legs[0].r_nostop)
                .collect::<Vec<_>>(),
        )
        .unwrap();
        assert_eq!(
            result[60].legs[0].sigma.unwrap().to_bits(),
            expected.to_bits()
        );
        assert!(stdev(&[0.1; 59]).is_err());
        assert!(stdev(&[0.1; 60]).is_err());
        let l = result[60].legs[0];
        assert_eq!(
            l.c.to_bits(),
            (0.2 * 0.0001 * (1.0 / l.entry_price + 1.0 / l.walk.exit_price)).to_bits()
        );
        let later = Day {
            date: date(2007, 7, 2),
            eu_min: 120,
            us_exit_min: 825,
            prices: [1., 1.01, 1.02],
            fallback: [false; 3],
        };
        assert!(
            compute(
                &[later],
                &[],
                Mode::Stop,
                false,
                Order::Conservative,
                0.4,
                0.3
            )
            .is_err()
        );
    }
    #[test]
    fn changing_future_bars_does_not_change_earlier_calculations() {
        let mut raw = Vec::new();
        for i in 0..180 {
            let date = date(2007, 1, 2)
                .checked_add(jiff::Span::new().days(i))
                .unwrap();
            for minute in 0..1440 {
                let price = 1.1
                    + 0.001 * (i as f64 * 0.17).sin()
                    + 0.0005 * (minute as f64 * 0.01 + i as f64 * 0.21).sin();
                raw.push(Bar {
                    timestamp: date.at((minute / 60) as i8, (minute % 60) as i8, 0, 0),
                    open: price,
                    high: price + 0.00001,
                    low: price - 0.00001,
                    close: price,
                });
            }
        }
        let calculate = |raw: &[Bar]| {
            let (clean, _) = super::super::input::clean(raw);
            let days = super::super::input::days(&clean).unwrap();
            compute(
                &days,
                &clean,
                Mode::Stop,
                false,
                Order::Conservative,
                0.4,
                0.3,
            )
            .unwrap()
        };
        let original = calculate(&raw);
        let cutoff_index = (90..original.len() - 1)
            .find(|&i| original[i].date.tomorrow().unwrap() == original[i + 1].date)
            .unwrap();
        let cutoff = original[cutoff_index].date;
        let next_date = original[cutoff_index + 1].date;
        let mut changed_next = 0;
        for bar in &mut raw {
            if bar.timestamp.date() > cutoff {
                if bar.timestamp.date() == next_date {
                    changed_next += 1;
                }
                bar.open *= 1.2;
                bar.high *= 1.2;
                bar.low *= 1.2;
                bar.close *= 1.2;
            }
        }
        assert_eq!(changed_next, 1440);
        let changed = calculate(&raw);
        for (a, b) in original[..=cutoff_index]
            .iter()
            .zip(&changed[..=cutoff_index])
        {
            assert_eq!(a.date, b.date);
            for (x, y) in a.legs.iter().zip(b.legs.iter()) {
                assert_eq!(x.r.to_bits(), y.r.to_bits());
                assert_eq!(x.sigma.map(f64::to_bits), y.sigma.map(f64::to_bits));
                assert_eq!(
                    x.walk.initial_stop.map(f64::to_bits),
                    y.walk.initial_stop.map(f64::to_bits)
                );
                assert_eq!(x.walk.exit_price.to_bits(), y.walk.exit_price.to_bits());
            }
            assert_eq!(a.u.to_bits(), b.u.to_bits());
            assert_eq!(a.sigma_hat.map(f64::to_bits), b.sigma_hat.map(f64::to_bits));
            assert_eq!(a.leverage.map(f64::to_bits), b.leverage.map(f64::to_bits));
            assert_eq!(
                a.daily_return.map(f64::to_bits),
                b.daily_return.map(f64::to_bits)
            );
        }
    }
    #[test]
    fn reverse_and_no_stop_are_supported() {
        let (days, bars) = sample();
        let original = compute(
            &days,
            &bars,
            Mode::NoStop,
            false,
            Order::Conservative,
            0.4,
            0.3,
        )
        .unwrap();
        let reversed = compute(
            &days,
            &bars,
            Mode::NoStop,
            true,
            Order::Conservative,
            0.4,
            0.3,
        )
        .unwrap();
        assert!(!original[100].legs[0].walk.stopped);
        assert!(!reversed[100].legs[0].walk.stopped);
        assert_eq!(original[100].legs[0].r, -reversed[100].legs[0].r);
    }
    #[test]
    fn parquet_has_expected_columns_and_values() {
        let (days, bars) = sample();
        let data = compute(
            &days,
            &bars,
            Mode::Stop,
            false,
            Order::Conservative,
            0.4,
            0.3,
        )
        .unwrap();
        let path =
            std::env::temp_dir().join(format!("mikiri-h006-days-{}.parquet", std::process::id()));
        write_days(&path, &data).unwrap();
        let mut reader = ParquetRecordBatchReaderBuilder::try_new(File::open(&path).unwrap())
            .unwrap()
            .build()
            .unwrap();
        let batch = reader.next().unwrap().unwrap();
        assert_eq!(batch.num_rows(), 130);
        let expected = [
            "date",
            "us_dst",
            "eu_entry_min",
            "us_entry_min",
            "us_exit_min_boundary",
            "eu_entry_price",
            "eu_entry_fallback",
            "eu_boundary_exit_price",
            "eu_sigma",
            "eu_stop_initial",
            "eu_flag_min",
            "eu_stopped",
            "eu_exit_min",
            "eu_exit_price",
            "eu_r_nostop",
            "eu_r",
            "eu_a",
            "eu_c",
            "us_entry_price",
            "us_entry_fallback",
            "us_boundary_exit_price",
            "us_sigma",
            "us_stop_initial",
            "us_flag_min",
            "us_stopped",
            "us_exit_min",
            "us_exit_price",
            "us_r_nostop",
            "us_r",
            "us_a",
            "us_c",
            "u",
            "sigma_hat",
            "leverage",
            "R",
        ];
        let names: Vec<_> = batch
            .schema()
            .fields()
            .iter()
            .map(|f| f.name().clone())
            .collect();
        assert_eq!(names, expected);
        let r = batch
            .column_by_name("R")
            .unwrap()
            .as_any()
            .downcast_ref::<Float64Array>()
            .unwrap();
        assert_eq!(r.value(60), data[60].daily_return.unwrap());
        std::fs::remove_file(path).unwrap();
    }
}
