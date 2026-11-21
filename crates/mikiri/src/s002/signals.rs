use super::{
    Result,
    input::{LEGS, Report},
    mean, sample_variance,
};
use jiff::{
    Span,
    civil::{Date, Weekday, date},
};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Action {
    Report(i8),
    Exit,
}

impl Action {
    pub fn direction(self) -> i8 {
        match self {
            Self::Report(direction) => direction,
            Self::Exit => 0,
        }
    }
}

pub(super) type Actions = [Option<Action>; LEGS];

#[derive(Clone, Copy)]
pub(super) enum Signal {
    Level,
    Z52,
    Z156,
}
impl Signal {
    fn window(self) -> usize {
        match self {
            Self::Level => 1,
            Self::Z52 => 52,
            Self::Z156 => 156,
        }
    }

    fn score(self, published: &BTreeMap<usize, f64>, report: Report) -> Result<Option<f64>> {
        let window = self.window();
        if window == 1 {
            return Ok(Some(report.hp));
        }
        let Some(first) = (report.slot + 1).checked_sub(window) else {
            return Ok(None);
        };
        let points: Vec<_> = published
            .range(first..=report.slot)
            .map(|(_, &v)| v)
            .collect();
        if points.len() != window {
            return Ok(None);
        }
        let variance = sample_variance(&points);
        if !variance.is_finite() || variance <= 0.0 {
            return Err("invalid COT window variance".into());
        }
        Ok(Some((report.hp - mean(&points)) / variance.sqrt()))
    }
}

#[derive(Clone, Copy)]
pub(super) struct Config {
    pub signal: Signal,
    pub band: bool,
    pub even: bool,
    pub fx8: bool,
}

impl Config {
    pub fn all() -> impl Iterator<Item = Self> {
        [Signal::Level, Signal::Z52, Signal::Z156]
            .into_iter()
            .flat_map(|signal| {
                [false, true].into_iter().flat_map(move |band| {
                    [false, true].into_iter().flat_map(move |even| {
                        [false, true].into_iter().map(move |fx8| Self {
                            signal,
                            band,
                            even,
                            fx8,
                        })
                    })
                })
            })
    }

    pub fn id(self) -> String {
        let signal = match self.signal {
            Signal::Level => "lvl",
            Signal::Z52 => "z52",
            Signal::Z156 => "z156",
        };
        let universe = if self.fx8 { "fx8" } else { "jpy" };
        format!(
            "{signal}-b{}-h{}-{universe}",
            u8::from(self.band),
            1 + u8::from(self.even)
        )
    }

    fn quote_direction(self, leg: usize, score: f64) -> i8 {
        let band = match (self.band, self.signal) {
            (false, _) => 0.0,
            (true, Signal::Level) => 0.10,
            (true, _) => 1.0,
        };
        if score.abs() <= band {
            return 0;
        }
        let foreign_long = if score > 0.0 { 1 } else { -1 };
        if usd_base(leg) {
            -foreign_long
        } else {
            foreign_long
        }
    }
}

pub(super) fn usd_base(leg: usize) -> bool {
    matches!(leg, 0 | 3 | 4 | 7)
}

fn public_bound(as_of: Date) -> Result<Date> {
    let date = if (date(2013, 10, 1)..=date(2013, 11, 19)).contains(&as_of) {
        date(2013, 11, 29)
    } else if (date(2018, 12, 24)..=date(2019, 3, 12)).contains(&as_of) {
        date(2019, 3, 29)
    } else {
        let delay = if as_of.weekday() == Weekday::Tuesday {
            6
        } else {
            14
        };
        as_of.checked_add(Span::new().days(delay))?
    };
    Ok(date)
}

pub(super) struct Schedule {
    pub actions: Vec<Actions>,
    pub cot_ready: Vec<[bool; LEGS]>,
    pub report_as_of: Vec<[Option<Date>; LEGS]>,
}

