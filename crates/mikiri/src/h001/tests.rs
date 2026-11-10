use super::*;
use crate::tiingo::parse_daily_bars;
use arrow_array::Float64Array;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use proptest::prelude::*;
use std::fs::File;
use time::{Month, Weekday};

fn date(year: i32, month: u8, day: u8) -> Date {
    Date::from_calendar_date(year, Month::try_from(month).expect("month"), day).expect("date")
}

fn synthetic_bars(start: Date, end: Date) -> Vec<DailyBar> {
    let mut day = start;
    let mut bars = Vec::new();
    while day <= end {
        if !matches!(day.weekday(), Weekday::Saturday | Weekday::Sunday) && day != date(2026, 9, 7)
        {
            bars.push(DailyBar {
                date: day,
                close: 100.0,
                div_cash: f64::from(day.ordinal()) / 100.0,
                split_factor: 1.0,
            });
        }
        day = day.next_day().expect("next day");
    }
    bars
}

fn calendar_bars() -> Vec<DailyBar> {
    synthetic_bars(date(2026, 8, 31), date(2026, 11, 3))
}

#[test]
fn october_calendar_matches_hand_calculation() {
    let bars = calendar_bars();
    let computed = cycles(&bars, ROUND_TRIP_COST).expect("cycles");
    assert_eq!(computed.len(), 1);
    let october = &computed[0];
    assert_eq!(october.plus1, date(2026, 10, 1));
    assert_eq!(october.period, Period::Main);
    assert!(october.recent);
    assert_eq!(october.in_days, 4);
    assert_eq!(october.out_days, 18);
    let inside = [
        date(2026, 9, 30),
        date(2026, 10, 1),
        date(2026, 10, 2),
        date(2026, 10, 5),
    ];
    let expected_in = inside
        .iter()
        .map(|d| f64::from(d.ordinal()) / 10_000.0)
        .sum::<f64>()
        / 4.0;
    let outside: Vec<_> = bars
        .iter()
        .filter(|b| {
            b.date >= date(2026, 9, 30) && b.date <= date(2026, 10, 29) && !inside.contains(&b.date)
        })
        .collect();
    assert_eq!(outside.len(), 18);
    let expected_out = outside.iter().map(|b| b.div_cash / b.close).sum::<f64>() / 18.0;
    assert!((october.in_mean - expected_in).abs() < 1e-15);
    assert!((october.out_mean - expected_out).abs() < 1e-15);
    assert!((october.d - (expected_in - ROUND_TRIP_COST / 4.0 - expected_out)).abs() < 1e-15);
    assert!(computed.iter().all(|c| c.plus1 != date(2026, 11, 2)));
}

#[test]
fn future_prices_do_not_change_completed_cycles() {
    let mut bars = synthetic_bars(date(2026, 1, 1), date(2026, 11, 3));
    let before = cycles(&bars, ROUND_TRIP_COST).expect("before");
    let cutoff = date(2026, 8, 15);
    for bar in bars.iter_mut().filter(|b| b.date > cutoff) {
        bar.close *= 1.7 + f64::from(bar.date.day()) / 100.0;
    }
    let after = cycles(&bars, ROUND_TRIP_COST).expect("after");
    let mut checked = 0;
    for (old, new) in before
        .iter()
        .zip(&after)
        .filter(|(old, _)| old.plus1 < date(2026, 8, 1))
    {
        assert_eq!(old.plus1, new.plus1);
        assert_eq!(old.d.to_bits(), new.d.to_bits());
        checked += 1;
    }
    assert!(checked >= 5);
}

proptest! {
    #[test]
    fn round_trip_cost_is_spread_over_four_days(prices in prop::collection::vec(50.0_f64..150.0, 64)) {
        let mut bars = calendar_bars();
        for (bar, price) in bars.iter_mut().zip(prices.iter().cycle()) {
            bar.close = *price;
        }
        let free = cycles(&bars, 0.0).expect("zero cost");
        let base = cycles(&bars, ROUND_TRIP_COST).expect("base cost");
        let doubled = cycles(&bars, 2.0 * ROUND_TRIP_COST).expect("double cost");
        prop_assert!(!free.is_empty());
        for ((zero, one), two) in free.iter().zip(&base).zip(&doubled) {
            prop_assert!((zero.d - one.d - 0.0001).abs() < 1e-15);
            prop_assert!((one.d - two.d - 0.0001).abs() < 1e-15);
            prop_assert_eq!(zero.out_mean.to_bits(), two.out_mean.to_bits());
        }
    }
}

#[test]
fn psr_matches_fixed_preregistered_value() {
    let values: [f64; 8] = [0.001, -0.002, 0.003, 0.0005, -0.001, 0.002, 0.0015, -0.0005];
    let mean = values.iter().sum::<f64>() / values.len() as f64;
    let moment =
        |power| values.iter().map(|d| (d - mean).powi(power)).sum::<f64>() / values.len() as f64;
    let m2 = moment(2);
    for (actual, expected) in [
        (mean / m2.sqrt(), 0.36291502734548037),
        (moment(3) / m2.powf(1.5), -0.11684093563317921),
        (moment(4) / m2.powi(2), 1.9651715248859807),
        (psr(&values).expect("psr"), 0.8228899111257035),
    ] {
        assert!(
            (actual - expected).abs() / expected.abs() < 1e-9,
            "{actual} != {expected}"
        );
    }
}

