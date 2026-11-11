use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::{self, BufRead, BufReader, Write},
    path::Path,
};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::{Date, Duration, OffsetDateTime};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    expires_on: Date,
    symbols: Vec<String>,
    max_position_shares: i64,
    max_daily_orders: u64,
    max_daily_loss_usd: f64,
}

#[derive(Debug)]
pub struct Limits(Config);

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("config file is missing")]
    Missing,
    #[error("cannot read config or kill switch metadata: {0}")]
    Io(#[from] io::Error),
    #[error("invalid TOML: {0}")]
    Parse(toml::de::Error),
    #[error("invalid config schema: {0}")]
    Schema(toml::de::Error),
    #[error("symbols must be nonempty valid identifiers")]
    Symbols,
    #[error("limits must be positive and finite")]
    InvalidLimit,
    #[error("config has expired")]
    Expired,
    #[error("config expires more than 31 days from today")]
    TooFar,
    #[error("kill switch exists")]
    Killed,
}

impl Limits {
    pub fn load(path: impl AsRef<Path>, today: Date) -> Result<Self, ConfigError> {
        let path = path.as_ref();
        match fs::symlink_metadata(path.with_file_name("KILL")) {
            Ok(_) => return Err(ConfigError::Killed),
            Err(e) if e.kind() == io::ErrorKind::NotFound => (),
            Err(e) => return Err(e.into()),
        }
        let text = fs::read_to_string(path).map_err(|e| {
            if e.kind() == io::ErrorKind::NotFound {
                ConfigError::Missing
            } else {
                ConfigError::Io(e)
            }
        })?;
        let value: toml::Value = toml::from_str(&text).map_err(ConfigError::Parse)?;
        let limits: Config = value.try_into().map_err(ConfigError::Schema)?;
        if limits.symbols.is_empty() || limits.symbols.iter().any(|s| !identifier(s)) {
            return Err(ConfigError::Symbols);
        }
        if limits.max_position_shares <= 0
            || limits.max_daily_orders == 0
            || !limits.max_daily_loss_usd.is_finite()
            || limits.max_daily_loss_usd <= 0.0
        {
            return Err(ConfigError::InvalidLimit);
        }
        if today > limits.expires_on {
            return Err(ConfigError::Expired);
        }
        if limits.expires_on - today > Duration::days(31) {
            return Err(ConfigError::TooFar);
        }
        Ok(Self(limits))
    }
}

