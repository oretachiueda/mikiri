use super::*;

fn d(s: &str) -> Date {
    date(s).unwrap()
}
fn series(values: &[(&str, f64)]) -> Series {
    values.iter().map(|(day, v)| (d(day), Some(*v))).collect()
}
fn near(a: f64, b: f64) {
    assert!(
        (a - b).abs() <= 1e-10 * a.abs().max(b.abs()).max(1.0),
        "{a} != {b}"
    );
}
fn observation(kind: &str, name: &str, day: Date, value: Option<f64>) -> Observation {
    Observation {
        kind: kind.into(),
        series: name.into(),
        date: day.to_string(),
        value,
        close: None,
        div_cash: None,
        split_factor: None,
    }
}
fn synthetic_rows(end: &str) -> Vec<Observation> {
    let end = d(end);
    let mut rows = Vec::new();
    let mut day = d("2007-12-01");
    while day <= end {
        if day.day() == 1 {
            for (j, area) in AREAS.iter().enumerate() {
                rows.push(observation("rate", area, day, Some(2.0 + j as f64 * 0.1)));
            }
        }
        if business(day) {
            let n = (day - d("2008-01-01")).whole_days() as f64;
            for (j, name) in FX.iter().enumerate() {
                let normalized = (n * 0.00005 + 0.025 * (n * 0.08 + j as f64 * 0.17).sin()).exp();
                let raw = if matches!(j, 1 | 3 | 4 | 7 | 8) {
                    1.0 / normalized
                } else {
                    normalized
                };
                rows.push(observation("fx", name, day, Some(raw)));
            }
            if day >= d("2008-01-01") {
                let mut row = observation("spy", "SPY", day, None);
                row.close = Some(100.0 * (n * 0.00008).exp());
                row.div_cash = Some(0.0);
                row.split_factor = Some(1.0);
                rows.push(row);
            }
        }
        day = next(day).unwrap();
    }
    rows.sort_by(|a, b| (&a.kind, &a.series, &a.date).cmp(&(&b.kind, &b.series, &b.date)));
    rows
}
fn simulation_history(start: &str, end: &str) -> Vec<Day> {
    let mut days = Vec::new();
    let mut day = d(start);
    while day <= d(end) {
        if business(day) {
            let n = days.len() as f64;
            let spot = [1.0 + n * 0.0001; 9];
            let x = if days.is_empty() {
                None
            } else {
                Some([0.001; 9])
            };
            days.push(Day {
                date: day,
                spot,
                rates: [0.02; 10],
                interest: x.map(|_| [0.0; 9]),
                x,
                mean: x.map(|_| [0.001; 9]),
                sigma: Some([0.4; 9]),
                cumulative: Some([0.1; 9]),
                signal: Some([1; 9]),
                month_end: false,
                close: 100.0,
                dividend: 0.0,
                split: 1.0,
            });
        }
        day = next(day).unwrap();
    }
    for i in 0..days.len() {
        days[i].month_end = i + 1 == days.len() || month(days[i].date) != month(days[i + 1].date);
    }
    days
}