#[test]
fn adjusted_close_is_ignored() {
    let bars = parse_daily_bars(
        br#"[
        {"date":"2026-10-01T00:00:00Z","close":100,"adjClose":800,"divCash":0,"splitFactor":1},
        {"date":"2026-10-02T00:00:00Z","close":110,"adjClose":2,"divCash":0,"splitFactor":1}
    ]"#,
    )
    .expect("parse");
    let returns = daily_returns(&bars).expect("returns");
    assert_eq!(returns.len(), 1);
    assert!((returns[0] - 0.1).abs() < 1e-15);
}

#[test]
fn coverage_rejects_late_start() {
    let bars = synthetic_bars(date(2005, 12, 30), date(2026, 11, 3));
    assert!(validate_coverage(&bars).is_err());
}

#[test]
fn coverage_rejects_early_end() {
    let bars = synthetic_bars(date(2005, 12, 1), date(2026, 10, 29));
    assert!(validate_coverage(&bars).is_err());
}

#[test]
fn coverage_rejects_six_day_gap() {
    let mut bars = synthetic_bars(date(2005, 12, 1), date(2026, 11, 3));
    bars.retain(|b| b.date <= date(2020, 6, 2) || b.date >= date(2020, 6, 8));
    assert!(validate_coverage(&bars).is_err());
}

#[test]
fn split_and_dividend_use_same_day_unadjusted_close() {
    let bars = [
        DailyBar {
            date: date(2026, 10, 1),
            close: 100.0,
            div_cash: 0.0,
            split_factor: 1.0,
        },
        DailyBar {
            date: date(2026, 10, 2),
            close: 52.0,
            div_cash: 1.5,
            split_factor: 2.0,
        },
    ];
    let returns = daily_returns(&bars).expect("returns");
    assert_eq!(
        returns[0].to_bits(),
        ((52.0_f64 * 2.0 + 1.5) / 100.0 - 1.0).to_bits()
    );
}

#[test]
fn cycles_parquet_roundtrip_preserves_rows_and_d() {
    let computed = cycles(&calendar_bars(), ROUND_TRIP_COST).expect("cycles");
    let path = std::env::temp_dir().join(format!(
        "mikiri-h001-cycle-roundtrip-{}.parquet",
        std::process::id()
    ));
    write_cycles(&computed, &path).expect("write parquet");
    let reader = ParquetRecordBatchReaderBuilder::try_new(File::open(&path).expect("open parquet"))
        .expect("reader")
        .build()
        .expect("build reader");
    let mut actual = Vec::new();
    for batch in reader {
        let batch = batch.expect("batch");
        let values = batch
            .column_by_name("d")
            .expect("d column")
            .as_any()
            .downcast_ref::<Float64Array>()
            .expect("float d");
        actual.extend(values.values().iter().map(|d| d.to_bits()));
    }
    assert_eq!(
        actual,
        computed.iter().map(|c| c.d.to_bits()).collect::<Vec<_>>()
    );
    std::fs::remove_file(path).expect("cleanup parquet");
}

#[test]
fn in_mean_is_pre_cost_and_d_unchanged_across_costs() {
    let bars = calendar_bars();
    let free = cycles(&bars, 0.0).expect("zero cost");
    let base = cycles(&bars, ROUND_TRIP_COST).expect("base cost");
    assert_eq!(free.len(), base.len());
    for (zero, one) in free.iter().zip(&base) {
        assert_eq!(zero.plus1, one.plus1);
        assert_eq!(zero.in_mean.to_bits(), one.in_mean.to_bits());
        assert!((one.d - (zero.d - ROUND_TRIP_COST / 4.0)).abs() < 1e-15);
    }
}

#[test]
fn write_outputs_failure_removes_temp_and_leaves_no_out_dir() {
    use std::{path::PathBuf, sync::atomic::Ordering};
    struct ChdirBack(std::path::PathBuf);
    impl Drop for ChdirBack {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.0);
        }
    }
    let root = std::env::temp_dir().join(format!("mikiri-h001-write-fail-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("workspace");
    let out = PathBuf::from("results/atomic-fail");
    let cwd = std::env::current_dir().expect("cwd");
    std::env::set_current_dir(&root).expect("chdir");
    let _chdir = ChdirBack(cwd);
    let bars = synthetic_bars(date(2005, 11, 1), date(2026, 11, 3));
    let (cycles, summary) = compute(&bars).expect("compute");
    TEST_WRITE_FAIL_AFTER.store(1, Ordering::Relaxed);
    let err = super::write_outputs(&bars, &cycles, &summary, &out).expect_err("write");
    TEST_WRITE_FAIL_AFTER.store(usize::MAX, Ordering::Relaxed);
    assert_eq!(err.to_string(), "test-induced write failure");
    assert!(!root.join(&out).exists());
    let tmp = root.join(format!(".atomic-fail.tmp-{}", std::process::id()));
    assert!(!tmp.exists());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn full_synthetic_coverage_has_250_main_and_120_recent_cycles() {
    let bars = synthetic_bars(date(2005, 11, 1), date(2026, 11, 3));
    validate_coverage(&bars).expect("coverage");
    let (computed, summary) = compute(&bars).expect("compute");
    assert_eq!(summary.main_n, 250);
    assert_eq!(summary.recent_n, 120);
    let main: Vec<_> = computed
        .iter()
        .filter(|c| c.period == Period::Main)
        .collect();
    assert_eq!(main.first().expect("first").plus1, date(2006, 1, 2));
    assert_eq!(main.last().expect("last").plus1, date(2026, 10, 1));
    assert_eq!(summary.gate_a, summary.mean_d_main > 0.0);
    assert_eq!(summary.gate_b, summary.psr_main >= 0.95);
    assert_eq!(summary.gate_c, summary.mean_d_recent > 0.0);
    assert!((summary.mean_d_main - summary.mean_d_main_cost2x - 0.0001).abs() < 1e-15);
}
