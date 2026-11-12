use super::*;
use serde_json::{Value, json};
use time::{Duration, Weekday};

fn fixture(ticker: usize) -> Vec<Value> {
    let mut day = date(2006, Month::January, 2).unwrap();
    let end = date(2026, Month::November, 10).unwrap();
    let (mut scale, mut price) = (1.0, 100.0);
    let mut rows = Vec::new();
    while day <= end {
        if !matches!(day.weekday(), Weekday::Saturday | Weekday::Sunday)
            && day != date(2008, Month::January, 1).unwrap()
        {
            let i = rows.len();
            let split = if ticker == 1
                && ["2015-05-20", "2020-08-18", "2022-01-13"].contains(&day.to_string().as_str())
            {
                2.0
            } else {
                1.0
            };
            scale *= split;
            let daily = if i % 40 < 30 {
                0.003
            } else if i % 40 < 36 {
                -0.002
            } else {
                0.007
            };
            if i > 0 {
                price *= 1.0 + daily * (ticker as f64 + 1.0);
            }
            let close = price / scale;
            let dividend = if split != 1.0 || i % 63 == 0 {
                0.01
            } else {
                0.0
            };
            rows.push(json!({"date":format!("{day}T00:00:00.000Z"),"close":close,"high":close,"low":close,"open":close,"adjClose":close,"adjHigh":close,"adjLow":close,"adjOpen":close,"volume":1000,"adjVolume":1000,"divCash":dividend,"splitFactor":split}));
        }
        day += Duration::days(1);
    }
    rows
}
fn inputs() -> [Vec<DailyBar>; 2] {
    [0, 1].map(|t| parse_daily_bars(&serde_json::to_vec(&fixture(t)).unwrap()).unwrap())
}

#[test]
fn index_averages_rsi_seed_recursion_and_flat_case() {
    let mut rows = fixture(0);
    rows.truncate(201);
    for row in &mut rows {
        row["close"] = json!(100.0);
        row["divCash"] = json!(0.0);
    }
    let flat = parse_daily_bars(&serde_json::to_vec(&rows).unwrap()).unwrap();
    let history = indicators(&flat).unwrap();
    assert_eq!(history[2].rsi, Some(50.0));
    assert_eq!(history[199].m200, Some(1.0));
    assert_eq!(history[4].m5, Some(1.0));
    assert_eq!(history[198].signal, None);
    assert_eq!(history[199].signal, Some(false));
    for (row, price) in rows.iter_mut().zip([100.0, 102.0, 101.0, 104.0, 52.0]) {
        row["close"] = json!(price);
    }
    rows[4]["splitFactor"] = json!(2.0);
    rows[4]["divCash"] = json!(1.0);
    let bars = parse_daily_bars(&serde_json::to_vec(&rows).unwrap()).unwrap();
    let h = indicators(&bars).unwrap();
    assert!((h[2].rsi.unwrap() - 200.0 / 3.0).abs() < 1e-10);
    assert!((h[3].rsi.unwrap() - 800.0 / 9.0).abs() < 1e-10);
    assert!((h[4].index - 1.05).abs() < 1e-14);
    assert!((h[4].m5.unwrap() - 1.024).abs() < 1e-14);
}

#[test]
fn state_machine_entry_holding_exit_and_no_reentry() {
    assert!(next_signal(false, true, false, 5.0));
    assert!(!next_signal(false, false, false, 5.0));
    assert!(!next_signal(false, true, false, 5.1));
    assert!(next_signal(true, false, false, 99.0));
    assert!(!next_signal(true, true, true, 1.0));
    assert!(next_signal(false, true, true, 1.0));
}

#[test]
fn dev_outputs_ignore_future_prices_and_invalid_future_fields() {
    for ticker in [0, 1] {
        let raw = fixture(ticker);
        let (before, canonical_before) =
            load(&serde_json::to_vec(&raw).unwrap(), Stage::Dev).unwrap();
        let mut changed = raw;
        for row in &mut changed {
            if row["date"].as_str().unwrap() > "2024-10-31T23:59:59Z" {
                row["close"] = json!(-99.0);
                row["splitFactor"] = json!(0.0);
            }
        }
        let (after, canonical_after) =
            load(&serde_json::to_vec(&changed).unwrap(), Stage::Dev).unwrap();
        assert_eq!(before, after);
        assert_eq!(canonical_before, canonical_after);
    }
}

