use super::*;
use crate::s002::{
    input::Report,
    signals::{Config, schedule},
};
use jiff::civil::date;

fn near(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() <= 1e-11 * expected.abs().max(1.0),
        "{actual} != {expected}"
    );
}

fn days(start: Date, count: usize) -> Vec<Date> {
    (0..count)
        .map(|i| start.checked_add(Span::new().days(i as i64)).unwrap())
        .collect()
}

fn empty_inputs() -> Inputs {
    Inputs {
        reports: std::array::from_fn(|_| Vec::new()),
        fixes: std::array::from_fn(|_| Series::new()),
        rates: std::array::from_fn(|_| [(date(1995, 1, 1), 0.02)].into()),
        spy: Series::new(),
        hashes: BTreeMap::new(),
    }
}

fn empty_schedule(n: usize) -> Schedule {
    Schedule {
        actions: vec![[None; LEGS]; n],
        active: vec![[true; LEGS]; n],
    }
}

fn fix(price: f64, spot: f64, carry: (f64, f64)) -> Fix {
    Fix {
        price,
        spot,
        log_return: Some(spot.ln_1p()),
        carry: Some(carry),
    }
}

#[test]
fn strict_previous_fixes_and_fill_after_signal_g5() {
    let dates = days(date(1995, 9, 1), 130);
    let mut input = empty_inputs();
    let index = 65;
    for (i, &day) in dates.iter().enumerate() {
        let price = 100.0 + (i as f64).sin();
        input.fixes[1].insert(day, 1.0 + (i as f64).cos() / 100.0);
        if i != index + 1 {
            input.fixes[0].insert(day, price);
        }
    }
    let mut market = Market::new(&input).unwrap();
    market.start = index;
    let mut signals = empty_schedule(dates.len());
    signals.actions[index][0] = Some(Action::Report(1));
    let original = simulate(&market, &mut Sizing::default(), &signals, None).unwrap();
    let direction = std::array::from_fn(|leg| i8::from(leg == 0));
    let expected = Sizing::default()
        .targets(&market, index, direction)
        .unwrap()[0];
    assert_eq!(original.days[0].after, 0.0);
    assert_eq!(original.days[1].after, 0.0);
    let fill_price = input.fixes[0][&dates[index + 2]];
    near(original.days[2].after, -0.001 / fill_price * expected);

    *input.fixes[0].get_mut(&dates[index]).unwrap() *= 1.5;
    let mut changed = Market::new(&input).unwrap();
    changed.start = index;
    let revised = simulate(&changed, &mut Sizing::default(), &signals, None).unwrap();
    near(revised.days[2].after, original.days[2].after);
    let same_day_lookahead = Sizing::default()
        .targets(&changed, index + 1, direction)
        .unwrap()[0];
    assert!((same_day_lookahead - expected).abs() > 0.5);
    signals.actions[index - 1][0] = signals.actions[index][0].take();
    changed.start -= 1;
    let early = simulate(&changed, &mut Sizing::default(), &signals, None).unwrap();
    assert!(early.days[1].after < 0.0);
}

