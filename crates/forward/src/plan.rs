use std::{collections::HashSet, fs::File, io::Read, os::unix::fs::PermissionsExt, path::Path};

use serde::Deserialize;
use time::{Date, OffsetDateTime, Time, UtcOffset, format_description::well_known::Iso8601};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPlan {
    pub instrument_id: String,
    pub price_increment_milli: i64,
    pub utc_offset_seconds: i32,
    pub blackout: RawWindow,
    pub legs: Vec<RawLeg>,
    pub sizing: Sizing,
    pub base_capital_milli: i64,
    pub halt_threshold_milli: i64,
    pub event_dates: Vec<String>,
    pub expires: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawWindow {
    start: String,
    end: String,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Buy,
    Sell,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLeg {
    pub name: String,
    pub direction: Direction,
    pub entry: RawWindow,
    pub exit: RawWindow,
    pub stop: Stop,
    pub max_spread_milli: i64,
    pub order_limits: OrderLimits,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Stop {
    pub enabled: bool,
    pub width_ppm: i64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sizing {
    pub unit_cap: i64,
    pub absolute_cap: i64,
    pub multiplier: i64,
    pub lot_size: i64,
    pub minimum_units: i64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrderLimits {
    pub entry: u32,
    pub stop: u32,
    pub exit: u32,
    pub cancel: u32,
}

fn date(s: &str) -> Result<Date, String> {
    Date::parse(s, &Iso8601::DATE).map_err(|e| e.to_string())
}
fn clock(s: &str) -> Result<Time, String> {
    Time::parse(
        s,
        &time::format_description::parse_borrowed::<2>("[hour]:[minute]:[second]")
            .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}
#[derive(Debug)]
pub struct Window {
    pub start: Time,
    pub end: Time,
}
#[derive(Debug)]
pub struct Leg {
    pub name: String,
    pub direction: Direction,
    pub entry: Window,
    pub exit: Window,
    pub stop: Stop,
    pub max_spread_milli: i64,
    pub order_limits: OrderLimits,
}
#[derive(Debug)]
pub struct Plan {
    pub instrument_id: String,
    pub price_increment_milli: i64,
    pub utc_offset: UtcOffset,
    pub blackout: Window,
    pub legs: Vec<Leg>,
    pub sizing: Sizing,
    pub base_capital_milli: i64,
    pub halt_threshold_milli: i64,
    pub event_dates: Vec<Date>,
    pub expires: Date,
}
impl TryFrom<RawWindow> for Window {
    type Error = String;
    fn try_from(w: RawWindow) -> Result<Self, String> {
        let result = Self {
            start: clock(&w.start)?,
            end: clock(&w.end)?,
        };
        if result.start >= result.end {
            return Err("invalid window".into());
        }
        Ok(result)
    }
}
impl Window {
    fn overlaps(&self, other: &Self) -> bool {
        self.start < other.end && other.start < self.end
    }
}
impl Plan {
    pub fn read(path: &Path, now: OffsetDateTime) -> Result<Self, String> {
        let mut file = File::open(path).map_err(|e| e.to_string())?;
        if file
            .metadata()
            .map_err(|e| e.to_string())?
            .permissions()
            .mode()
            & 0o7777
            != 0o600
        {
            return Err("plan permissions must be 600".into());
        }
        let mut text = String::new();
        file.read_to_string(&mut text).map_err(|e| e.to_string())?;
        let raw: RawPlan = toml::from_str(&text).map_err(|e| e.to_string())?;
        let utc_offset =
            UtcOffset::from_whole_seconds(raw.utc_offset_seconds).map_err(|e| e.to_string())?;
        let expires = date(&raw.expires)?;
        if expires < now.to_offset(utc_offset).date() {
            return Err("expired plan".into());
        }
        let event_dates: Vec<Date> = raw
            .event_dates
            .iter()
            .map(|d| date(d))
            .collect::<Result<_, _>>()?;
        if event_dates.is_empty() {
            return Err("empty event dates".into());
        }
        if event_dates.iter().collect::<HashSet<_>>().len() != event_dates.len() {
            return Err("duplicate event date".into());
        }
        let blackout = Window::try_from(raw.blackout)?;
        let mut names = HashSet::new();
        let mut legs = Vec::new();
        for l in raw.legs {
            let entry = Window::try_from(l.entry)?;
            let exit = Window::try_from(l.exit)?;
            if entry.end > exit.start || entry.overlaps(&blackout) || exit.overlaps(&blackout) {
                return Err("conflicting windows".into());
            }
            if l.name.trim().is_empty()
                || !names.insert(l.name.clone())
                || l.max_spread_milli <= 0
                || l.stop.width_ppm < 0
                || (l.stop.enabled && (l.stop.width_ppm == 0 || l.stop.width_ppm >= 1_000_000))
            {
                return Err("invalid leg".into());
            }
            let limits = &l.order_limits;
            if limits.entry == 0
                || limits.exit == 0
                || limits.cancel == 0
                || (l.stop.enabled && limits.stop == 0)
                || (!l.stop.enabled && limits.stop != 0)
            {
                return Err("invalid order limits".into());
            }
            legs.push(Leg {
                name: l.name,
                direction: l.direction,
                entry,
                exit,
                stop: l.stop,
                max_spread_milli: l.max_spread_milli,
                order_limits: l.order_limits,
            });
        }
        let s = &raw.sizing;
        if raw.instrument_id.trim().is_empty()
            || legs.is_empty()
            || [
                s.unit_cap,
                s.absolute_cap,
                s.multiplier,
                s.lot_size,
                s.minimum_units,
                raw.price_increment_milli,
                raw.base_capital_milli,
                raw.halt_threshold_milli,
            ]
            .iter()
            .any(|v| *v <= 0)
        {
            return Err("invalid positive value".into());
        }
        if raw.halt_threshold_milli >= raw.base_capital_milli
            || s.minimum_units > s.unit_cap
            || s.unit_cap > s.absolute_cap
            || s.unit_cap % s.lot_size != 0
            || s.minimum_units % s.lot_size != 0
        {
            return Err("inconsistent sizing".into());
        }
        Ok(Self {
            instrument_id: raw.instrument_id,
            price_increment_milli: raw.price_increment_milli,
            utc_offset,
            blackout,
            legs,
            sizing: raw.sizing,
            base_capital_milli: raw.base_capital_milli,
            halt_threshold_milli: raw.halt_threshold_milli,
            event_dates,
            expires,
        })
    }
}
#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::{fs, os::unix::fs::PermissionsExt};

    pub(crate) const VALID: &str = r#"
instrument_id = "synthetic"
price_increment_milli = 5
utc_offset_seconds = 3600
base_capital_milli = 1000000
halt_threshold_milli = 100
expires = "2099-01-01"
event_dates = ["2098-01-01"]
[blackout]
start = "01:00:00"
end = "02:00:00"
[sizing]
unit_cap = 100
absolute_cap = 200
multiplier = 2
lot_size = 10
minimum_units = 10
[[legs]]
name = "A"
direction = "buy"
max_spread_milli = 9
[legs.entry]
start = "03:10:00"
end = "03:20:00"
[legs.exit]
start = "04:10:00"
end = "04:20:00"
[legs.stop]
enabled = true
width_ppm = 20000
[legs.order_limits]
entry = 1
stop = 1
exit = 1
cancel = 1
"#;
    pub(crate) fn read(text: &str, mode: u32) -> Result<Plan, String> {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("plan.toml");
        fs::write(&path, text).expect("write");
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).expect("chmod");
        Plan::read(&path, OffsetDateTime::UNIX_EPOCH)
    }
    #[test]
    fn accepts_valid() {
        assert!(read(VALID, 0o600).is_ok());
    }
    #[test]
    fn rejects_invalid_with_expected_errors() {
        let cases = [
            (
                "permissions",
                VALID.to_string(),
                0o644,
                "plan permissions must be 600",
            ),
            (
                "unknown",
                VALID.replace("instrument_id =", "unknown = 1\ninstrument_id ="),
                0o600,
                "unknown field",
            ),
            (
                "missing",
                VALID.replace("instrument_id = \"synthetic\"\n", ""),
                0o600,
                "missing field",
            ),
            (
                "expired",
                VALID.replace("2099-01-01", "1969-01-01"),
                0o600,
                "expired plan",
            ),
            (
                "empty",
                VALID.replace("[\"2098-01-01\"]", "[]"),
                0o600,
                "empty event dates",
            ),
            (
                "duplicate",
                VALID.replace("[\"2098-01-01\"]", "[\"2098-01-01\", \"2098-01-01\"]"),
                0o600,
                "duplicate event date",
            ),
            (
                "window",
                VALID.replace("03:20:00", "03:00:00"),
                0o600,
                "invalid window",
            ),
            (
                "overlap",
                VALID.replace("03:10:00", "01:30:00"),
                0o600,
                "conflicting windows",
            ),
            (
                "order",
                VALID.replace("04:10:00", "03:15:00"),
                0o600,
                "conflicting windows",
            ),
            (
                "zero",
                VALID.replace("unit_cap = 100", "unit_cap = 0"),
                0o600,
                "invalid positive value",
            ),
            (
                "negative",
                VALID.replace("max_spread_milli = 9", "max_spread_milli = -1"),
                0o600,
                "invalid leg",
            ),
            (
                "threshold",
                VALID.replace(
                    "halt_threshold_milli = 100",
                    "halt_threshold_milli = 1000000",
                ),
                0o600,
                "inconsistent sizing",
            ),
            (
                "size order",
                VALID.replace("unit_cap = 100", "unit_cap = 300"),
                0o600,
                "inconsistent sizing",
            ),
            (
                "lot",
                VALID.replace("minimum_units = 10", "minimum_units = 11"),
                0o600,
                "inconsistent sizing",
            ),
            (
                "stop limit",
                VALID.replace("stop = 1", "stop = 0"),
                0o600,
                "invalid order limits",
            ),
            (
                "disabled stop limit",
                VALID.replace("enabled = true", "enabled = false"),
                0o600,
                "invalid order limits",
            ),
        ];
        for (name, text, mode, expected) in cases {
            let err = read(&text, mode).expect_err(name);
            assert!(err.contains(expected), "{name}: {err}");
        }
    }
}