#[test]
fn synthetic_compute_causality_and_holdout_only() {
    let mut bars = inputs();
    let (before, summary) = compute(&bars, Stage::Dev).unwrap();
    assert_eq!(summary["n_cumulative"], 4);
    assert!(summary["main"]["entry_count"].as_u64().unwrap() > 0);
    assert_eq!(
        summary["main"]["tax_by_year"].as_object().unwrap().len(),
        17
    );
    let boundary = date(2018, Month::June, 1).unwrap();
    for input in &mut bars {
        for b in input.iter_mut().filter(|b| b.date > boundary) {
            b.close *= 1.1;
        }
    }
    let (after, _) = compute(&bars, Stage::Dev).unwrap();
    for (a, b) in before
        .iter()
        .zip(after)
        .take_while(|(a, _)| a.date <= boundary)
    {
        assert_eq!((a.signal, a.v, a.u, a.a), (b.signal, b.v, b.u, b.a));
    }
    let (_, h) = compute(&bars, Stage::Holdout).unwrap();
    assert!(h.get("main").is_none());
    assert!(h.get("gate_h").is_some());
    assert!(h.get("psr_main").is_none());
}

#[test]
fn missing_dates_history_and_bad_values_fail_closed() {
    let mut bars = inputs();
    let i = bars[1]
        .iter()
        .position(|b| b.date == date(2010, Month::June, 1).unwrap())
        .unwrap();
    bars[1].remove(i);
    assert!(
        compute(&bars, Stage::Dev)
            .unwrap_err()
            .to_string()
            .contains("missing SSO")
    );
    let mut bars = inputs();
    let signal = bars[0]
        .iter()
        .position(|b| b.date == date(2007, Month::December, 31).unwrap())
        .unwrap();
    bars[0].drain(..signal - 199);
    assert!(
        compute(&bars, Stage::Dev)
            .unwrap_err()
            .to_string()
            .contains("200 SPY")
    );
    for (field, value) in [
        (0, 0.0),
        (0, -1.0),
        (0, f64::NAN),
        (0, f64::INFINITY),
        (1, -1.0),
        (1, f64::NAN),
        (2, 0.0),
        (2, -1.0),
        (2, f64::INFINITY),
    ] {
        let mut bars = inputs();
        let bar = &mut bars[1][600];
        match field {
            0 => bar.close = value,
            1 => bar.div_cash = value,
            _ => bar.split_factor = value,
        }
        assert!(compute(&bars, Stage::Dev).is_err());
    }
}

#[test]
fn profit_loss_carry_expiry_and_dividend_only_tax() {
    let mut taxes = Taxes::default();
    assert_eq!(taxes.close_year(2010, 10.0), 2.0315);
    assert_eq!(taxes.close_year(2011, -10.0), 0.0);
    assert_eq!(taxes.close_year(2012, 4.0), 0.0);
    assert_eq!(taxes.close_year(2014, 7.0), 0.20315);
    assert_eq!(taxes.close_year(2015, -10.0), 0.0);
    assert_eq!(taxes.close_year(2019, 10.0), 2.0315);
    let mut p = Position::buy(1, 100.0, 10.0, 0.0);
    let b = DailyBar {
        date: date(2020, Month::January, 2).unwrap(),
        close: 5.0,
        split_factor: 2.0,
        div_cash: 0.5,
    };
    let dividend = p.distributions(&b);
    assert_eq!(dividend, 5.0);
    assert_eq!(p.shares, 21.0);
    assert_eq!(p.basis, 105.0);
    assert_eq!(taxes.close_year(2020, dividend), 1.01575);
}

#[test]
fn psr_undefined_stops() {
    assert!(psr(&[0.0, 0.0]).is_err());
    assert!(psr(&[1.0, 1.0, 2.0]).is_err());
    let mut bars = inputs();
    for input in &mut bars {
        for b in input {
            b.close = 100.0;
            b.div_cash = 0.0;
            b.split_factor = 1.0;
        }
    }
    assert!(compute(&bars, Stage::Dev).is_err());
}

#[test]
fn holdout_requires_existing_passing_dev_verdict() {
    assert!(require_dev_verdict(None).is_err());
    assert!(require_dev_verdict(Some(Path::new("/tmp/mikiri-no-such-h004-verdict"))).is_err());
}