#[test]
fn joint_sizing_uses_signed_todays_weights_and_complete_joint_days() {
    let n = VOL_WINDOW + 1;
    let market = Market {
        dates: days(DEV_START, n),
        start: 0,
        legs: (0..n)
            .map(|index| {
                let a = 0.001_f64 * if index % 2 == 0 { 1.0 } else { -1.0 };
                let b = 0.002_f64 * if index / 2 % 2 == 0 { 1.0 } else { -1.0 };
                let rest = [LegDay::default(); LEGS];
                let mut legs = rest;
                for (leg, log, amp) in [(0, a, 0.001), (1, b, 0.002)] {
                    legs[leg] = LegDay {
                        fix: Some(Fix {
                            price: 1.0,
                            spot: log.exp_m1(),
                            log_return: Some(log),
                            carry: Some((0.0, 0.0)),
                        }),
                        sigma: Some(amp * (252.0 * 60.0 / 59.0_f64).sqrt()),
                        rates_ready: true,
                    };
                }
                legs
            })
            .collect(),
        baseline: vec![],
    };
    let direction = [1, -1, 0, 0, 0, 0, 0, 0];
    let targets = Sizing::default().targets(&market, 60, direction).unwrap();
    near(targets[0], 20.0 / 3.0);
    near(targets[1], -10.0 / 3.0);
    near(targets.into_iter().map(f64::abs).sum(), 10.0);
    assert_eq!(
        Sizing::default().targets(&market, 59, direction).unwrap(),
        [0.0; LEGS]
    );
    let mut uncapped = Market {
        dates: market.dates.clone(),
        start: market.start,
        legs: market.legs.clone(),
        baseline: vec![],
    };
    for row in &mut uncapped.legs {
        for leg in &mut row[..2] {
            leg.sigma = leg.sigma.map(|sigma| 10.0 * sigma);
            if let Some(fix) = &mut leg.fix {
                fix.log_return = fix.log_return.map(|log| 10.0 * log);
            }
        }
    }
    let targets = Sizing::default().targets(&uncapped, 60, direction).unwrap();
    let expected = 0.30 / 2.0_f64.sqrt() / uncapped.legs[60][0].sigma.unwrap();
    near(targets[0], expected);
    near(targets[1], -expected / 2.0);
    let mut market = market;
    market.legs[3][1].fix = None;
    assert_eq!(
        Sizing::default().targets(&market, 60, direction).unwrap(),
        [0.0; LEGS]
    );
    let jpy = [1, 0, 0, 0, 0, 0, 0, 0];
    near(
        Sizing::default().targets(&market, 60, jpy).unwrap()[0],
        10.0,
    );
    market.legs[60][0].sigma = Some(0.12);
    near(Sizing::default().targets(&market, 60, jpy).unwrap()[0], 2.5);
    market.legs[60][0].sigma = None;
    assert_eq!(
        Sizing::default().targets(&market, 60, jpy).unwrap(),
        [0.0; LEGS]
    );
}

#[test]
fn one_trade_hand_calc_uses_fix_pnl_carry_markup_spread_and_fixed_notional_t5() {
    let dates = days(date(2024, 1, 28), 7);
    let mut rates = empty_inputs().rates;
    rates[0] = [(date(2023, 10, 1), 0.01), (date(2023, 12, 1), 0.02)].into();
    rates[LEGS] = [(date(2023, 11, 1), 0.05), (date(2023, 12, 1), 0.06)].into();
    let jpy_carry = carry(0, &rates, dates[2], dates[5]).unwrap().unwrap();
    near(jpy_carry.0, (0.05 - 0.01 + 2.0 * (0.06 - 0.02)) / 365.0);
    near(jpy_carry.1, 3.0 * 0.0135 / 365.0);
    near(
        carry(7, &rates, dates[2], dates[5]).unwrap().unwrap().1,
        3.0 * 0.020 / 365.0,
    );

    let mut market = Market {
        dates,
        start: 0,
        legs: vec![[LegDay::default(); LEGS]; 7],
        baseline: vec![],
    };
    market.legs[0][0].sigma = Some(0.30);
    market.legs[0][0].rates_ready = true;
    market.legs[3][0].sigma = Some(0.30);
    market.legs[2][0].fix = Some(fix(100.0, 0.0, (0.0, 0.0)));
    market.legs[5][0].fix = Some(fix(101.0, 0.01, jpy_carry));
    market.legs[6][0].fix = Some(fix(102.0, 102.0 / 101.0 - 1.0, (0.0, 0.0)));
    let mut signals = empty_schedule(7);
    signals.actions[0][0] = Some(Action::Report(-1));
    signals.actions[3][0] = Some(Action::Exit);

    let account = simulate(&market, &mut Sizing::default(), &signals, None).unwrap();
    let entry = 0.001 / 100.0;
    let gross_loss = 0.01 + 0.12 / 365.0;
    let marked_loss = gross_loss + 3.0 * 0.0135 / 365.0;
    let exit = 0.001 / 101.0;
    near(account.days[2].after, -entry);
    near(account.days[5].before, -gross_loss / (1.0 - entry));
    near(account.days[5].after, -(marked_loss + exit) / (1.0 - entry));
    near(account.equity, 1.0 - entry - marked_loss - exit);
    assert_eq!(account.days[6].after, 0.0);
    assert_eq!(account.fills, 2);
    near(ROUND_TRIP[5] / 2.0, 0.00003);
}