pub(super) fn schedule(
    dates: &[Date],
    reports: &[Vec<Report>; LEGS],
    config: Config,
    start: usize,
) -> Result<Schedule> {
    let mut schedule = Schedule {
        actions: vec![[None; LEGS]; dates.len()],
        cot_ready: vec![[false; LEGS]; dates.len()],
        report_as_of: vec![[None; LEGS]; dates.len()],
    };
    for (leg, reports) in reports
        .iter()
        .enumerate()
        .take(if config.fx8 { LEGS } else { 1 })
    {
        let mut publications = BTreeMap::<usize, Vec<Report>>::new();
        for &report in reports {
            let bound = public_bound(report.as_of)?;
            let index = dates.partition_point(|day| *day < bound);
            publications.entry(index).or_default().push(report);
        }
        let mut published = BTreeMap::new();
        let mut expires = None;
        let mut qualified = false;
        for (index, &day) in dates.iter().enumerate() {
            let mut action = None;
            let mut action_report = None;
            let mut latest_report = None;
            if let Some(batch) = index.checked_sub(1).and_then(|i| publications.get_mut(&i)) {
                batch.sort_unstable_by_key(|report| report.as_of);
                for &report in batch.iter() {
                    latest_report = Some(report.as_of);
                    published.insert(report.slot, report.hp);
                    let score = config.signal.score(&published, report)?;
                    qualified = score.is_some();
                    if !config.even || report.slot % 2 == 0 {
                        action_report = Some(report.as_of);
                        action = Some(match score {
                            Some(score) => Action::Report(config.quote_direction(leg, score)),
                            None => Action::Exit,
                        });
                    }
                }
            }
            let publishes_today = publications.contains_key(&index);
            if expires
                .is_some_and(|deadline| day > deadline || (day == deadline && !publishes_today))
            {
                action = Some(Action::Exit);
                expires = None;
            }
            if publishes_today {
                expires = Some(day.checked_add(Span::new().days(14))?);
            }
            if index >= start {
                schedule.actions[index][leg] = action;
                schedule.report_as_of[index][leg] = action_report.or(latest_report);
            }
            schedule.cot_ready[index][leg] = qualified;
        }
    }
    Ok(schedule)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reports(leg: usize, rows: Vec<Report>) -> [Vec<Report>; LEGS] {
        let mut all = std::array::from_fn(|_| Vec::new());
        all[leg] = rows;
        all
    }

    fn days(start: Date, end: Date) -> Vec<Date> {
        (0..=super::super::input::days_between(start, end))
            .map(|n| start.checked_add(Span::new().days(n)).unwrap())
            .collect()
    }

    fn report(as_of: Date, slot: usize, hp: f64) -> Report {
        Report { as_of, slot, hp }
    }

    #[test]
    fn public_date_uses_calendar_then_signal_uses_strictly_next_date_t1() {
        let dates = [
            date(1996, 1, 2),
            date(1996, 1, 9),
            date(1996, 1, 10),
            date(1996, 1, 11),
        ];
        let data = reports(0, vec![report(dates[0], 170, 0.8)]);
        let decisions = schedule(&dates, &data, Config::all().next().unwrap(), 0).unwrap();
        assert_eq!(public_bound(dates[0]).unwrap(), date(1996, 1, 8));
        let expected = vec![None, None, Some(Action::Report(-1)), None];
        assert_eq!(
            decisions
                .actions
                .iter()
                .map(|day| day[0])
                .collect::<Vec<_>>(),
            expected
        );
    }

    #[test]
    fn non_tuesday_report_uses_fourteen_days_before_calendar_roll() {
        let dates = [
            date(1997, 12, 19),
            date(1997, 12, 26),
            date(1998, 1, 5),
            date(1998, 1, 6),
        ];
        let data = reports(0, vec![report(dates[0], 271, 0.8)]);
        let s = schedule(&dates, &data, Config::all().next().unwrap(), 0).unwrap();
        assert_eq!(public_bound(dates[0]).unwrap(), date(1998, 1, 2));
        assert_eq!(s.actions[1][0], None);
        assert_eq!(s.actions[2][0], None);
        assert_eq!(s.actions[3][0], Some(Action::Report(-1)));
    }

    #[test]
    fn shutdown_reports_batch_in_as_of_order_after_stale_exit_t2() {
        let dates = days(date(2013, 9, 24), date(2013, 12, 2));
        let data = reports(
            0,
            vec![
                report(date(2013, 9, 24), 1000, 0.8),
                report(date(2013, 11, 19), 1008, -0.8),
                report(date(2013, 10, 1), 1001, 0.8),
            ],
        );
        let s = schedule(&dates, &data, Config::all().next().unwrap(), 0).unwrap();
        let events: Vec<_> = dates
            .iter()
            .zip(s.actions)
            .filter_map(|(&day, row)| Some((day, row[0]?.direction())))
            .collect();
        assert_eq!(
            events,
            [
                (date(2013, 10, 1), -1),
                (date(2013, 10, 14), 0),
                (date(2013, 11, 30), 1),
            ]
        );
        assert_eq!(public_bound(date(2018, 12, 24)).unwrap(), date(2019, 3, 29));
        assert_eq!(public_bound(date(2019, 3, 12)).unwrap(), date(2019, 3, 29));
    }

    #[test]
    fn date_staleness_uses_publications_even_for_odd_slots_and_nzd_gap_is_cash_t4() {
        let dates = days(date(2000, 4, 4), date(2004, 9, 1));
        let data = reports(
            6,
            vec![
                report(dates[0], 390, 0.8),
                report(date(2000, 4, 11), 391, 0.9),
                report(date(2004, 8, 17), 618, 0.8),
            ],
        );
        let cfg = Config::all().find(|cfg| cfg.fx8 && cfg.even).unwrap();
        let s = schedule(&dates, &data, cfg, 0).unwrap();
        let events: Vec<_> = dates
            .iter()
            .zip(s.actions)
            .filter_map(|(&day, row)| Some((day, row[6]?.direction())))
            .collect();
        assert_eq!(
            events,
            [
                (date(2000, 4, 11), 1),
                (date(2000, 5, 1), 0),
                (date(2004, 8, 24), 1),
            ]
        );
    }

    #[test]
    fn z_windows_require_exact_slots_and_refill_before_reactivation() {
        let mut published = BTreeMap::new();
        for slot in 0..310 {
            if slot == 156 {
                continue;
            }
            let hp = slot as f64 / 310.0;
            published.insert(slot, hp);
            let report = report(date(2000, 1, 1), slot, hp);
            for (signal, length) in [(Signal::Z52, 52), (Signal::Z156, 156)] {
                let score = signal.score(&published, report).unwrap();
                let complete = (length - 1..156).contains(&slot) || slot >= 156 + length;
                assert_eq!(score.is_some(), complete, "slot {slot}, window {length}");
                if let Some(score) = score {
                    let expected =
                        (length - 1) as f64 / 2.0 / ((length * (length + 1)) as f64 / 12.0).sqrt();
                    assert!((score - expected).abs() < 1e-12);
                }
            }
        }
    }

    #[test]
    fn missing_week_queues_cash_and_contiguous_refill_enables_z_again() {
        let start = date(1999, 1, 5);
        let dates = days(start, start.checked_add(Span::new().days(110 * 7)).unwrap());
        let reports = reports(
            0,
            (0..110)
                .filter(|&slot| slot != 53)
                .map(|slot| report(dates[slot * 7], slot, slot as f64 / 110.0))
                .collect(),
        );
        let config = Config {
            signal: Signal::Z52,
            ..Config::all().next().unwrap()
        };
        let s = schedule(&dates, &reports, config, 0).unwrap();
        assert_eq!(s.actions[(52 + 1) * 7][0], Some(Action::Report(-1)));
        assert_eq!(s.actions[(54 + 1) * 7][0], Some(Action::Exit));
        assert!(!s.cot_ready[(104 + 1) * 7][0]);
        assert_eq!(s.actions[(105 + 1) * 7][0], Some(Action::Report(-1)));
    }

    #[test]
    fn h2_does_not_place_an_order_on_an_odd_incomplete_report() {
        let start = date(1999, 1, 5);
        let dates = days(start, start.checked_add(Span::new().days(58 * 7)).unwrap());
        let reports = reports(
            0,
            (0..58)
                .filter(|&slot| slot != 54)
                .map(|slot| report(dates[slot * 7], slot, slot as f64 / 58.0))
                .collect(),
        );
        let config = Config {
            signal: Signal::Z52,
            even: true,
            ..Config::all().next().unwrap()
        };
        let s = schedule(&dates, &reports, config, 0).unwrap();
        assert_eq!(s.actions[(52 + 1) * 7][0], Some(Action::Report(-1)));
        assert_eq!(s.actions[(55 + 1) * 7][0], None);
        assert_eq!(s.actions[(56 + 1) * 7][0], Some(Action::Exit));
    }

    #[test]
    fn inclusive_bands_and_quote_direction_match_pair_orientation() {
        let cfg = Config::all()
            .find(|cfg| cfg.band && cfg.even && cfg.fx8)
            .unwrap();
        assert_eq!(cfg.quote_direction(0, 0.10), 0);
        assert_eq!(cfg.quote_direction(0, 0.11), -1);
        assert_eq!(cfg.quote_direction(3, -0.11), 1);
        assert_eq!(cfg.quote_direction(1, -0.11), -1);
        let z = Config {
            signal: Signal::Z52,
            ..cfg
        };
        assert_eq!(z.quote_direction(2, -1.0), 0);
        assert_eq!(z.quote_direction(2, 1.1), 1);
        assert_eq!(Config::all().count(), 24);
    }
}
