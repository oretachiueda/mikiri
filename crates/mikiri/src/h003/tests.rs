use super::*;
#[path = "../../tests/common/h003.rs"]
mod common;

fn inputs() -> [Vec<DailyBar>; 3] {
    [0, 1, 2].map(|t| parse_daily_bars(&serde_json::to_vec(&common::fixture(t)).unwrap()).unwrap())
}

#[test]
fn flat_signal_index_is_not_above_its_average() {
    for price in [30.0, 70.0, 90.0] {
        let mut rows = common::fixture(0);
        rows.truncate(401);
        for (i, row) in rows.iter_mut().enumerate() {
            row["close"] = serde_json::json!(if i == 0 { 100.0 } else { price });
            row["divCash"] = serde_json::json!(0.0);
        }
        let bars = parse_daily_bars(&serde_json::to_vec(&rows).unwrap()).unwrap();
        assert!(signals(&bars).unwrap()[200..].iter().all(|&s| !s));
    }
}

#[test]
fn synthetic_compute_and_causality() {
    let mut bars = inputs();
    let (before, summary) = compute(&bars).unwrap();
    assert_eq!(summary.n_cumulative, 3);
    assert_eq!(summary.psr_t, before.len() - 1);
    assert_eq!(summary.main.tax_by_year.len(), 12);
    assert!(summary.main.tax_by_year.values().any(|&t| t > 0.0));
    let boundary = date(2020, Month::June, 1).unwrap();
    for input in &mut bars {
        for b in input.iter_mut().filter(|b| b.date > boundary) {
            b.close *= 1.1;
        }
    }
    let (after, _) = compute(&bars).unwrap();
    for (a, b) in before
        .iter()
        .zip(&after)
        .take_while(|(a, _)| a.date <= boundary)
    {
        assert_eq!((a.signal, a.v, a.u, a.a), (b.signal, b.v, b.u, b.a));
    }
}

#[test]
fn missing_main_date_fails_for_each_traded_ticker() {
    for ticker in [1, 2] {
        let mut bars = inputs();
        let i = bars[ticker]
            .iter()
            .position(|b| b.date == date(2020, Month::June, 1).unwrap())
            .unwrap();
        bars[ticker].remove(i);
        assert!(
            compute(&bars)
                .unwrap_err()
                .to_string()
                .contains("missing main")
        );
    }
}

#[test]
fn insufficient_signal_history_fails() {
    let mut bars = inputs();
    let first = bars[0]
        .iter()
        .position(|b| b.date == date(2015, Month::November, 2).unwrap())
        .unwrap();
    bars[0].drain(..first - 200);
    assert!(compute(&bars).unwrap_err().to_string().contains("200 SPY"));
}

#[test]
fn invalid_bar_values_fail_closed() {
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
        assert!(compute(&bars).is_err());
    }
}

#[test]
fn gain_loss_carry_use_and_expiry() {
    let mut taxes = Taxes::default();
    assert_eq!(taxes.close_year(2010, -10.0), 0.0);
    assert_eq!(taxes.close_year(2011, 0.0), 0.0);
    assert_eq!(taxes.close_year(2012, 0.0), 0.0);
    assert_eq!(taxes.close_year(2013, 10.0), 0.0);
    assert_eq!(taxes.close_year(2015, 10.0), 2.0315);
    assert_eq!(taxes.close_year(2016, -10.0), 0.0);
    assert_eq!(taxes.close_year(2017, 4.0), 0.0);
    assert_eq!(taxes.close_year(2018, 10.0), 0.20315 * 4.0);
    assert_eq!(taxes.close_year(2019, -10.0), 0.0);
    assert_eq!(taxes.close_year(2020, 0.0), 0.0);
    assert_eq!(taxes.close_year(2021, 0.0), 0.0);
    assert_eq!(taxes.close_year(2022, 0.0), 0.0);
    assert_eq!(taxes.close_year(2023, 10.0), 2.0315);
}

#[test]
fn tax_sale_halts_at_full_holding() {
    let mut p = Position {
        ticker: 1,
        shares: 1.0,
        basis: 1.0,
    };
    assert!(p.pay_tax(0.9995, 1.0, 0.0005).is_err());
    assert!(p.pay_tax(1.0, 1.0, 0.0005).is_err());
}