#[test]
fn fixed_notional_holds_until_next_action_and_bust_stops() {
    let mut market = Market {
        dates: days(DEV_START, 5),
        start: 0,
        legs: vec![[LegDay::default(); LEGS]; 5],
        baseline: vec![],
    };
    for legs in &mut market.legs {
        legs[0].fix = Some(fix(100.0, 0.02, (0.0, 0.0)));
        legs[0].sigma = Some(0.15);
        legs[0].rates_ready = true;
    }
    let mut signals = empty_schedule(5);
    signals.actions[0][0] = Some(Action::Report(1));
    signals.actions[2][0] = Some(Action::Report(1));
    let run = simulate(&market, &mut Sizing::default(), &signals, None).unwrap();
    let previous = 1.0 - 0.001 / 100.0 * 2.0;
    near(run.days[2].after, 0.04 / previous);
    let previous = previous + 0.04;
    let new_notional = previous * 2.0;
    let resize = (new_notional - 2.0) * 0.001 / 100.0;
    near(run.days[3].after, (0.04 - resize) / previous);
    near(
        run.days[4].after,
        new_notional * 0.02 / (previous + 0.04 - resize),
    );
    market.legs[4][0].fix = Some(fix(1.0, -0.99, (0.0, 0.0)));
    assert!(simulate(&market, &mut Sizing::default(), &signals, None).is_err());
}

#[test]
fn tax_loss_carries_three_years_and_settles_stub_years() {
    let dates = [
        date(2018, 11, 30),
        date(2019, 12, 31),
        date(2020, 12, 31),
        date(2021, 12, 31),
        date(2022, 12, 31),
        date(2023, 10, 31),
    ];
    let days = [-0.2, 0.125, -0.1, 0.0, 0.0, 0.25].map(|v| Daily {
        after: v,
        before: v,
    });
    let taxable = 0.81 * 0.25 - 0.09;
    near(
        retax(&dates, &days).unwrap(),
        0.81 * 1.25 - taxable * 0.20315,
    );
}

#[test]
fn future_inputs_leave_past_signals_sizes_and_returns_identical() {
    let mut input = empty_inputs();
    let dates = days(date(1995, 8, 1), 350);
    for (i, &day) in dates.iter().enumerate() {
        input.fixes[0].insert(day, 100.0 + (i as f64).sin() / 3.0);
        input.spy.insert(day, 100.0 + i as f64);
        if i % 7 == 0 {
            input.reports[0].push(Report {
                as_of: day,
                slot: i / 7,
                hp: if i % 2 == 0 { 0.6 } else { -0.6 },
            });
        }
    }
    let cfg = Config::all().next().unwrap();
    let cut = 260;
    let t = dates[cut];
    let market = Market::new(&input).unwrap();
    let s = schedule(&market.dates, &input.reports, cfg, market.start).unwrap();
    let original = simulate(&market, &mut Sizing::default(), &s, None).unwrap();

    for (_, p) in
        input.fixes[0].range_mut((std::ops::Bound::Excluded(t), std::ops::Bound::Unbounded))
    {
        *p *= 1.005;
    }
    for report in input.reports[0].iter_mut().filter(|r| r.as_of > t) {
        report.hp *= -1.0;
    }
    input.rates[0].insert(date(1996, 6, 1), 0.05);
    input.spy.insert(dates[cut + 1], 10_000.0);
    let changed = Market::new(&input).unwrap();
    let changed_s = schedule(&changed.dates, &input.reports, cfg, changed.start).unwrap();
    let revised = simulate(&changed, &mut Sizing::default(), &changed_s, None).unwrap();
    assert_eq!(s.actions[..=cut], changed_s.actions[..=cut]);
    let q = [1, 0, 0, 0, 0, 0, 0, 0];
    for index in market.start..=cut {
        assert_eq!(
            Sizing::default().targets(&market, index, q).unwrap(),
            Sizing::default().targets(&changed, index, q).unwrap()
        );
    }
    assert_eq!(
        original.days[..=cut - market.start],
        revised.days[..=cut - market.start]
    );
    assert_ne!(original.days, revised.days);
}

