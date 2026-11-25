use super::{
    daily::{Daily, WEIGHTS},
    input::Stage,
};
use crate::{
    h001,
    tiingo::{self, DailyBar},
};
use arrow_array::{ArrayRef, Float64Array, RecordBatch, StringArray};
use arrow_schema::{Field, Schema};
use jiff::civil::{Date, date};
use parquet::arrow::ArrowWriter;
use serde::Serialize;
use std::{collections::BTreeMap, error::Error, fs, path::Path, sync::Arc};

type Result<T> = std::result::Result<T, Box<dyn Error>>;
const TAX_RATE: f64 = 0.20315;
const PSR_THRESHOLD: f64 = 1.0 - 0.05 / 6.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Segment {
    Dev,
    Recent,
    Holdout,
}
impl Segment {
    pub fn name(self) -> &'static str {
        match self {
            Self::Dev => "dev",
            Self::Recent => "recent",
            Self::Holdout => "holdout",
        }
    }
    pub(crate) fn bounds(self) -> (Date, Date) {
        match self {
            Self::Dev => (date(2007, 7, 2), date(2024, 10, 31)),
            Self::Recent => (date(2019, 11, 1), date(2024, 10, 31)),
            Self::Holdout => (date(2024, 11, 1), date(2026, 10, 30)),
        }
    }
}
#[derive(Clone, Debug)]
pub struct AccountDay {
    pub segment: Segment,
    pub date: Date,
    pub equity_start: f64,
    pub daily_return: f64,
    pub tax_paid: f64,
    pub equity_end: f64,
    pub equity_pretax: f64,
}
#[derive(Debug, Serialize)]
pub struct SegmentSummary {
    pub segment: Segment,
    pub w: f64,
    pub b: f64,
    pub sr: f64,
    pub cagr_pretax: f64,
    pub cagr_aftertax: f64,
    pub drawdown_pretax: f64,
    pub drawdown_aftertax: f64,
    pub max_leverage: f64,
    pub max_leg_notional_to_equity: f64,
}
#[derive(Debug, Serialize)]
pub struct Gates {
    pub a: bool,
    pub b: bool,
    pub c: bool,
    pub h: Option<bool>,
}
pub struct Computed {
    pub accounts: Vec<AccountDay>,
    pub segments: Vec<SegmentSummary>,
    pub psr: f64,
    pub gates: Gates,
}

pub fn read_spy(path: &Path, stage: Stage) -> Result<Vec<DailyBar>> {
    let data = fs::read(path)?;
    let mut json: serde_json::Value = serde_json::from_slice(&data)?;
    let rows = json.as_array_mut().ok_or("SPY input must be an array")?;
    let cutoff = if stage == Stage::Dev {
        "2024-10-31"
    } else {
        "2026-10-30"
    };
    for row in rows.iter() {
        if row.get("date").and_then(|v| v.as_str()).is_none() {
            return Err("missing SPY date".into());
        }
    }
    rows.retain(|row| {
        row["date"]
            .as_str()
            .is_none_or(|d| d.get(..10).is_none_or(|day| day <= cutoff))
    });
    Ok(tiingo::parse_daily_bars(&serde_json::to_vec(&json)?)?)
}

#[derive(Default)]
struct Taxes {
    losses: BTreeMap<i32, f64>,
}
impl Taxes {
    fn settle(&mut self, year: i32, income: f64, rate: f64) -> Result<f64> {
        if !income.is_finite() || !rate.is_finite() || !(0.0..=1.0).contains(&rate) {
            return Err("invalid income or tax rate".into());
        }
        self.losses.retain(|y, _| *y + 3 >= year);
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
        Ok(taxable * rate)
    }
}

pub fn baseline(spy: &[DailyBar], start: Date, end: Date) -> Result<f64> {
    let start = start.to_string();
    let end = end.to_string();
    let rows: Vec<_> = spy
        .iter()
        .filter(|b| {
            let d = b.date.to_string();
            d >= start && d <= end
        })
        .collect();
    let first = rows.first().ok_or("no SPY rows in segment")?;
    let mut shares = 1.0 / 1.0002 / first.close;
    let mut index = 1.0 / 1.0002;
    for pair in rows.windows(2) {
        let prev = pair[0];
        let current = pair[1];
        shares = shares * current.split_factor + shares * current.div_cash / current.close;
        index *= (current.close * current.split_factor + current.div_cash) / prev.close;
        let value = shares * current.close;
        if !value.is_finite()
            || value <= 0.0
            || (value - index).abs() > 1e-10 * value.abs().max(1.0)
        {
            return Err("SPY total-return reconciliation failed".into());
        }
    }
    let last = rows.last().ok_or("no SPY rows in segment")?;
    let value = shares * last.close * 0.9998;
    if !value.is_finite() || value <= 0.0 {
        return Err("invalid SPY baseline".into());
    }
    Ok(value)
}

