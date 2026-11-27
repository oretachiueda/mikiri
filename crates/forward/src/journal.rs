use crate::plan::Plan;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};
use std::{
    collections::HashSet,
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    str::FromStr,
};
use time::{
    Date, OffsetDateTime,
    format_description::well_known::{Iso8601, Rfc3339},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Milli(pub i64);
impl Serialize for Milli {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let value = i128::from(self.0);
        let sign = if value < 0 { "-" } else { "" };
        let magnitude = value.abs();
        let s = format!("{sign}{}.{:03}", magnitude / 1000, magnitude % 1000);
        serde_json::Number::from_str(&s)
            .map_err(serde::ser::Error::custom)?
            .serialize(serializer)
    }
}
impl<'de> Deserialize<'de> for Milli {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let n = serde_json::Number::deserialize(deserializer)?;
        let s = n.to_string();
        let (mantissa, exponent) = match s.split_once(['e', 'E']) {
            Some((m, e)) => (m, e.parse::<i32>().map_err(D::Error::custom)?),
            None => (s.as_str(), 0),
        };
        let negative = mantissa.starts_with('-');
        let unsigned = mantissa.strip_prefix('-').unwrap_or(mantissa);
        let (whole, fraction) = unsigned.split_once('.').unwrap_or((unsigned, ""));
        let digits = format!("{whole}{fraction}");
        let mut value = digits.parse::<i128>().map_err(D::Error::custom)?;
        let scale = exponent
            .checked_add(3)
            .and_then(|v| v.checked_sub(i32::try_from(fraction.len()).ok()?))
            .ok_or_else(|| D::Error::custom("money scale overflow"))?;
        let power = 10_i128
            .checked_pow(scale.unsigned_abs())
            .ok_or_else(|| D::Error::custom("money scale overflow"))?;
        if scale >= 0 {
            value = value
                .checked_mul(power)
                .ok_or_else(|| D::Error::custom("money overflow"))?;
        } else {
            if value % power != 0 {
                return Err(D::Error::custom("money exceeds milli precision"));
            }
            value /= power;
        }
        if negative {
            value = -value;
        }
        Ok(Self(i64::try_from(value).map_err(D::Error::custom)?))
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Environment {
    Live,
    Sim,
}
impl Environment {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Live => "LIVE",
            Self::Sim => "SIM",
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Declare,
    Entry,
    Exit,
    StopExit,
    ManualClose,
    Cancel,
    Halt,
    Kill,
    Resume,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Scheduled,
    Filled,
    Partial,
    Skipped,
    Missed,
    Halted,
    Killed,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CostStatus {
    Confirmed,
    Pending,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Side {
    Buy,
    Sell,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Row {
    pub version: u8,
    pub env: Environment,
    pub event_id: String,
    pub leg: String,
    pub tokyo_date: String,
    pub action: Action,
    pub idem_key: String,
    pub status: Status,
    pub recorded_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub declared_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub side: Option<Side>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub units: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub order_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fill_time: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fill_price: Option<Milli>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commission_jpy: Option<Milli>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_charge_jpy: Option<Milli>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_status: Option<CostStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub swap_jpy: Option<Milli>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub swap_status: Option<CostStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub realized_pnl_jpy_ex_swap: Option<Milli>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub equity_before_jpy: Option<Milli>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correction_of: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_sha256: Option<String>,
}
fn filled(status: Status) -> bool {
    matches!(status, Status::Filled | Status::Partial)
}
fn timestamp(s: &str) -> Result<(), String> {
    OffsetDateTime::parse(s, &Rfc3339)
        .map(|_| ())
        .map_err(|_| "invalid timestamp".into())
}
fn valid_hash(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

/// A single-use authorization returned only after an intent is durable.
///
/// ```compile_fail
/// use forward::journal::Permit;
/// fn duplicate(permit: Permit) { let _second = permit.clone(); }
/// ```
///
/// ```compile_fail
/// use forward::journal::Permit;
/// let _permit = Permit { intent: todo!() };
/// ```
#[must_use]
#[derive(Debug)]
pub struct Permit {
    intent: Row,
    max_units: i64,
    lot_size: i64,
}
impl Permit {
    pub fn leg(&self) -> &str {
        &self.intent.leg
    }
    pub fn authorize(
        mut self,
        key: &str,
        leg: &str,
        side: Side,
        units: i64,
    ) -> Result<Row, String> {
        if self.intent.idem_key != key
            || self.intent.leg != leg
            || self.intent.side != Some(side)
            || units <= 0
        {
            return Err("Human: order does not match permit".into());
        }
        if self.intent.action == Action::Entry {
            if units > self.max_units
                || units % self.lot_size != 0
                || self.intent.units.is_some_and(|u| u != units)
            {
                return Err("Human: entry size exceeds permit".into());
            }
        } else if self.intent.units != Some(units) {
            return Err("Human: exit size does not match permit".into());
        }
        self.intent.units = Some(units);
        Ok(self.intent)
    }
}

pub struct Journal<'a> {
    path: PathBuf,
    env: Environment,
    plan: &'a Plan,
    forbidden_hashes: HashSet<String>,
}
impl<'a> Journal<'a> {
    pub fn new(
        path: impl AsRef<Path>,
        env: Environment,
        plan: &'a Plan,
        forbidden_hashes: HashSet<String>,
    ) -> Self {
        Self {
            path: path.as_ref().to_owned(),
            env,
            plan,
            forbidden_hashes,
        }
    }
    pub(crate) fn plan(&self) -> &Plan {
        self.plan
    }
    pub(crate) fn environment(&self) -> Environment {
        self.env
    }
    pub fn validate_row(&self, row: &Row, existing: &[Row]) -> Result<(), String> {
        self.validate(row, existing, true)
    }
    fn validate(&self, row: &Row, existing: &[Row], current_plan: bool) -> Result<(), String> {
        if row.version != 1 || row.env != self.env {
            return Err("invalid version or environment".into());
        }
        if row.leg.trim().is_empty()
            || (current_plan && !self.plan.legs.iter().any(|leg| leg.name == row.leg))
        {
            return Err("invalid leg".into());
        }
        let date =
            Date::parse(&row.tokyo_date, &Iso8601::DATE).map_err(|_| "invalid event date")?;
        let day = format!(
            "{:04}{:02}{:02}",
            date.year(),
            u8::from(date.month()),
            date.day()
        );
        if row.event_id != format!("{}-{day}", row.leg) {
            return Err("event id mismatch".into());
        }
        let prefix = format!("{}-{day}-", row.leg);
        let rest = row.idem_key.strip_prefix(&prefix).ok_or("invalid key")?;
        let entry = format!("ENTRY-{}", self.env.name());
        let exit = format!("EXIT-{}", self.env.name());
        if rest != entry && rest != exit && !(1..=3).any(|n| rest == format!("{exit}-{n}")) {
            return Err("invalid key".into());
        }
        if let Some(reason) = &row.reason {
            let normalized: String = reason
                .chars()
                .filter(|c| !c.is_ascii_whitespace() && *c != '-' && *c != '_')
                .flat_map(char::to_lowercase)
                .collect();
            if normalized.contains("formatcheck") {
                return Err("format check forbidden".into());
            }
        }
        if let Some(hash) = &row.source_sha256
            && (!valid_hash(hash) || self.forbidden_hashes.contains(hash))
        {
            return Err("invalid source hash".into());
        }
        timestamp(&row.recorded_at)?;
        if let Some(value) = &row.declared_at {
            timestamp(value)?;
        }
        if let Some(value) = &row.fill_time {
            timestamp(value)?;
        }
        if filled(row.status) {
            if !matches!(
                row.action,
                Action::Entry | Action::Exit | Action::StopExit | Action::ManualClose
            ) {
                return Err("invalid fill action".into());
            }
            if current_plan && row.action == Action::Entry && !self.plan.event_dates.contains(&date)
            {
                return Err("entry date not in plan".into());
            }
            if row.order_id.as_ref().is_none_or(|v| v.trim().is_empty())
                || row.source_sha256.is_none()
                || row.fill_time.is_none()
                || row.fill_price.is_none_or(|v| v.0 <= 0)
                || row.units.is_none_or(|v| v <= 0)
                || row.side.is_none()
            {
                return Err("missing fill fields".into());
            }
            if row.action != Action::Entry && row.realized_pnl_jpy_ex_swap.is_none() {
                return Err("missing realized pnl".into());
            }
        }
        if let Some(target) = &row.correction_of {
            if target != &row.idem_key || !existing.iter().any(|prior| prior.idem_key == *target) {
                return Err("invalid correction target".into());
            }
        } else {
            for prior in existing
                .iter()
                .filter(|p| p.correction_of.is_none() && p.idem_key == row.idem_key)
            {
                if filled(row.status) && filled(prior.status) {
                    return Err("duplicate fill".into());
                }
                if prior.action == row.action {
                    return Err("duplicate key and action".into());
                }
            }
        }
        Ok(())
    }
    fn read_locked(&self, file: &mut File) -> Result<Vec<Row>, String> {
        file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
        let mut data = String::new();
        file.read_to_string(&mut data).map_err(|e| e.to_string())?;
        if !data.is_empty() && !data.ends_with('\n') {
            return Err("incomplete journal line".into());
        }
        let mut rows = Vec::new();
        for line in data.lines() {
            if line.trim().is_empty() {
                return Err("empty journal line".into());
            }
            let row: Row = serde_json::from_str(line).map_err(|e| e.to_string())?;
            self.validate(&row, &rows, false)?;
            rows.push(row);
        }
        Ok(rows)
    }
    pub fn read_or_empty(&self) -> Result<Vec<Row>, String> {
        match self.path.try_exists() {
            Ok(false) => Ok(Vec::new()),
            Ok(true) => self.read(),
            Err(e) => Err(e.to_string()),
        }
    }
    pub fn read(&self) -> Result<Vec<Row>, String> {
        let mut file = File::open(&self.path).map_err(|e| e.to_string())?;
        file.lock().map_err(|e| e.to_string())?;
        self.read_locked(&mut file)
    }
    pub fn append(&self, row: &Row) -> Result<(), String> {
        self.append_checked(row, false)
    }
    /// Persists a scheduled declaration carrying the order key and side.
    /// A later fill can then use its trade action without duplicating the declaration.
    pub fn begin(&self, intent: Row) -> Result<Permit, String> {
        if intent.status != Status::Scheduled
            || !matches!(intent.action, Action::Entry | Action::Exit)
            || intent.correction_of.is_some()
            || intent.units.is_some_and(|v| v <= 0)
            || (intent.action == Action::Exit && intent.units.is_none())
            || intent.side.is_none()
        {
            return Err("invalid order intent".into());
        }
        if intent.order_id.is_some()
            || intent.fill_time.is_some()
            || intent.fill_price.is_some()
            || intent.source_sha256.is_some()
            || intent.realized_pnl_jpy_ex_swap.is_some()
        {
            return Err("invalid order intent".into());
        }
        if intent.action == Action::Entry {
            let leg = self
                .plan
                .legs
                .iter()
                .find(|leg| leg.name == intent.leg)
                .ok_or("invalid leg")?;
            let expected_side = match leg.direction {
                crate::plan::Direction::Buy => Side::Buy,
                crate::plan::Direction::Sell => Side::Sell,
            };
            if intent.side != Some(expected_side) {
                return Err("entry direction does not match plan".into());
            }
            let date = Date::parse(&intent.tokyo_date, &Iso8601::DATE)
                .map_err(|_| "invalid event date")?;
            if !self.plan.event_dates.contains(&date) {
                return Err("entry date not in plan".into());
            }
        }
        let suffix = match intent.action {
            Action::Entry => format!("-ENTRY-{}", self.env.name()),
            Action::Exit => format!("-EXIT-{}", self.env.name()),
            _ => return Err("invalid order intent".into()),
        };
        let valid_key = match intent.action {
            Action::Entry => intent.idem_key.ends_with(&suffix),
            Action::Exit => {
                intent.idem_key.ends_with(&suffix)
                    || (1..=3).any(|i| intent.idem_key.ends_with(&format!("{suffix}-{i}")))
            }
            _ => false,
        };
        if !valid_key {
            return Err("intent key does not match action".into());
        }
        if self.plan.sizing.lot_size <= 0
            || self.plan.sizing.unit_cap <= 0
            || self.plan.sizing.absolute_cap <= 0
        {
            return Err("invalid sizing limits".into());
        }
        let mut recorded = intent.clone();
        recorded.action = Action::Declare;
        self.append_checked(&recorded, true)?;
        Ok(Permit {
            intent,
            max_units: self.plan.sizing.unit_cap.min(self.plan.sizing.absolute_cap),
            lot_size: self.plan.sizing.lot_size,
        })
    }
    fn append_checked(&self, row: &Row, new_key: bool) -> Result<(), String> {
        let (mut file, _created) = match OpenOptions::new()
            .read(true)
            .append(true)
            .create_new(true)
            .open(&self.path)
        {
            Ok(f) => (f, true),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (
                OpenOptions::new()
                    .read(true)
                    .append(true)
                    .open(&self.path)
                    .map_err(|e| e.to_string())?,
                false,
            ),
            Err(e) => return Err(e.to_string()),
        };
        file.lock().map_err(|e| e.to_string())?;
        let existing = self.read_locked(&mut file)?;
        if new_key && existing.iter().any(|prior| prior.idem_key == row.idem_key) {
            return Err("intent key already used".into());
        }
        self.validate_row(row, &existing)?;
        let encoded = serde_json::to_vec(row).map_err(|e| e.to_string())?;
        file.write_all(&encoded)
            .and_then(|()| file.write_all(b"\n"))
            .and_then(|()| file.sync_all())
            .map_err(|e| e.to_string())?;
        let parent = self
            .path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        File::open(parent)
            .and_then(|dir| dir.sync_all())
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::tests::{VALID, read};
    use serde_json::json;
    use std::{fs, thread};

    fn plan() -> Plan {
        read(VALID, 0o600).expect("synthetic plan")
    }
    fn row() -> Row {
        serde_json::from_value(json!({"version":1,"env":"SIM","event_id":"A-20980101","leg":"A","tokyo_date":"2098-01-01","action":"entry","idem_key":"A-20980101-ENTRY-SIM","status":"filled","recorded_at":"2098-01-01T03:15:00+01:00","order_id":"synthetic","side":"Buy","units":10,"fill_time":"2098-01-01T03:15:00+01:00","fill_price":12.345,"source_sha256":"a".repeat(64)})).expect("row")
    }
    fn journal<'a>(path: &Path, plan: &'a Plan) -> Journal<'a> {
        Journal::new(path, Environment::Sim, plan, HashSet::new())
    }
    #[test]
    fn test_env_must_be_live() {
        let p = plan();
        let j = Journal::new("unused", Environment::Live, &p, HashSet::new());
        assert_eq!(
            j.validate_row(&row(), &[]).expect_err("env"),
            "invalid version or environment"
        );
    }
    #[test]
    fn test_env_required() {
        let mut v = serde_json::to_value(row()).expect("value");
        v.as_object_mut().expect("object").remove("env");
        assert!(
            serde_json::from_value::<Row>(v)
                .expect_err("missing env")
                .to_string()
                .contains("missing field")
        );
    }
    #[test]
    fn test_idem_mock_sim_forbidden() {
        let p = plan();
        let j = journal(Path::new("unused"), &p);
        let mut r = row();
        r.idem_key = "A-20980101-ENTRY-LIVE".into();
        assert_eq!(j.validate_row(&r, &[]).expect_err("key"), "invalid key");
        r.idem_key = "A-20980101-ENTRY-MOCK".into();
        assert!(j.validate_row(&r, &[]).is_err());
    }
    #[test]
    fn test_fill_requires_order_id_and_hash() {
        let p = plan();
        let j = journal(Path::new("unused"), &p);
        let mut r = row();
        r.order_id = None;
        assert_eq!(
            j.validate_row(&r, &[]).expect_err("id"),
            "missing fill fields"
        );
        r.order_id = Some("x".into());
        r.source_sha256 = Some("abc".into());
        assert_eq!(
            j.validate_row(&r, &[]).expect_err("sha"),
            "invalid source hash"
        );
    }
    #[test]
    fn test_forbidden_hash() {
        let p = plan();
        let j = Journal::new(
            "unused",
            Environment::Sim,
            &p,
            HashSet::from(["a".repeat(64)]),
        );
        assert_eq!(
            j.validate_row(&row(), &[]).expect_err("hash"),
            "invalid source hash"
        );
    }
    #[test]
    fn test_non_event_entry_fails() {
        let p = plan();
        let j = journal(Path::new("unused"), &p);
        let mut r = row();
        r.tokyo_date = "2098-01-02".into();
        r.event_id = "A-20980102".into();
        r.idem_key = "A-20980102-ENTRY-SIM".into();
        assert_eq!(
            j.validate_row(&r, &[]).expect_err("date"),
            "entry date not in plan"
        );
    }
    #[test]
    fn test_format_check_reason_fails() {
        let p = plan();
        let j = journal(Path::new("unused"), &p);
        let mut r = row();
        r.reason = Some("FORMAT-check".into());
        assert_eq!(
            j.validate_row(&r, &[]).expect_err("reason"),
            "format check forbidden"
        );
    }
    #[test]
    fn test_duplicate_idem_fill() {
        let p = plan();
        let j = journal(Path::new("unused"), &p);
        let r = row();
        assert_eq!(
            j.validate_row(&r, std::slice::from_ref(&r))
                .expect_err("duplicate"),
            "duplicate fill"
        );
    }
    #[test]
    fn test_grok_c_1_duplicate_fill_different_actions() {
        let p = plan();
        let j = journal(Path::new("unused"), &p);
        let first = row();
        let mut second = row();
        second.action = Action::StopExit;
        second.realized_pnl_jpy_ex_swap = Some(Milli(0));
        assert_eq!(
            j.validate_row(&second, &[first]).expect_err("duplicate"),
            "duplicate fill"
        );
    }
    #[test]
    fn test_close_requires_realized_pnl() {
        let p = plan();
        let j = journal(Path::new("unused"), &p);
        let mut r = row();
        r.action = Action::Exit;
        r.idem_key = format!("{}-EXIT-SIM", r.event_id);
        assert_eq!(
            j.validate_row(&r, &[]).expect_err("pnl"),
            "missing realized pnl"
        );
    }
    #[test]
    fn test_grok_b1_06_duplicate_scheduled_same_key() {
        let p = plan();
        let j = journal(Path::new("unused"), &p);
        let mut r = row();
        r.action = Action::Declare;
        r.status = Status::Scheduled;
        assert_eq!(
            j.validate_row(&r, std::slice::from_ref(&r))
                .expect_err("duplicate"),
            "duplicate key and action"
        );
    }
    #[test]
    fn test_grok_d_4_correction_target_must_exist_and_match_key() {
        let p = plan();
        let j = journal(Path::new("unused"), &p);
        let mut r = row();
        r.correction_of = Some(r.idem_key.clone());
        assert_eq!(
            j.validate_row(&r, &[]).expect_err("absent"),
            "invalid correction target"
        );
        let first = row();
        assert!(j.validate_row(&r, std::slice::from_ref(&first)).is_ok());
        r.correction_of = Some("different".into());
        assert_eq!(
            j.validate_row(&r, &[first]).expect_err("mismatch"),
            "invalid correction target"
        );
    }
    #[test]
    fn test_append_only_preserves_prefix() {
        let p = plan();
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("journal");
        let j = journal(&path, &p);
        let mut declared = row();
        declared.action = Action::Declare;
        declared.status = Status::Scheduled;
        j.append(&declared).expect("append");
        let before = fs::read(&path).expect("read");
        j.append(&row()).expect("fill");
        let after = fs::read(&path).expect("read");
        assert!(after.starts_with(&before));
        assert!(
            String::from_utf8(after)
                .expect("utf8")
                .contains("\"fill_price\":12.345")
        );
    }
    #[test]
    fn test_grok_b1_08_append_under_lock_single_fill() {
        let p = plan();
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("journal");
        let j = journal(&path, &p);
        let r = row();
        thread::scope(|scope| {
            let handles: Vec<_> = (0..8).map(|_| scope.spawn(|| j.append(&r))).collect();
            let successes = handles
                .into_iter()
                .map(|h| h.join().expect("thread"))
                .filter(Result::is_ok)
                .count();
            assert_eq!(successes, 1);
        });
        assert_eq!(fs::read_to_string(path).expect("read").lines().count(), 1);
    }
    #[test]
    fn corrupted_file_is_never_appended() {
        let p = plan();
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("journal");
        let bytes = b"{broken}\n";
        fs::write(&path, bytes).expect("write");
        let j = journal(&path, &p);
        assert!(j.append(&row()).is_err());
        assert_eq!(fs::read(&path).expect("read"), bytes);
    }
    #[test]
    fn existing_invalid_row_is_never_appended() {
        let p = plan();
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("journal");
        let mut invalid = row();
        invalid.env = Environment::Live;
        let bytes = format!("{}\n", serde_json::to_string(&invalid).expect("json"));
        fs::write(&path, &bytes).expect("write");
        assert_eq!(
            journal(&path, &p)
                .append(&row())
                .expect_err("invalid existing"),
            "invalid version or environment"
        );
        assert_eq!(fs::read_to_string(path).expect("read"), bytes);
    }
    #[test]
    fn money_precision_and_unknown_fields() {
        let r = row();
        assert_eq!(r.fill_price, Some(Milli(12345)));
        let mut v = serde_json::to_value(r).expect("value");
        v["fill_price"] = json!(1.2345);
        assert!(serde_json::from_value::<Row>(v.clone()).is_err());
        v["fill_price"] = json!(1);
        v["unexpected"] = json!(true);
        assert!(serde_json::from_value::<Row>(v).is_err());
    }
    #[test]
    fn rotated_plan_validates_old_rows_without_old_dates_or_legs() {
        let old = plan();
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("journal");
        journal(&path, &old).append(&row()).expect("old fill");
        let text = VALID
            .replace("2098-01-01", "2098-01-02")
            .replace("name = \"A\"", "name = \"B\"");
        let new = read(&text, 0o600).expect("new plan");
        let j = journal(&path, &new);
        assert_eq!(j.read().expect("old history").len(), 1);
        let mut next = row();
        next.leg = "B".into();
        next.tokyo_date = "2098-01-02".into();
        next.event_id = "B-20980102".into();
        next.idem_key = format!("{}-ENTRY-SIM", next.event_id);
        j.append(&next).expect("new fill");
        let mut not_event = next;
        not_event.tokyo_date = "2098-01-01".into();
        not_event.event_id = "B-20980101".into();
        not_event.idem_key = format!("{}-ENTRY-SIM", not_event.event_id);
        assert_eq!(
            j.append(&not_event).expect_err("old date"),
            "entry date not in plan"
        );
    }
    fn intent() -> Row {
        let mut r = row();
        r.status = Status::Scheduled;
        r.order_id = None;
        r.fill_time = None;
        r.fill_price = None;
        r.source_sha256 = None;
        r
    }
    #[test]
    fn test_idempotency_retry_restart() {
        let p = plan();
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("journal");
        let j = journal(&path, &p);
        let first = intent();
        let permit = j.begin(first.clone()).expect("permit");
        assert_eq!(
            permit
                .authorize(&first.idem_key, &first.leg, Side::Buy, 10)
                .expect("authorize")
                .idem_key,
            first.idem_key
        );
        let persisted = j.read().expect("read");
        assert_eq!(persisted.len(), 1);
        assert_eq!(persisted[0].action, Action::Declare);
        assert_eq!(persisted[0].status, Status::Scheduled);
        let restarted = journal(&path, &p);
        assert!(matches!(restarted.begin(first.clone()),Err(e) if e=="intent key already used"));
        j.append(&row()).expect("subsequent fill");
        assert!(matches!(restarted.begin(first),Err(e) if e=="intent key already used"));
    }
    #[test]
    fn test_t8_restart_no_duplicate_keys() {
        let p = plan();
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("journal");
        let j = journal(&path, &p);
        thread::scope(|scope| {
            let handles: Vec<_> = (0..8).map(|_| scope.spawn(|| j.begin(intent()))).collect();
            let successes = handles
                .into_iter()
                .map(|h| h.join().expect("join"))
                .filter(Result::is_ok)
                .count();
            assert_eq!(successes, 1);
        });
        assert_eq!(journal(&path, &p).read().expect("read").len(), 1);
        assert!(matches!(journal(&path,&p).begin(intent()),Err(e) if e=="intent key already used"));
    }
    #[test]
    fn failed_begin_issues_no_permit_and_preserves_bytes() {
        let p = plan();
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("journal");
        fs::write(&path, b"broken\n").expect("write");
        assert!(journal(&path, &p).begin(intent()).is_err());
        assert_eq!(fs::read(path).expect("read"), b"broken\n");
    }
    #[test]
    fn existing_declaration_blocks_restart_without_changing_bytes() {
        let p = plan();
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("journal");
        let j = journal(&path, &p);
        let mut declared = intent();
        declared.action = Action::Declare;
        declared.units = None;
        declared.side = None;
        j.append(&declared).expect("declare");
        let before = fs::read(&path).expect("read");
        let restarted = journal(&path, &p);
        assert!(matches!(restarted.begin(intent()),Err(e) if e=="intent key already used"));
        assert_eq!(fs::read(path).expect("read"), before);
    }
    #[test]
    fn test_exit_fill_next_day() {
        let p = plan();
        let j = journal(Path::new("unused"), &p);
        let mut r = row();
        r.action = Action::Exit;
        r.idem_key = format!("{}-EXIT-SIM-1", r.event_id);
        r.side = Some(Side::Sell);
        r.realized_pnl_jpy_ex_swap = Some(Milli(0));
        r.fill_time = Some("2098-01-02T04:00:00+01:00".into());
        assert!(j.validate_row(&r, &[]).is_ok());
    }
    #[test]
    fn test_halt_key_rejected() {
        let p = plan();
        let j = journal(Path::new("unused"), &p);
        let mut r = row();
        r.action = Action::Halt;
        r.status = Status::Halted;
        r.idem_key = format!("{}-HALT-SIM", r.event_id);
        assert_eq!(j.validate_row(&r, &[]).expect_err("key"), "invalid key");
    }
    #[test]
    fn test_declare_fill_and_skipped_forbidden_hash() {
        let p = plan();
        let j = Journal::new(
            "unused",
            Environment::Sim,
            &p,
            HashSet::from(["a".repeat(64)]),
        );
        let mut declare = row();
        declare.action = Action::Declare;
        assert_eq!(
            j.validate_row(&declare, &[]).expect_err("fill"),
            "invalid source hash"
        );
        declare.source_sha256 = Some("b".repeat(64));
        assert_eq!(
            j.validate_row(&declare, &[]).expect_err("fill"),
            "invalid fill action"
        );
        let mut skipped = row();
        skipped.status = Status::Skipped;
        assert_eq!(
            j.validate_row(&skipped, &[]).expect_err("hash"),
            "invalid source hash"
        );
    }
    fn full_day(path: &Path, stop_exit: bool, env: Environment) {
        let config = if stop_exit {
            VALID.to_owned()
        } else {
            VALID
                .replace("enabled = true", "enabled = false")
                .replace("stop = 1", "stop = 0")
        };
        let p = read(&config, 0o600).expect("plan");
        let j = Journal::new(path, env, &p, HashSet::new());
        let mut entry_intent = intent();
        entry_intent.env = env;
        entry_intent.idem_key = format!("{}-ENTRY-{}", entry_intent.event_id, env.name());
        let _entry = j.begin(entry_intent.clone()).expect("entry permit");
        let rows = j.read().expect("declaration");
        assert_eq!(rows[0].action, Action::Declare);
        assert_eq!(rows[0].status, Status::Scheduled);
        assert!(matches!(j.begin(entry_intent.clone()),Err(e) if e=="intent key already used"));
        let pending = j
            .replay(&std::collections::HashMap::new())
            .expect("pending");
        assert_eq!(pending.orders.len(), 1);
        assert_eq!(pending.orders[0].state, crate::decide::OrderState::Unknown);
        let mut entry_fill = row();
        entry_fill.env = env;
        entry_fill.idem_key = entry_intent.idem_key;
        j.append(&entry_fill).expect("entry fill");
        let mut exit = intent();
        exit.env = env;
        exit.action = Action::Exit;
        exit.side = Some(Side::Sell);
        exit.idem_key = format!("{}-EXIT-{}-1", exit.event_id, env.name());
        if !stop_exit {
            let _close = j.begin(exit.clone()).expect("exit permit");
        }
        exit.action = if stop_exit {
            Action::StopExit
        } else {
            Action::Exit
        };
        exit.status = Status::Filled;
        exit.order_id = Some("synthetic-close".into());
        exit.fill_time = Some(exit.recorded_at.clone());
        exit.fill_price = Some(Milli(12345));
        exit.source_sha256 = Some("a".repeat(64));
        exit.realized_pnl_jpy_ex_swap = Some(Milli(250));
        j.append(&exit).expect("exit fill");
        let restarted = Journal::new(path, env, &p, HashSet::new())
            .replay(&std::collections::HashMap::new())
            .expect("replay");
        assert!(restarted.positions.is_empty());
        assert_eq!(restarted.realized_milli, 250);
    }
    #[test]
    fn full_day_declare_begin_fill_close_and_restart() {
        let dir = tempfile::tempdir().expect("dir");
        full_day(&dir.path().join("regular"), false, Environment::Sim);
        full_day(&dir.path().join("stop"), true, Environment::Sim);
    }
    #[test]
    #[ignore]
    fn write_synthetic_sample() {
        let path = std::env::var_os("FORWARD_SAMPLE_PATH").expect("sample path required");
        let path = PathBuf::from(path);
        assert!(!path.exists(), "sample already exists");
        full_day(&path, false, Environment::Live);
    }
    #[test]
    fn empty_entry_size_is_bound_at_authorization() {
        let p = plan();
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("journal");
        let j = journal(&path, &p);
        let mut entry = intent();
        entry.units = None;
        let key = entry.idem_key.clone();
        let permit = j.begin(entry).expect("begin");
        let rows = j.read().expect("validate");
        assert_eq!(rows[0].units, None);
        let authorized = permit.authorize(&key, "A", Side::Buy, 20).expect("size");
        assert_eq!(authorized.units, Some(20));
    }
    #[test]
    fn predeclared_entry_rejects_excess_or_unaligned_size() {
        let p = plan();
        let dir = tempfile::tempdir().expect("dir");
        for (i, units) in [110, 11, 0].into_iter().enumerate() {
            let path = dir.path().join(i.to_string());
            let j = journal(&path, &p);
            let mut entry = intent();
            entry.units = None;
            let key = entry.idem_key.clone();
            let permit = j.begin(entry).expect("begin");
            assert!(
                permit
                    .authorize(&key, "A", Side::Buy, units)
                    .expect_err("size")
                    .contains("Human")
            );
            assert_eq!(j.read().expect("read").len(), 1);
        }
    }
}