proptest::proptest! {
    #[test]
    fn tax_sale_preserves_proportional_basis_and_cash(
        shares in 1.0..1000.0_f64, close in 1.0..1000.0_f64,
        basis in 1.0..1000.0_f64, fraction in 0.001..0.9_f64,
    ) {
        let mut p = Position { ticker: 1, shares, basis };
        let tax = shares * close * fraction * 0.9995;
        let (gain, v) = p.pay_tax(tax, close, 0.0005).expect("partial sale");
        proptest::prop_assert!((p.basis - basis * (1.0 - fraction)).abs() < 1e-9);
        proptest::prop_assert!((gain - (tax - basis * fraction)).abs() < 1e-9);
        proptest::prop_assert!((v - tax - shares * close * (1.0 - fraction)).abs() < 1e-8);
    }
}

#[test]
fn psr_halt_conditions() {
    assert!(psr(&[0.0, 0.0]).is_err());
    assert!(psr(&[1.0, 1.0, 2.0]).is_err());
}

#[test]
fn split_dividend_switch_and_tax_sale_order() {
    let dates = [
        date(2015, Month::December, 29).unwrap(),
        date(2015, Month::December, 30).unwrap(),
        date(2015, Month::December, 31).unwrap(),
        date(2016, Month::January, 1).unwrap(),
        date(2016, Month::January, 4).unwrap(),
    ];
    let bars = [0, 1, 2].map(|ticker| {
        dates
            .iter()
            .enumerate()
            .map(|(i, &date)| DailyBar {
                date,
                close: if ticker == 1 && i >= 3 { 6.0 } else { 10.0 },
                div_cash: if ticker == 1 && i == 2 {
                    1.0
                } else if ticker == 1 && i == 3 {
                    0.5
                } else {
                    0.0
                },
                split_factor: if ticker == 1 && i == 3 { 2.0 } else { 1.0 },
            })
            .collect()
    });
    let aligned = [[1, 1, 1], [2, 2, 2], [3, 3, 3], [4, 4, 4]];
    let (days, window) = simulate(
        &bars,
        &aligned,
        &[true, true, false, false, false],
        dates[1],
        1.0,
    )
    .unwrap();
    let original_q = 1.0 / 1.0005 / 10.0;
    let dividend_2015 = original_q;
    let q = original_q + dividend_2015 / 10.0;
    let dividend_2016 = q * 0.5;
    let proceeds = (q * 2.0 + dividend_2016 / 6.0) * 6.0 * 0.9995;
    let after_switch = proceeds / 1.0002;
    let tax = dividend_2015 * 0.20315;
    let gross_sale = tax / 0.9998;
    let expected_u = after_switch - gross_sale;
    assert!((days[2].u - expected_u).abs() < 1e-14);
    assert!((days[2].v - (expected_u + tax)).abs() < 1e-14);
    assert_eq!(days[2].holding, "BIL");
    assert_eq!(window.switch_count, 1);
    assert_eq!(window.tax_by_year[&2015], tax);
    let realized_switch = proceeds - (1.0 + dividend_2015 + dividend_2016);
    let tax_sale_gain = tax - proceeds * gross_sale / after_switch;
    let final_proceeds = expected_u * 0.9998;
    let final_basis = proceeds * (1.0 - gross_sale / after_switch);
    let final_income =
        dividend_2016 + realized_switch + tax_sale_gain + final_proceeds - final_basis;
    assert!((window.tax_by_year[&2016] - final_income.max(0.0) * 0.20315).abs() < 1e-14);
    assert!((window.w - (final_proceeds - window.tax_by_year[&2016])).abs() < 1e-14);
}

#[test]
fn loss_expiry_boundary() {
    let mut taxes = Taxes::default();
    assert_eq!(taxes.close_year(2020, -100.0), 0.0);
    assert_eq!(taxes.close_year(2023, 40.0), 0.0);
    assert_eq!(taxes.close_year(2024, 60.0), 0.20315 * 60.0);
}

#[test]
fn oldest_loss_first() {
    let mut taxes = Taxes::default();
    assert_eq!(taxes.close_year(2020, -100.0), 0.0);
    assert_eq!(taxes.close_year(2021, -100.0), 0.0);
    assert_eq!(taxes.close_year(2023, 100.0), 0.0);
    assert_eq!(taxes.close_year(2024, 100.0), 0.0);
}