pub fn sharpe(values: &[f64]) -> Result<f64> {
    if values.len() < 2 || values.iter().any(|v| !v.is_finite()) {
        return Err("SR needs at least two finite returns".into());
    }
    let mean = values[0] + values.iter().map(|v| v - values[0]).sum::<f64>() / values.len() as f64;
    let sd =
        (values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / (values.len() - 1) as f64).sqrt();
    if !sd.is_finite() || sd <= 0.0 {
        return Err("invalid SR variance".into());
    }
    let value = 252.0_f64.sqrt() * mean / sd;
    if !value.is_finite() {
        return Err("nonfinite SR".into());
    }
    Ok(value)
}

pub fn account(segment: Segment, days: &[Daily], rate: f64) -> Result<Vec<AccountDay>> {
    let (rows, ruined) = account_report(segment, days, rate)?;
    if ruined.is_some() {
        return Err("invalid or ruined equity".into());
    }
    Ok(rows)
}

pub(crate) fn account_report(
    segment: Segment,
    days: &[Daily],
    rate: f64,
) -> Result<(Vec<AccountDay>, Option<Date>)> {
    if !rate.is_finite() || !(0.0..=1.0).contains(&rate) {
        return Err("invalid tax rate".into());
    }
    if days.is_empty() {
        return Err("empty segment".into());
    }
    let mut tax = Taxes::default();
    let mut equity = 1.0;
    let mut pretax = 1.0;
    let mut income = 0.0;
    let mut rows = Vec::with_capacity(days.len());
    for (i, day) in days.iter().enumerate() {
        if i > 0 && days[i - 1].date >= day.date {
            return Err("segment days must increase".into());
        }
        let r = day
            .daily_return
            .ok_or("missing daily return or preparation history")?;
        if day.leverage.is_none() || !r.is_finite() {
            return Err("missing leverage or invalid return".into());
        }
        let start = equity;
        let pnl = start * r;
        income += pnl;
        equity += pnl;
        pretax *= 1.0 + r;
        if !income.is_finite() || !equity.is_finite() || !pretax.is_finite() {
            return Err("nonfinite equity".into());
        }
        if equity <= 0.0 || pretax <= 0.0 {
            return Ok((rows, Some(day.date)));
        }
        let year_end = i + 1 == days.len() || days[i + 1].date.year() != day.date.year();
        let paid = if year_end {
            let paid = tax.settle(i32::from(day.date.year()), income, rate)?;
            income = 0.0;
            paid
        } else {
            0.0
        };
        equity -= paid;
        if !equity.is_finite() {
            return Err("nonfinite equity".into());
        }
        if equity <= 0.0 {
            return Ok((rows, Some(day.date)));
        }
        rows.push(AccountDay {
            segment,
            date: day.date,
            equity_start: start,
            daily_return: r,
            tax_paid: paid,
            equity_end: equity,
            equity_pretax: pretax,
        });
    }
    Ok((rows, None))
}

pub(crate) fn drawdown(values: impl Iterator<Item = f64>) -> f64 {
    let mut peak: f64 = 1.0;
    let mut worst: f64 = 0.0;
    for value in values {
        peak = peak.max(value);
        worst = worst.max(1.0 - value / peak);
    }
    worst
}
fn summarize(
    segment: Segment,
    days: &[Daily],
    rows: &[AccountDay],
    spy: &[DailyBar],
) -> Result<SegmentSummary> {
    let first = days.first().ok_or("empty segment")?;
    let last = days.last().ok_or("empty segment")?;
    let span = last.date.duration_since(first.date).as_secs() as f64 / 86400.0;
    if span <= 0.0 {
        return Err("segment needs positive calendar span".into());
    }
    let end = rows.last().ok_or("empty accounts")?;
    let max_leverage = days.iter().filter_map(|d| d.leverage).fold(0.0, f64::max);
    let cagr_pretax = end.equity_pretax.powf(365.0 / span) - 1.0;
    let cagr_aftertax = end.equity_end.powf(365.0 / span) - 1.0;
    if !cagr_pretax.is_finite() || !cagr_aftertax.is_finite() {
        return Err("nonfinite CAGR".into());
    }
    Ok(SegmentSummary {
        segment,
        w: end.equity_end,
        b: baseline(spy, segment.bounds().0, segment.bounds().1)?,
        sr: sharpe(&rows.iter().map(|r| r.daily_return).collect::<Vec<_>>())?,
        cagr_pretax,
        cagr_aftertax,
        drawdown_pretax: drawdown(rows.iter().map(|r| r.equity_pretax)),
        drawdown_aftertax: drawdown(rows.iter().map(|r| r.equity_end)),
        max_leverage,
        max_leg_notional_to_equity: max_leverage * WEIGHTS[0],
    })
}