#[test]
fn no_edge_and_planted_edge_are_distinguished_g1_g3() {
    let n = 2200;
    let dates = days(date(1995, 7, 1), n);
    let mut rng = 574_831_327_u64;
    let mut signals = empty_schedule(n);
    let mut input = empty_inputs();
    let start = dates.partition_point(|&day| day < DEV_START);
    let mut flat = 100.0;
    let mut edge = 100.0;
    let mut planted = Series::new();
    for (i, &day) in dates.iter().enumerate() {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        let noise = if rng.is_multiple_of(2) { 0.004 } else { -0.004 };
        let signal = |i| if i / 7 % 2 == 0 { 1 } else { -1 };
        if i >= start && i % 7 == 0 {
            signals.actions[i][0] = Some(Action::Report(signal(i)));
        }
        let premium = if i >= start + 9 {
            0.001 * f64::from(signal(i - 2))
        } else {
            0.0
        };
        flat *= 1.0 + noise;
        edge *= 1.0 + noise + premium;
        input.fixes[0].insert(day, flat);
        planted.insert(day, edge);
    }
    let t = |prices: &Inputs| {
        let market = Market::new(prices).unwrap();
        let run = simulate(&market, &mut Sizing::default(), &signals, None).unwrap();
        let r: Vec<_> = run.days.iter().map(|d| d.after).collect();
        r.iter().sum::<f64>() / (r.len() as f64 * sample_variance(&r)).sqrt()
    };
    let null = t(&input);
    assert!(null.abs() < 3.0, "unplanted {null}");
    input.fixes[0] = planted;
    let positive = t(&input);
    assert!(positive > 3.0, "planted {positive}");
}

#[test]
fn dev_starts_flat_and_benchmark_carries_last_joint_value() {
    let mut input = empty_inputs();
    let dates = [
        date(1996, 1, 1),
        DEV_START,
        date(2019, 11, 1),
        date(2024, 10, 31),
    ];
    input.fixes[0].extend([(dates[0], 100.0), (dates[1], 95.0), (dates[2], 125.0)]);
    input.fixes[1].insert(dates[3], 1.0);
    input
        .spy
        .extend([(dates[0], 2.0), (dates[2], 3.0), (dates[3], 4.0)]);
    let market = Market::new(&input).unwrap();
    assert_eq!(market.dates[market.start], DEV_START);
    near(market.baseline(market.start).unwrap(), 375.0 / 200.0);
    near(market.baseline(2).unwrap(), 1.0);
    let mut signals = empty_schedule(4);
    signals.actions[0][0] = Some(Action::Report(1));
    assert!(
        simulate(&market, &mut Sizing::default(), &signals, None)
            .unwrap()
            .days
            .iter()
            .all(|day| day.after == 0.0)
    );
}

#[test]
fn circular_shift_wraps_signal_days_and_skips_inactive_destinations() {
    let n = 6;
    let row = LegDay {
        fix: Some(fix(100.0, 0.01, (0.0, 0.0))),
        sigma: Some(0.30),
        rates_ready: true,
    };
    let market = Market {
        dates: days(DEV_START, n),
        start: 1,
        legs: vec![[row; LEGS]; n],
        baseline: vec![],
    };
    let mut s = empty_schedule(n);
    s.actions[1][0] = Some(Action::Report(1));
    s.actions[4][0] = Some(Action::Report(-1));
    s.active[3][0] = false;
    let shifted = simulate(&market, &mut Sizing::default(), &s, Some(2)).unwrap();
    assert_eq!(shifted.skipped, 1);
    assert_eq!(shifted.days[0].after, 0.0);
    near(shifted.days[1].after, -0.001 / 100.0);
    near(shifted.days[2].before, -0.01 / (1.0 - 0.001 / 100.0));
    assert_eq!(shifted.days[4].after, 0.0);
    assert_eq!(shifted.fills, 2);
}
