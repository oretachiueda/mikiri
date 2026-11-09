use sha2::{Digest, Sha256};
use std::{
    fmt, fs,
    io::Write,
    path::Path,
    process::{Command, Stdio},
};
use time::{Date, OffsetDateTime, format_description::well_known::Rfc3339};

#[derive(Debug, PartialEq)]
pub struct DailyBar {
    pub date: Date,
    pub close: f64,
    pub div_cash: f64,
    pub split_factor: f64,
}

#[derive(Debug, PartialEq)]
pub struct ParseError {
    /// One-based array position; None identifies the response itself.
    pub element: Option<usize>,
    pub reason: &'static str,
}
impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.element {
            Some(i) => write!(f, "element {i}: {}", self.reason),
            None => write!(f, "response: {}", self.reason),
        }
    }
}
impl std::error::Error for ParseError {}

pub fn parse_daily_bars(raw: &[u8]) -> Result<Vec<DailyBar>, ParseError> {
    let error = |reason| ParseError {
        element: None,
        reason,
    };
    let value: serde_json::Value =
        serde_json::from_slice(raw).map_err(|_| error("invalid JSON"))?;
    let rows = value.as_array().ok_or_else(|| error("expected array"))?;
    if rows.is_empty() {
        return Err(error("empty array"));
    }
    let mut bars: Vec<DailyBar> = Vec::with_capacity(rows.len());
    for (index, row) in rows.iter().enumerate() {
        let fail = |reason| ParseError {
            element: Some(index + 1),
            reason,
        };
        let date = row
            .get("date")
            .and_then(|v| v.as_str())
            .and_then(|s| OffsetDateTime::parse(s, &Rfc3339).ok())
            .filter(|d| d.offset() == time::UtcOffset::UTC && d.time() == time::Time::MIDNIGHT)
            .ok_or_else(|| fail("date must be an RFC3339 UTC midnight"))?
            .date();
        let number = |name, reason| {
            row.get(name)
                .and_then(|v| v.as_f64())
                .filter(|v| v.is_finite())
                .ok_or_else(|| fail(reason))
        };
        let close = number("close", "missing or invalid close")?;
        let div_cash = number("divCash", "missing or invalid divCash")?;
        let split_factor = number("splitFactor", "missing or invalid splitFactor")?;
        if close <= 0.0 {
            return Err(fail("close must be positive"));
        }
        if div_cash < 0.0 {
            return Err(fail("divCash must be nonnegative"));
        }
        if split_factor <= 0.0 {
            return Err(fail("splitFactor must be positive"));
        }
        if bars.last().is_some_and(|previous| previous.date >= date) {
            return Err(fail("dates must be strictly increasing (no duplicates)"));
        }
        bars.push(DailyBar {
            date,
            close,
            div_cash,
            split_factor,
        });
    }
    Ok(bars)
}

#[derive(Debug)]
pub struct FetchError(&'static str);
impl fmt::Display for FetchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for FetchError {}

pub fn fetch_daily_bars(
    base_url: &str,
    ticker: &str,
    start: Date,
    token_file: &Path,
    out: &Path,
) -> Result<String, FetchError> {
    if ticker.is_empty()
        || !ticker
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
    {
        return Err(FetchError("invalid ticker"));
    }
    let token = fs::read_to_string(token_file).map_err(|_| FetchError("cannot read token file"))?;
    let token = token.trim();
    if token.is_empty() || token.chars().any(char::is_control) {
        return Err(FetchError("invalid token"));
    }
    let url = format!(
        "{}/tiingo/daily/{ticker}/prices?startDate={start}&format=json",
        base_url.trim_end_matches('/')
    );
    let mut child = Command::new("curl")
        .args([
            "--disable",
            "--silent",
            "--fail",
            "--connect-timeout",
            "10",
            "--max-time",
            "120",
            "--proto",
            "=http,https",
            "-H",
            "@-",
            "--url",
            &url,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| FetchError("cannot start curl"))?;
    let sent = match child.stdin.take() {
        Some(mut stdin) => writeln!(stdin, "Authorization: Token {token}"),
        None => Err(std::io::Error::other("missing curl stdin")),
    };
    let output = child
        .wait_with_output()
        .map_err(|_| FetchError("cannot wait for curl"))?;
    if sent.is_err() || !output.status.success() {
        return Err(FetchError("curl request failed"));
    }
    save_daily_bars(&output.stdout, out)
}

pub fn save_daily_bars(raw: &[u8], out: &Path) -> Result<String, FetchError> {
    let bars = parse_daily_bars(raw).map_err(|_| FetchError("invalid daily bars response"))?;
    let first = bars.first().ok_or(FetchError("empty response"))?;
    let last = bars.last().ok_or(FetchError("empty response"))?;
    let parent = out
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent).map_err(|_| FetchError("cannot create output directory"))?;
    let temporary = parent.join(format!(".fetch-tiingo-{}.tmp", std::process::id()));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|_| FetchError("cannot create temporary output"))?;
    let result = (|| {
        file.write_all(raw)?;
        file.sync_all()?;
        fs::rename(&temporary, out)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
        return Err(FetchError("cannot publish output"));
    }
    Ok(format!(
        "wrote {} bars={} first={} last={} sha256={:x}",
        out.display(),
        bars.len(),
        first.date,
        last.date,
        Sha256::digest(raw)
    ))
}