pub fn compute(days: &[Daily], spy: &[DailyBar], stage: Stage) -> Result<Computed> {
    let mut summaries = Vec::new();
    let mut accounts = Vec::new();
    let mut psr = None;
    let segments = if stage == Stage::Dev {
        vec![Segment::Dev, Segment::Recent]
    } else {
        vec![Segment::Dev, Segment::Recent, Segment::Holdout]
    };
    for segment in segments {
        let (start, end) = segment.bounds();
        let first = days.partition_point(|d| d.date < start);
        let last = days.partition_point(|d| d.date <= end);
        let slice = &days[first..last];
        let rows = account(segment, slice, TAX_RATE)?;
        if segment == Segment::Dev {
            psr = Some(h001::psr(
                &rows.iter().map(|d| d.daily_return).collect::<Vec<_>>(),
            )?);
        }
        summaries.push(summarize(segment, slice, &rows, spy)?);
        accounts.extend(rows);
    }
    let psr = psr.ok_or("missing dev PSR")?;
    let dev = &summaries[0];
    let recent = &summaries[1];
    let gates = Gates {
        a: dev.w > dev.b,
        b: psr >= PSR_THRESHOLD,
        c: recent.w > recent.b,
        h: summaries.get(2).map(|s| s.w > s.b),
    };
    Ok(Computed {
        accounts,
        segments: summaries,
        psr,
        gates,
    })
}