#[test]
fn spot_direction_and_five_business_day_limit() {
    let s = series(&[("2021-01-04", 2.0)]);
    near(spot(&s, d("2021-01-04"), 0).unwrap().unwrap(), 2.0);
    near(spot(&s, d("2021-01-04"), 1).unwrap().unwrap(), 0.5);
    assert!(spot(&s, d("2021-01-11"), 0).unwrap().is_some());
    assert!(spot(&s, d("2021-01-12"), 0).unwrap().is_none());
    assert_eq!(business_count(d("2021-01-05"), d("2021-01-11")).unwrap(), 5);
}
#[test]
fn rates_apply_previous_month_on_first_calendar_day() {
    let s = series(&[
        ("2020-12-01", 1.0),
        ("2021-01-01", 3.0),
        ("2021-02-01", 9.0),
    ]);
    near(rate(&s, d("2021-01-31"), 0, &s).unwrap().unwrap(), 0.01);
    near(rate(&s, d("2021-02-01"), 0, &s).unwrap().unwrap(), 0.03);
    near(rate(&s, d("2021-02-28"), 0, &s).unwrap().unwrap(), 0.03);
}
#[test]
fn rate_gap_counts_substitute_days_in_applying_month() {
    let s = series(&[("2021-01-01", 2.0)]);
    near(rate(&s, d("2021-03-05"), 0, &s).unwrap().unwrap(), 0.02);
    near(rate(&s, d("2021-03-07"), 0, &s).unwrap().unwrap(), 0.02);
    assert!(rate(&s, d("2021-03-08"), 0, &s).unwrap().is_none());
}
#[test]
fn only_us_april_2020_gets_long_exception() {
    let s = series(&[("2020-03-01", 1.7)]);
    near(rate(&s, d("2020-05-29"), 9, &s).unwrap().unwrap(), 0.017);
    assert!(rate(&s, d("2020-05-29"), 0, &s).unwrap().is_none());
    assert!(rate(&s, d("2020-06-29"), 9, &s).unwrap().is_none());
    let missing_uk = series(&[("2026-02-01", 4.0)]);
    assert!(rate(&missing_uk, d("2026-04-08"), 2, &s).unwrap().is_none());
}
#[test]
fn calendar_interest_crosses_month_and_weekend() {
    let mut rates: [Series; 10] =
        std::array::from_fn(|_| series(&[("2020-12-01", 1.0), ("2021-01-01", 2.0)]));
    rates[0] = series(&[("2020-12-01", 4.0), ("2021-01-01", 8.0)]);
    near(
        integrated(&rates, &rates[9], d("2021-01-29"), d("2021-02-02"), 0)
            .unwrap()
            .unwrap(),
        (3.0 * 0.03 + 0.06) / 365.0,
    );
    rates[0] = series(&[("2020-12-01", -1.0), ("2021-01-01", -2.0)]);
    assert!(
        integrated(&rates, &rates[9], d("2021-01-29"), d("2021-02-02"), 0)
            .unwrap()
            .unwrap()
            < 0.0
    );
}
#[test]
fn date_parser_rejects_unicode_and_invalid_dates_without_panicking() {
    for s in ["2024é10-31", "2024-02-30", "2024-1-001", "2024-11-1x", ""] {
        assert!(date(s).is_err());
    }
}
#[test]
fn targets_use_all_nine_and_zero_signal_does_not_reallocate() {
    let mut h = simulation_history("2008-01-01", "2010-01-15");
    let i = h.iter().rposition(|v| v.date == d("2009-12-31")).unwrap();
    h[i].signal = Some([0, 1, -1, 1, 1, 1, 1, 1, 1]);
    let n = target(&h[i], i, 2.0, 1.5).unwrap();
    near(n[0], 0.0);
    near(n[1], 2.0 * 1.5 / 9.0);
    near(n[2], -2.0 * 1.5 / 9.0);
    assert!(target(&h[i], 260, 1.0, 1.0).is_err());
    h[i].sigma = Some([0.0; 9]);
    assert!(target(&h[i], i, 1.0, 1.0).is_err());
}
#[test]
fn execution_has_no_first_day_profit_and_fixed_quantity() {
    let h = simulation_history("2008-01-01", "2010-02-05");
    let s = simulate(&h, "2010-01-04", "2010-02-05", "main", SCENARIOS[0]).unwrap();
    let a = &s.days[0];
    let b = &s.days[1];
    near(a["P_EUR"].as_f64().unwrap(), 0.0);
    near(a["financing_EUR"].as_f64().unwrap(), 0.0);
    assert!(a["R"].is_null());
    near(a["transaction_EUR"].as_f64().unwrap(), 0.0001 / 9.0);
    near(
        b["q_before_EUR"].as_f64().unwrap(),
        a["q_after_EUR"].as_f64().unwrap(),
    );
    near(
        b["P_EUR"].as_f64().unwrap(),
        a["q_after_EUR"].as_f64().unwrap()
            * h.iter().find(|v| v.date == d("2010-01-04")).unwrap().spot[0]
            * 0.001,
    );
    for row in &s.days[1..20] {
        near(
            row["q_before_EUR"].as_f64().unwrap(),
            a["q_after_EUR"].as_f64().unwrap(),
        );
    }
    let last = s.days.last().unwrap();
    near(last["q_after_EUR"].as_f64().unwrap(), 0.0);
    assert_eq!(s.report["reconciliation"], true);
    assert_eq!(s.z.len(), s.days.len() - 1);
}
#[test]
fn t_signal_t_plus_one_execution_t_plus_two_first_return() {
    let mut h = simulation_history("2008-01-01", "2010-02-05");
    let jan = h.iter().position(|v| v.date == d("2010-01-29")).unwrap();
    h[jan].signal = Some([-1; 9]);
    let s = simulate(&h, "2010-01-04", "2010-02-05", "main", SCENARIOS[0]).unwrap();
    let t = s.days.iter().find(|v| v["date"] == "2010-01-29").unwrap();
    let exec = s.days.iter().find(|v| v["date"] == "2010-02-01").unwrap();
    let pnl = s.days.iter().find(|v| v["date"] == "2010-02-02").unwrap();
    assert!(t["N_EUR"].is_null());
    assert!(exec["q_after_EUR"].as_f64().unwrap() < 0.0);
    assert!(exec["P_EUR"].as_f64().unwrap() > 0.0);
    assert!(pnl["P_EUR"].as_f64().unwrap() < 0.0);
    let e = t["E"].as_f64().unwrap();
    near(exec["N_EUR"].as_f64().unwrap(), -e / 9.0);
    let p = h.iter().find(|v| v.date == d("2010-02-01")).unwrap().spot[0];
    let cost = 0.0001
        * (exec["q_after_EUR"].as_f64().unwrap() - exec["q_before_EUR"].as_f64().unwrap()).abs()
        * p;
    near(exec["transaction_EUR"].as_f64().unwrap(), cost);
}
#[test]
fn financing_charges_absolute_long_and_short_notional_over_weekend() {
    let mut h = simulation_history("2008-01-01", "2010-01-15");
    let i = h.iter().position(|v| v.date == d("2009-12-31")).unwrap();
    h[i].signal = Some([1, -1, 1, -1, 1, -1, 1, -1, 1]);
    let s = simulate(&h, "2010-01-04", "2010-01-15", "main", SCENARIOS[0]).unwrap();
    let monday = s.days.iter().find(|v| v["date"] == "2010-01-11").unwrap();
    for c in CURRENCIES {
        near(
            monday[format!("financing_{c}")].as_f64().unwrap(),
            0.0025 * monday[format!("G_{c}")].as_f64().unwrap() * 3.0 / 365.0,
        );
    }
}
#[test]
fn tax_profits_losses_oldest_first_and_expiration() {
    let mut t = Taxes::default();
    near(t.settle(2010, 10.0), 2.0315);
    near(t.settle(2011, -5.0), 0.0);
    near(t.settle(2012, -7.0), 0.0);
    near(t.settle(2013, 8.0), 0.0);
    near(t.losses[&2011], 0.0);
    near(t.losses[&2012], 4.0);
    near(t.settle(2015, 6.0), 2.0 * 0.20315);
    let mut t = Taxes::default();
    t.settle(2010, -10.0);
    near(t.settle(2014, 10.0), 2.0315);
    assert!(!t.losses.contains_key(&2010));
}
#[test]
fn final_partial_year_settles_and_pretax_asset_compounds_daily() {
    let h = simulation_history("2008-01-01", "2010-01-15");
    let s = simulate(&h, "2010-01-04", "2010-01-15", "main", SCENARIOS[0]).unwrap();
    assert_eq!(s.report["yearly"].as_array().unwrap().len(), 1);
    assert!(s.days.last().unwrap()["tax"].as_f64().unwrap() > 0.0);
    let mut a = s.days[0]["V"].as_f64().unwrap();
    for row in &s.days[1..] {
        a *= 1.0 + row["R"].as_f64().unwrap();
        near(a, row["A"].as_f64().unwrap());
    }
    let contribution_total: f64 = CURRENCIES
        .iter()
        .map(|c| s.report["contribution"][*c].as_f64().unwrap())
        .sum();
    near(
        s.report["contribution_total"].as_f64().unwrap(),
        contribution_total,
    );
    assert_eq!(s.report["reconciliation"], true);
    let taxes = s.report["total_tax"].as_f64().unwrap();
    let contribution: f64 = CURRENCIES
        .iter()
        .map(|c| s.report["contribution"][*c].as_f64().unwrap())
        .sum();
    near(1.0 + contribution - taxes, s.report["W"].as_f64().unwrap());
}
#[test]
fn baseline_applies_dividend_split_and_both_costs() {
    let mut h = simulation_history("2008-01-01", "2010-01-06");
    let b = h.iter().position(|v| v.date == d("2010-01-04")).unwrap();
    h[b + 1].close = 50.0;
    h[b + 1].split = 2.0;
    h[b + 1].dividend = 1.0;
    h[b + 2].close = 51.0;
    let (v, _) = benchmark(&h[b..]).unwrap();
    near(v[0], 1.0 / 1.0002);
    near(v[1], 1.01 / 1.0002);
    near(v[2], 1.01 * 51.0 / 50.0 / 1.0002 * 0.9998);
}
#[test]
fn ruin_is_recorded_at_first_failed_stage_and_has_no_cagr() {
    let mut h = simulation_history("2008-01-01", "2010-01-15");
    let i = h.iter().position(|v| v.date == d("2010-01-05")).unwrap();
    h[i].x = Some([-2.0; 9]);
    let s = simulate(&h, "2010-01-04", "2010-01-15", "main", SCENARIOS[0]).unwrap();
    assert!(s.ruined);
    assert_eq!(s.days.len(), 2);
    assert_eq!(s.report["ruin_date"], "2010-01-05");
    assert!(s.report["cagr_pretax"].is_null());
    assert!(s.report["cagr_aftertax"].is_null());
    let last = s.days.last().unwrap();
    near(last["financing_EUR"].as_f64().unwrap(), 0.0);
    near(last["transaction_EUR"].as_f64().unwrap(), 0.0);
    assert_eq!(s.report["reconciliation"], true);
}
#[test]
fn zero_cost_row_has_no_strategy_cost_but_unchanged_spy_cost() {
    let h = simulation_history("2008-01-01", "2010-01-15");
    let a = simulate(&h, "2010-01-04", "2010-01-15", "main", SCENARIOS[0]).unwrap();
    let z = simulate(&h, "2010-01-04", "2010-01-15", "main", SCENARIOS[5]).unwrap();
    assert_eq!(a.report["B"], z.report["B"]);
    for row in z.days {
        for c in CURRENCIES {
            near(row[format!("transaction_{c}")].as_f64().unwrap(), 0.0);
            near(row[format!("financing_{c}")].as_f64().unwrap(), 0.0);
        }
    }
}
#[test]
fn normalized_ewma_and_twelve_month_product_match_explicit_weights() {
    let rows = synthetic_rows("2024-10-31");
    let source = sources(&rows, Stage::Dev).unwrap();
    let h = history(&source, Stage::Dev).unwrap();
    let i = h.iter().position(|v| v.date == d("2009-01-30")).unwrap();
    let mut weights = 0.0;
    let mut avg = 0.0;
    let decay: f64 = 60.0 / 61.0;
    for (n, day) in h.iter().enumerate().take(i + 1).skip(1) {
        let w = decay.powi((i - n) as i32);
        weights += w;
        avg += w * day.x.unwrap()[0];
    }
    avg /= weights;
    let variance: f64 = (1..=i)
        .map(|n| decay.powi((i - n) as i32) * (h[n].x.unwrap()[0] - avg).powi(2))
        .sum::<f64>()
        / weights;
    near(h[i].mean.unwrap()[0], avg);
    near(h[i].sigma.unwrap()[0], (261.0 * variance).sqrt());
    let l = h.iter().position(|v| v.date == d("2008-01-31")).unwrap();
    let product = (l + 1..=i).fold(1.0, |a, n| a * (1.0 + h[n].x.unwrap()[0]));
    near(h[i].cumulative.unwrap()[0], product - 1.0);
}
#[test]
fn semantic_validation_rejects_duplicate_nonfinite_and_invalid_prices() {
    let mut rows = synthetic_rows("2008-01-04");
    let duplicate = rows[0].clone();
    rows.push(duplicate);
    assert!(sources(&rows, Stage::Dev).is_err());
    rows.pop();
    let i = rows.iter().position(|r| r.kind == "fx").unwrap();
    rows[i].value = Some(0.0);
    assert!(sources(&rows, Stage::Dev).is_err());
    rows[i].value = Some(f64::NAN);
    assert!(sources(&rows, Stage::Dev).is_err());
    rows[i].value = Some(1.0);
    let i = rows.iter().position(|r| r.kind == "spy").unwrap();
    rows[i].div_cash = Some(-1.0);
    assert!(sources(&rows, Stage::Dev).is_err());
    rows[i].div_cash = Some(0.0);
    rows[i].split_factor = Some(0.0);
    assert!(sources(&rows, Stage::Dev).is_err());
}
#[test]
fn dev_future_rows_do_not_change_any_numerical_output_and_reports_reuse_verdict() {
    let rows = synthetic_rows("2024-10-31");
    let a = compute(&rows, Stage::Dev).unwrap();
    let mut changed = rows.clone();
    changed.push(observation("fx", FX[0], d("2024-11-01"), Some(f64::NAN)));
    changed.push(observation(
        "rate",
        AREAS[0],
        d("2024-10-01"),
        Some(f64::NAN),
    ));
    let mut spy = observation("spy", "SPY", d("2024-11-01"), None);
    spy.close = Some(-99.0);
    changed.push(spy);
    let b = compute(&changed, Stage::Dev).unwrap();
    assert_eq!(a.summary, b.summary);
    assert_eq!(a.indicators, b.indicators);
    assert_eq!(a.days, b.days);
    assert_eq!(
        a.summary["main"],
        a.summary["report_tables"]["main"]["rows"][0]
    );
    assert_eq!(a.summary["actual_scenarios"].as_array().unwrap().len(), 8);
}
#[test]
fn absent_vintage_is_unavailable_not_fatal_and_us_level_is_not_comparable() {
    let rows = synthetic_rows("2024-10-31");
    let s = sources(&rows, Stage::Dev).unwrap();
    let h = history(&s, Stage::Dev).unwrap();
    let v = vintage(&s, &h, Stage::Dev).unwrap();
    assert_eq!(v["spot"][0]["comparable_months"], 0);
    assert!(v["spot"][0]["available_start"].is_null());
    let us = v["rates"][9]["values"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["date"] == "2020-04-01")
        .unwrap();
    assert!(us["current"].is_null());
    assert!(us["difference"].is_null());
    assert_eq!(v["rate_signals"][0]["comparable_months"], 0);
}
#[test]
fn sixth_stale_fx_weekday_errors_even_when_spy_is_closed() {
    let mut rows = synthetic_rows("2024-10-31");
    for row in &mut rows {
        if row.kind == "fx"
            && row.series == FX[0]
            && row.date.as_str() >= "2010-01-05"
            && row.date.as_str() <= "2010-01-12"
        {
            row.value = None;
        }
    }
    rows.retain(|r| !(r.kind == "spy" && r.date == "2010-01-12"));
    assert!(
        compute(&rows, Stage::Dev)
            .unwrap_err()
            .to_string()
            .contains("FX gap")
    );
}
#[test]
fn psr_invalid_inputs_are_errors() {
    assert!(crate::h001::psr(&[1.0]).is_err());
    assert!(crate::h001::psr(&[1.0, 1.0]).is_err());
    assert!(crate::h001::psr(&[f64::NAN, 1.0]).is_err());
}
#[test]
fn identical_available_vintages_reproduce_current_signals_and_missing_history_stays_null() {
    let rows = synthetic_rows("2024-10-31");
    let mut s = sources(&rows, Stage::Dev).unwrap();
    s.first_fx = s.fx.clone();
    s.first_rates = s.rates.clone();
    let h = history(&s, Stage::Dev).unwrap();
    let v = vintage(&s, &h, Stage::Dev).unwrap();
    for row in v["spot"].as_array().unwrap() {
        assert!(row["comparable_months"].as_u64().unwrap() > 0);
        assert_eq!(row["signal_flips"], 0);
        for value in row["values"].as_array().unwrap() {
            near(value["difference"].as_f64().unwrap(), 0.0);
        }
    }
    for row in v["rate_signals"].as_array().unwrap() {
        assert!(row["comparable_months"].as_u64().unwrap() > 0);
        assert_eq!(row["signal_flips"], 0);
        assert!(
            row["values"]
                .as_array()
                .unwrap()
                .iter()
                .find(|r| r["date"] == "2013-06-28")
                .unwrap()["first_signal"]
                .is_null()
        );
    }
    let may = v["rate_signals"][0]["values"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["date"] == "2020-05-29")
        .unwrap();
    assert_eq!(may["current_signal"], may["first_signal"]);
    s.first_fx[0].clear();
    let v = vintage(&s, &h, Stage::Dev).unwrap();
    assert_eq!(v["spot"][0]["comparable_months"], 0);
    assert!(v["spot"][0]["values"][0]["first_signal"].is_null());
    assert!(v["spot"][1]["comparable_months"].as_u64().unwrap() > 0);
}
#[test]
fn year_end_taxes_do_not_rebalance_until_next_month_execution() {
    let h = simulation_history("2008-01-01", "2011-01-07");
    let s = simulate(&h, "2010-01-04", "2011-01-07", "main", SCENARIOS[0]).unwrap();
    let end = s.days.iter().find(|r| r["date"] == "2010-12-31").unwrap();
    let previous = s.days.iter().find(|r| r["date"] == "2010-12-30").unwrap();
    let first = s.days.iter().find(|r| r["date"] == "2011-01-03").unwrap();
    assert!(end["tax"].as_f64().unwrap() > 0.0);
    near(
        end["q_after_EUR"].as_f64().unwrap(),
        previous["q_after_EUR"].as_f64().unwrap(),
    );
    near(
        first["N_EUR"].as_f64().unwrap(),
        end["E"].as_f64().unwrap() / 9.0,
    );
    assert_eq!(s.report["yearly"].as_array().unwrap().len(), 2);
    let second = &s.report["yearly"][1];
    near(
        second["aftertax_return"].as_f64().unwrap(),
        s.report["W"].as_f64().unwrap() / end["E"].as_f64().unwrap() - 1.0,
    );
}