fn identifier(value: &str) -> bool {
    !value.is_empty() && !value.chars().any(|c| c == ':' || c.is_whitespace())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    Buy,
    Sell,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Order {
    day: Date,
    strategy: String,
    symbol: String,
    side: Side,
    shares: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Intent(Order);

#[derive(Debug, Error, PartialEq, Eq)]
pub enum IntentError {
    #[error("strategy must be nonempty and contain neither whitespace nor colon")]
    Strategy,
    #[error("symbol must be nonempty and contain neither whitespace nor colon")]
    Symbol,
    #[error("shares must be at least one")]
    Shares,
}

impl Intent {
    // shortcut: UTC dates match US regular-hours trading days (14:30–21:00 UTC in winter); use exchange-calendar dates before adding extended hours.
    pub fn new(
        day: Date,
        strategy: &str,
        symbol: &str,
        side: Side,
        shares: u32,
    ) -> Result<Self, IntentError> {
        if !identifier(strategy) {
            return Err(IntentError::Strategy);
        }
        if !identifier(symbol) {
            return Err(IntentError::Symbol);
        }
        if shares == 0 {
            return Err(IntentError::Shares);
        }
        Ok(Self(Order {
            day,
            strategy: strategy.into(),
            symbol: symbol.into(),
            side,
            shares,
        }))
    }

    pub fn day(&self) -> Date {
        self.0.day
    }
    pub fn strategy(&self) -> &str {
        &self.0.strategy
    }
    pub fn symbol(&self) -> &str {
        &self.0.symbol
    }
    pub fn side(&self) -> Side {
        self.0.side
    }
    pub fn shares(&self) -> u32 {
        self.0.shares
    }

    pub fn key(&self) -> String {
        let side = match self.0.side {
            Side::Buy => "buy",
            Side::Sell => "sell",
        };
        format!(
            "{}:{}:{}:{side}",
            self.0.day, self.0.strategy, self.0.symbol
        )
    }
}

#[derive(Debug)]
pub struct Account {
    pub position: i64,
    pub daily_pnl_usd: f64,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum Reject {
    #[error("symbol is not allowed")]
    Symbol,
    #[error("account daily profit/loss is not finite")]
    NonFinitePnl,
    #[error("daily order cap reached")]
    OrderCap,
    #[error("position arithmetic overflow")]
    Overflow,
    #[error("resulting position would be short")]
    Short,
    #[error("resulting position exceeds the limit")]
    PositionCap,
    #[error("daily loss limit reached; only reducing orders are allowed")]
    DailyLoss,
}

#[derive(Debug)]
pub struct Approved(Intent);

pub fn check(
    limits: &Limits,
    account: &Account,
    orders_today: u64,
    intent: &Intent,
) -> Result<Approved, Reject> {
    if !limits.0.symbols.contains(&intent.0.symbol) {
        return Err(Reject::Symbol);
    }
    if !account.daily_pnl_usd.is_finite() {
        return Err(Reject::NonFinitePnl);
    }
    if orders_today >= limits.0.max_daily_orders {
        return Err(Reject::OrderCap);
    }
    let shares = i64::from(intent.0.shares);
    let position = match intent.0.side {
        Side::Buy => account.position.checked_add(shares),
        Side::Sell => account.position.checked_sub(shares),
    }
    .ok_or(Reject::Overflow)?;
    if position < 0 {
        return Err(Reject::Short);
    }
    if position > limits.0.max_position_shares {
        return Err(Reject::PositionCap);
    }
    if account.daily_pnl_usd <= -limits.0.max_daily_loss_usd
        && position.unsigned_abs() >= account.position.unsigned_abs()
    {
        return Err(Reject::DailyLoss);
    }
    Ok(Approved(intent.clone()))
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    key: String,
    #[serde(flatten)]
    intent: Order,
    #[serde(with = "time::serde::rfc3339")]
    recorded_at: OffsetDateTime,
}

#[derive(Debug)]
pub struct Recorded {
    key: String,
    intent: Intent,
}

impl Recorded {
    pub fn key(&self) -> &str {
        &self.key
    }

    pub fn intent(&self) -> &Intent {
        &self.intent
    }
}

#[derive(Debug, Error)]
pub enum JournalError {
    #[error("journal is already locked")]
    Locked,
    #[error("journal IO failed: {0}")]
    Io(#[from] io::Error),
    #[error("journal is corrupt")]
    Corrupt,
    #[error("order key was already recorded")]
    Duplicate,
    #[error("journal write failed earlier; reopen and inspect before retrying")]
    Poisoned,
}

#[derive(Debug)]
pub struct Journal {
    file: File,
    records: Vec<Entry>,
    keys: HashSet<String>,
    poisoned: bool,
}

impl Journal {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, JournalError> {
        let path = path.as_ref();
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(path)?;
        match file.try_lock() {
            Ok(()) => (),
            Err(std::fs::TryLockError::WouldBlock) => return Err(JournalError::Locked),
            Err(std::fs::TryLockError::Error(e)) => return Err(e.into()),
        }
        let mut records = Vec::new();
        let mut keys = HashSet::new();
        let mut reader = BufReader::new(&file);
        let mut line = Vec::new();
        while reader.read_until(b'\n', &mut line)? != 0 {
            if !line.ends_with(b"\n") {
                return Err(JournalError::Corrupt);
            }
            let recorded: Entry =
                serde_json::from_slice(&line).map_err(|_| JournalError::Corrupt)?;
            let i = &recorded.intent;
            if Intent::new(i.day, &i.strategy, &i.symbol, i.side, i.shares).is_err()
                || recorded.key != Intent(i.clone()).key()
                || recorded.recorded_at.offset() != time::UtcOffset::UTC
                || !keys.insert(recorded.key.clone())
            {
                return Err(JournalError::Corrupt);
            }
            records.push(recorded);
            line.clear();
        }
        // Persist the directory entry as well as order data before any Recorded can escape.
        File::open(path.with_file_name("."))?.sync_all()?;
        Ok(Self {
            file,
            records,
            keys,
            poisoned: false,
        })
    }

    pub fn orders_on(&self, day: Date) -> u64 {
        self.records.iter().filter(|r| r.intent.day == day).count() as u64
    }

    pub fn record(&mut self, approved: Approved) -> Result<Recorded, JournalError> {
        if self.poisoned {
            return Err(JournalError::Poisoned);
        }
        let key = approved.0.key();
        if self.keys.contains(&key) {
            return Err(JournalError::Duplicate);
        }
        let recorded = Entry {
            key,
            intent: approved.0.0,
            recorded_at: OffsetDateTime::now_utc(),
        };
        let mut line = serde_json::to_vec(&recorded).map_err(|_| JournalError::Corrupt)?;
        line.push(b'\n');
        // A failed write or fsync may still have reached disk; this handle must not retry.
        self.poisoned = true;
        self.file.write_all(&line)?;
        self.file.sync_all()?;
        self.keys.insert(recorded.key.clone());
        self.records.push(Entry {
            key: recorded.key.clone(),
            intent: recorded.intent.clone(),
            recorded_at: recorded.recorded_at,
        });
        self.poisoned = false;
        Ok(Recorded {
            intent: Intent(recorded.intent.clone()),
            key: recorded.key,
        })
    }
}

#[cfg(test)]
mod tests;