pub fn write_accounts(path: &Path, rows: &[AccountDay]) -> Result<()> {
    let mut fields = Vec::new();
    let mut arrays: Vec<ArrayRef> = Vec::new();
    macro_rules! col {
        ($name:expr,$array:expr) => {{
            let a: ArrayRef = Arc::new($array);
            fields.push(Field::new($name, a.data_type().clone(), false));
            arrays.push(a);
        }};
    }
    col!(
        "segment",
        StringArray::from(rows.iter().map(|d| d.segment.name()).collect::<Vec<_>>())
    );
    col!(
        "date",
        StringArray::from(rows.iter().map(|d| d.date.to_string()).collect::<Vec<_>>())
    );
    col!(
        "equity_start",
        Float64Array::from(rows.iter().map(|d| d.equity_start).collect::<Vec<_>>())
    );
    col!(
        "R",
        Float64Array::from(rows.iter().map(|d| d.daily_return).collect::<Vec<_>>())
    );
    col!(
        "tax_paid",
        Float64Array::from(rows.iter().map(|d| d.tax_paid).collect::<Vec<_>>())
    );
    col!(
        "equity_end",
        Float64Array::from(rows.iter().map(|d| d.equity_end).collect::<Vec<_>>())
    );
    col!(
        "equity_pretax",
        Float64Array::from(rows.iter().map(|d| d.equity_pretax).collect::<Vec<_>>())
    );
    let batch = RecordBatch::try_new(Arc::new(Schema::new(fields)), arrays)?;
    let mut writer = ArrowWriter::try_new(fs::File::create(path)?, batch.schema(), None)?;
    writer.write(&batch)?;
    writer.close()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::h006::daily::{Leg, Walk};
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

    fn day(date: Date, r: f64) -> Daily {
        let leg = Leg {
            entry_price: 1.0,
            entry_fallback: false,
            boundary_exit_price: 1.0,
            sigma: Some(0.01),
            walk: Walk {
                initial_stop: Some(0.99),
                flag_min: None,
                stopped: false,
                exit_min: 480,
                exit_price: 1.0,
                double_touches: 0,
            },
            r_nostop: 0.0,
            r: 0.0,
            a: 0.0,
            c: 0.0,
        };
        Daily {
            date,
            us_dst: false,
            eu_entry_min: 120,
            us_exit_min_boundary: 825,
            legs: [leg; 2],
            u: 0.0,
            sigma_hat: Some(0.1),
            leverage: Some(2.0),
            daily_return: Some(r),
        }
    }
    fn near(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-12, "{a} != {b}");
    }
    // Values computed once with empyrical-reloaded 0.5.0 (Apache-2.0); the library is not a dependency.
    #[test]
    fn empyrical_sharpe_vectors() {
        near(
            sharpe(&[
                0.01, 0.02, 0.015, 0.005, 0.03, 0.012, 0.018, 0.007, 0.025, 0.011,
            ])
            .unwrap(),
            30.463302430398368,
        );
        near(
            sharpe(&[
                0.02, -0.01, 0.015, -0.025, 0.03, -0.005, 0.01, -0.012, 0.018, 0.004,
            ])
            .unwrap(),
            4.157948718948698,
        );
        near(
            sharpe(&[0.05, 0.02, -0.20, -0.10, 0.08, 0.12, 0.15, 0.04, 0.03, 0.02]).unwrap(),
            3.2500404150309685,
        );
        near(
            sharpe(&[
                0.01, 0.0, -0.01, 0.02, 0.0, -0.005, 0.015, -0.01, 0.005, 0.01,
            ])
            .unwrap(),
            5.400787344175649,
        );
    }
    fn spy() -> Vec<DailyBar> {
        tiingo::parse_daily_bars(br#"[{"date":"2007-07-02T00:00:00Z","close":100,"divCash":0,"splitFactor":1},{"date":"2008-01-02T00:00:00Z","close":50,"divCash":1,"splitFactor":2},{"date":"2019-11-01T00:00:00Z","close":50,"divCash":0,"splitFactor":1},{"date":"2020-11-01T00:00:00Z","close":50,"divCash":0,"splitFactor":1},{"date":"2024-11-01T00:00:00Z","close":50,"divCash":0,"splitFactor":1},{"date":"2025-11-01T00:00:00Z","close":50,"divCash":0,"splitFactor":1}]"#).unwrap()
    }
    #[test]
    fn profit_loss_and_final_partial_year() {
        let rows = account(
            Segment::Dev,
            &[
                day(date(2019, 12, 30), 0.10),
                day(date(2020, 2, 1), -0.1),
                day(date(2021, 2, 1), 0.2),
            ],
            0.20315,
        )
        .unwrap();
        near(rows[0].tax_paid, 0.1 * 0.20315);
        assert_eq!(rows[1].tax_paid, 0.0);
        let expected_income = rows[2].equity_start * 0.2;
        let old_loss = -rows[1].equity_start * rows[1].daily_return;
        near(rows[2].tax_paid, (expected_income - old_loss) * 0.20315);
        near(
            rows[2].equity_end,
            rows[2].equity_start * (1.0 + rows[2].daily_return) - rows[2].tax_paid,
        );
        near(rows[2].equity_pretax, 1.1 * 0.9 * 1.2);
    }
    #[test]
    fn carry_uses_oldest_first_and_expires_after_three_years() {
        let mut t = Taxes::default();
        near(t.settle(2020, -5.0, 0.2).unwrap(), 0.0);
        near(t.settle(2021, -7.0, 0.2).unwrap(), 0.0);
        near(t.settle(2022, 6.0, 0.2).unwrap(), 0.0);
        near(t.losses[&2020], 0.0);
        near(t.losses[&2021], 6.0);
        near(t.settle(2025, 10.0, 0.2).unwrap(), 2.0);
        let mut t = Taxes::default();
        t.settle(2020, -3.0, 0.2).unwrap();
        near(t.settle(2023, 2.0, 0.2).unwrap(), 0.0);
        near(t.settle(2024, 2.0, 0.2).unwrap(), 0.4);
    }
    #[test]
    fn segment_restarts_equity_and_loss_carry() {
        let rows = [
            day(date(2019, 10, 31), -0.2),
            day(date(2019, 11, 1), 0.1),
            day(date(2020, 1, 2), 0.1),
        ];
        let all = account(Segment::Dev, &rows, 0.2).unwrap();
        let fresh = account(Segment::Recent, &rows[1..], 0.2).unwrap();
        assert_eq!(fresh[0].equity_start, 1.0);
        near(fresh[0].tax_paid, 0.02);
        assert_eq!(all[1].tax_paid, 0.0);
    }
    #[test]
    fn equity_failure_and_initial_peak_drawdown() {
        assert!(account(Segment::Dev, &[day(date(2020, 1, 1), -1.0)], 0.2).is_err());
        assert!(account(Segment::Dev, &[day(date(2020, 1, 1), f64::NAN)], 0.2).is_err());
        let mut missing = day(date(2020, 1, 1), 0.01);
        missing.daily_return = None;
        assert!(account(Segment::Dev, &[missing], 0.2).is_err());
        near(drawdown([0.9, 0.95, 0.8].into_iter()), 0.2);
    }
    #[test]
    fn baseline_dividend_split_and_boundaries() {
        let s = spy();
        let b = baseline(&s, date(2007, 7, 2), date(2008, 1, 2)).unwrap();
        near(b, (101.0 / 100.0) / 1.0002 * 0.9998);
        assert!(baseline(&s, date(2017, 1, 1), date(2018, 1, 1)).is_err());
    }
    #[test]
    fn psr_errors_and_sharpe_validation() {
        assert!(h001::psr(&[]).is_err());
        assert!(h001::psr(&[0.1]).is_err());
        assert!(h001::psr(&[0.0, 0.0]).is_err());
        assert!(h001::psr(&[0.1, f64::NAN]).is_err());
        assert!(sharpe(&[0.1]).is_err());
        assert!(sharpe(&[0.1, 0.1]).is_err());
        assert!(sharpe(&[0.1, f64::NAN]).is_err());
    }
    #[test]
    fn gates_and_segment_ranges() {
        let days = [
            day(date(2007, 7, 2), 0.02),
            day(date(2008, 1, 2), 0.01),
            day(date(2019, 11, 1), 0.02),
            day(date(2020, 11, 1), 0.01),
            day(date(2024, 11, 1), -0.01),
            day(date(2025, 11, 1), -0.02),
        ];
        let dev = compute(&days, &spy(), Stage::Dev).unwrap();
        assert!(dev.gates.a);
        assert!(dev.gates.c);
        assert_eq!(dev.gates.b, dev.psr >= 1.0 - 0.05 / 6.0);
        assert_eq!(dev.gates.h, None);
        assert_eq!(
            dev.accounts
                .iter()
                .filter(|d| d.segment == Segment::Dev)
                .count(),
            4
        );
        assert_eq!(
            dev.accounts
                .iter()
                .filter(|d| d.segment == Segment::Recent)
                .count(),
            2
        );
        let hold = compute(&days, &spy(), Stage::Holdout).unwrap();
        assert_eq!(hold.gates.h, Some(false));
        assert_eq!(
            hold.accounts
                .iter()
                .find(|d| d.segment == Segment::Holdout)
                .unwrap()
                .equity_start,
            1.0
        );
        let mut missing = days.clone();
        missing[2].leverage = None;
        assert!(compute(&missing, &spy(), Stage::Dev).is_err());
        let mut failing = days;
        failing[0].daily_return = Some(-0.2);
        failing[2].daily_return = Some(-0.2);
        failing[3].daily_return = Some(-0.1);
        let result = compute(&failing, &spy(), Stage::Dev).unwrap();
        assert!(!result.gates.a);
        assert!(!result.gates.c);
    }
    #[test]
    fn spy_dev_cutoff_does_not_validate_future_fields() {
        let path = std::env::temp_dir().join(format!("h006-spy-{}.json", std::process::id()));
        fs::write(&path,br#"[{"date":"2024-10-31T00:00:00Z","close":100,"divCash":0,"splitFactor":1},{"date":"2024-11-01T00:00:00Z","close":"bad"}]"#).unwrap();
        assert_eq!(read_spy(&path, Stage::Dev).unwrap().len(), 1);
        assert!(read_spy(&path, Stage::Holdout).is_err());
        fs::remove_file(path).unwrap();
    }
    #[test]
    fn accounts_parquet_columns_and_values() {
        let rows = account(
            Segment::Dev,
            &[day(date(2020, 1, 1), 0.1), day(date(2020, 1, 2), -0.02)],
            0.2,
        )
        .unwrap();
        let path =
            std::env::temp_dir().join(format!("h006-accounts-{}.parquet", std::process::id()));
        write_accounts(&path, &rows).unwrap();
        let mut reader = ParquetRecordBatchReaderBuilder::try_new(fs::File::open(&path).unwrap())
            .unwrap()
            .build()
            .unwrap();
        let batch = reader.next().unwrap().unwrap();
        assert_eq!(batch.num_rows(), 2);
        let columns: Vec<_> = batch
            .schema()
            .fields()
            .iter()
            .map(|f| f.name().clone())
            .collect();
        assert_eq!(
            columns,
            [
                "segment",
                "date",
                "equity_start",
                "R",
                "tax_paid",
                "equity_end",
                "equity_pretax"
            ]
        );
        let equity = batch
            .column_by_name("equity_end")
            .unwrap()
            .as_any()
            .downcast_ref::<Float64Array>()
            .unwrap();
        near(equity.value(1), rows[1].equity_end);
        fs::remove_file(path).unwrap();
    }
}
