use serde_json::{Value, json};
use time::{Date, Duration, Month, Weekday};

pub fn fixture(ticker: usize) -> Vec<Value> {
    let mut day = Date::from_calendar_date(2014, Month::January, 1).expect("fixture start");
    let end = Date::from_calendar_date(2026, Month::November, 10).expect("fixture end");
    let mut rows = Vec::new();
    let mut scale = 1.0;
    let mut i = 0;
    while day <= end {
        if !matches!(day.weekday(), Weekday::Saturday | Weekday::Sunday) {
            let split = if ticker == 1 && day.to_string() == "2017-05-01" {
                4.0
            } else if ticker == 2 && day.to_string() == "2017-11-30" {
                0.5
            } else {
                1.0
            };
            scale *= split;
            let x = i as f64;
            let close = match ticker {
                0 => 100.0 * (0.00015 * x + 0.15 * (x / 90.0).sin()).exp(),
                1 => 100.0 * (0.0002 * x + 0.5 * (x / 70.0).sin()).exp() / scale,
                _ => 100.0 * (0.00001 * x + 0.005 * (x / 30.0).sin()).exp() / scale,
            };
            let dividend = if i % 63 == 0 || split != 1.0 {
                0.1
            } else {
                0.0
            };
            rows.push(
                json!({"date": format!("{day}T00:00:00.000Z"), "close": close,
                "high":close, "low":close, "open":close, "adjClose":close,
                "adjHigh":close,"adjLow":close,"adjOpen":close,"divCash":dividend,
                "splitFactor":split,"volume":1000,"adjVolume":1000}),
            );
            i += 1;
        }
        day += Duration::days(1);
    }
    rows
}
