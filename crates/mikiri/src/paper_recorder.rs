use serde::{Deserialize, Serialize};
use std::{fs, io::Write, path::Path};
use time::{Date, format_description::well_known::Iso8601};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    seq: usize,
    date: String,
    ticker: String,
    qty: u8,
    close: f64,
    bars_sha256: String,
    prev: String,
    hash: String,
}

fn digest(bytes: &[u8]) -> String {
    crate::sha256_hex(bytes)
}
fn hash(entry: &Entry) -> Result<String, String> {
    let mut value = serde_json::to_value(entry).map_err(|e| e.to_string())?;
    value.as_object_mut().ok_or("invalid entry")?.remove("hash");
    Ok(digest(
        &serde_json::to_vec(&value).map_err(|e| e.to_string())?,
    ))
}
fn date(s: &str) -> Result<Date, String> {
    Date::parse(s, &Iso8601::DATE).map_err(|e| e.to_string())
}
fn read_log(path: &Path, allow_missing: bool) -> Result<Vec<Entry>, (i32, String)> {
    let raw = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if allow_missing && e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err((1, e.to_string())),
    };
    let mut entries: Vec<Entry> = Vec::new();
    for line in raw.lines() {
        let e: Entry = serde_json::from_str(line).map_err(|e| (3, e.to_string()))?;
        let prev = entries.last();
        let valid = e.seq == entries.len()
            && e.qty == 1
            && e.hash == hash(&e).map_err(|e| (3, e))?
            && e.prev == prev.map_or_else(|| "0".repeat(64), |p| p.hash.clone())
            && date(&e.date).is_ok()
            && prev.is_none_or(|p| p.date < e.date && p.ticker == e.ticker);
        if !valid {
            return Err((3, "invalid log chain".into()));
        }
        entries.push(e);
    }
    Ok(entries)
}
pub fn verify(path: &Path) -> Result<String, (i32, String)> {
    let entries = read_log(path, false)?;
    Ok(format!(
        "lines={} last_hash={}",
        entries.len(),
        entries
            .last()
            .map_or_else(|| "0".repeat(64), |e| e.hash.clone())
    ))
}
pub fn record(
    bars_path: &Path,
    log: &Path,
    ticker: &str,
    today: &str,
) -> Result<String, (i32, String)> {
    if ticker.is_empty() {
        return Err((1, "empty ticker".into()));
    }
    let today = date(today).map_err(|e| (1, e))?;
    let mut entries = read_log(log, true)?;
    if entries.first().is_some_and(|e| e.ticker != ticker) {
        return Err((2, "ticker mismatch".into()));
    }
    let raw = fs::read(bars_path).map_err(|e| (1, e.to_string()))?;
    let bars = crate::tiingo::parse_daily_bars(&raw).map_err(|e| (1, e.to_string()))?;
    let latest = bars.last().ok_or((1, "empty bars".into()))?.date;
    if (today - latest).whole_days() > 4 {
        return Err((2, "stale bars".into()));
    }
    for bar in &bars {
        if entries
            .iter()
            .any(|e| e.date == bar.date.to_string() && e.close != bar.close)
        {
            return Err((2, "vendor revision".into()));
        }
    }
    let old_len = entries.len();
    let sha = digest(&raw);
    for bar in bars {
        if let Some(last) = entries.last() {
            let last_date = date(&last.date).map_err(|e| (3, e))?;
            if bar.date <= last_date {
                continue;
            }
            guard::session(last_date).map_err(|e| (2, e.to_string()))?;
            let mut day = last_date;
            while let Some(next) = day.next_day() {
                if next >= bar.date {
                    break;
                }
                if guard::session(next).map_err(|e| (2, e.to_string()))? != guard::Session::Closed {
                    return Err((2, "bar gap".into()));
                }
                day = next;
            }
            guard::session(bar.date).map_err(|e| (2, e.to_string()))?;
        }
        let mut e = Entry {
            seq: entries.len(),
            date: bar.date.to_string(),
            ticker: ticker.into(),
            qty: 1,
            close: bar.close,
            bars_sha256: sha.clone(),
            prev: entries
                .last()
                .map_or_else(|| "0".repeat(64), |p| p.hash.clone()),
            hash: String::new(),
        };
        e.hash = hash(&e).map_err(|e| (1, e))?;
        entries.push(e);
    }
    if entries.len() != old_len {
        let parent = log
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let tmp = parent.join(format!(".paper-recorder-{}.tmp", std::process::id()));
        let result = (|| -> Result<(), String> {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&tmp)
                .map_err(|e| e.to_string())?;
            for e in &entries {
                serde_json::to_writer(&mut file, e).map_err(|e| e.to_string())?;
                file.write_all(b"\n").map_err(|e| e.to_string())?;
            }
            file.sync_all().map_err(|e| e.to_string())?;
            fs::rename(&tmp, log).map_err(|e| e.to_string())
        })();
        if let Err(e) = result {
            let _ = fs::remove_file(tmp);
            return Err((1, e));
        }
    }
    Ok(format!(
        "appended={} last_date={} last_hash={}",
        entries.len() - old_len,
        entries.last().map_or("none", |e| &e.date),
        entries
            .last()
            .map_or_else(|| "0".repeat(64), |e| e.hash.clone())
    ))
}