#[test]
fn passing_verdict_accepts_only_true_dev_gates_and_matching_recompute() {
    let path =
        std::env::temp_dir().join(format!("mikiri-h004-verdict-{}.json", std::process::id()));
    let passing = json!({"stage":"dev","gate_a":true,"gate_b":true,"gate_c":true});
    let envelope = json!({"match":true,"python_summary":passing});
    fs::write(&path, serde_json::to_vec(&envelope).unwrap()).unwrap();
    assert!(require_dev_verdict(Some(&path)).is_ok());
    for invalid in [
        passing.clone(),
        json!({"match":true,"stage":"dev","gate_a":true,"gate_b":true,"gate_c":true}),
        json!({"python_summary":passing}),
        json!({"match":false,"python_summary":passing}),
        json!({"match":1,"python_summary":passing}),
    ] {
        fs::write(&path, serde_json::to_vec(&invalid).unwrap()).unwrap();
        assert!(require_dev_verdict(Some(&path)).is_err());
    }
    for gate in ["gate_a", "gate_b", "gate_c"] {
        for invalid in [json!(false), json!(1), json!(null)] {
            let mut verdict = envelope.clone();
            verdict["python_summary"][gate] = invalid;
            fs::write(&path, serde_json::to_vec(&verdict).unwrap()).unwrap();
            assert!(require_dev_verdict(Some(&path)).is_err());
        }
    }
    let mut wrong_stage = envelope;
    wrong_stage["python_summary"]["stage"] = json!("holdout");
    fs::write(&path, serde_json::to_vec(&wrong_stage).unwrap()).unwrap();
    assert!(require_dev_verdict(Some(&path)).is_err());
    fs::remove_file(path).unwrap();
}

proptest::proptest! {
    #[test]
    fn tax_sale_preserves_basis_and_cash(shares in 1.0..1000.0_f64, close in 1.0..1000.0_f64,basis in 1.0..1000.0_f64,fraction in 0.001..0.9_f64) {
        let mut p = Position{ticker:1,shares,basis};
        let tax = shares*close*fraction*0.9995;
        let (gain,v) = p.pay_tax(tax,close,0.0005).expect("partial tax sale");
        proptest::prop_assert!((p.basis-basis*(1.0-fraction)).abs()<1e-9);
        proptest::prop_assert!((gain-(tax-basis*fraction)).abs()<1e-9);
        proptest::prop_assert!((v-tax-shares*close*(1.0-fraction)).abs()<1e-8);
    }
}

#[test]
fn indicator_export_preserves_warmup_nulls() {
    let bars = inputs();
    let history = indicators(&bars[0]).unwrap();
    let batch = indicator_batch(&history).unwrap();
    for name in ["m200", "m5", "rsi", "signal"] {
        assert!(batch.schema().field_with_name(name).unwrap().is_nullable());
        assert!(batch.column_by_name(name).unwrap().null_count() > 0);
    }
    assert!(
        !batch
            .schema()
            .field_with_name("index")
            .unwrap()
            .is_nullable()
    );
}

#[test]
fn finite_prices_with_overflowing_rsi_stop() {
    let mut rows = fixture(0);
    rows.truncate(3);
    for (row, price) in rows.iter_mut().zip([1.0, 1e307, 1e307]) {
        row["close"] = json!(price);
        row["divCash"] = json!(0.0);
    }
    let bars = parse_daily_bars(&serde_json::to_vec(&rows).unwrap()).unwrap();
    assert!(
        indicators(&bars)
            .unwrap_err()
            .to_string()
            .contains("non-finite signal indicator")
    );
}

#[test]
fn flat_and_near_boundary_average_comparisons_are_stable() {
    assert!(!above_average(1.1, &[1.1; 200]));
    let mut window = [1.1; 200];
    window[0] = f64::from_bits(1.1_f64.to_bits() - 1);
    assert!(above_average(1.1, &window));
    window[0] = f64::from_bits(1.1_f64.to_bits() + 1);
    assert!(!above_average(1.1, &window));
    let mut rows = fixture(0);
    rows.truncate(600);
    for (i, row) in rows.iter_mut().enumerate() {
        row["close"] = json!(if i == 0 { 1.0 } else { 1.1 });
        row["divCash"] = json!(0.0);
    }
    let bars = parse_daily_bars(&serde_json::to_vec(&rows).unwrap()).unwrap();
    assert!(
        indicators(&bars).unwrap()[199..]
            .iter()
            .all(|row| row.signal == Some(false))
    );
}

#[test]
fn indicator_path_exits_before_considering_reentry() {
    let mut rows = fixture(0);
    rows.truncate(257);
    for (i, row) in rows.iter_mut().enumerate() {
        row["close"] = json!(match i {
            0..=199 => 100.0,
            200..=249 => 200.0,
            250..=254 => 150.0,
            255 => 150.01,
            _ => 150.011,
        });
        row["divCash"] = json!(0.0);
    }
    let bars = parse_daily_bars(&serde_json::to_vec(&rows).unwrap()).unwrap();
    let history = indicators(&bars).unwrap();
    assert_eq!(history[254].signal, Some(true));
    for i in [255, 256] {
        let row = &history[i];
        assert!(row.index > row.m5.unwrap() && row.index > row.m200.unwrap());
        assert!(row.rsi.unwrap() <= 5.0);
    }
    assert_eq!(history[255].signal, Some(false));
    assert_eq!(history[256].signal, Some(true));
}
