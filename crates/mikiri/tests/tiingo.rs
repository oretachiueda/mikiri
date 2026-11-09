use mikiri::tiingo::{FetchError, fetch_daily_bars, parse_daily_bars, save_daily_bars};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

fn rows() -> serde_json::Value {
    json!([
        {"date":"2020-01-02T00:00:00.000Z","close":10.0,"divCash":0.0,"splitFactor":1.0},
        {"date":"2020-01-03T00:00:00.000Z","close":11.0,"divCash":0.25,"splitFactor":1.0},
        {"date":"2020-01-06T00:00:00.000Z","close":6.0,"divCash":0.0,"splitFactor":2.0}
    ])
}
fn raw(value: &serde_json::Value) -> Vec<u8> {
    serde_json::to_vec(value).expect("synthetic JSON serializes")
}
fn directory() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "mikiri-test-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&path).expect("create isolated test directory");
    path
}
#[test]
fn parses_three_bars_without_adjusting_fields() {
    let bars = parse_daily_bars(&raw(&rows())).unwrap();
    assert_eq!(bars.len(), 3);
    for (i, (date, close, div_cash, split_factor)) in [
        ("2020-01-02", 10.0, 0.0, 1.0),
        ("2020-01-03", 11.0, 0.25, 1.0),
        ("2020-01-06", 6.0, 0.0, 2.0),
    ]
    .into_iter()
    .enumerate()
    {
        assert_eq!(bars[i].date.to_string(), date);
        assert_eq!(
            (bars[i].close, bars[i].div_cash, bars[i].split_factor),
            (close, div_cash, split_factor)
        );
    }
}
#[test]
fn rejects_each_missing_field_at_correct_position() {
    for field in ["date", "close", "divCash", "splitFactor"] {
        let mut value = rows();
        value[1].as_object_mut().unwrap().remove(field);
        let error = parse_daily_bars(&raw(&value)).unwrap_err();
        assert_eq!(error.element, Some(2));
        assert!(error.to_string().contains(field));
    }
}
#[test]
fn rejects_invalid_numbers_at_correct_position() {
    for (field, number) in [
        ("close", 0.0),
        ("close", -1.0),
        ("splitFactor", 0.0),
        ("splitFactor", -1.0),
        ("divCash", -0.1),
    ] {
        let mut value = rows();
        value[2][field] = json!(number);
        let error = parse_daily_bars(&raw(&value)).unwrap_err();
        assert_eq!(error.element, Some(3));
        assert!(error.to_string().contains(field));
    }
}
#[test]
fn rejects_duplicate_and_descending_dates() {
    for date in ["2020-01-02T00:00:00.000Z", "2020-01-01T00:00:00.000Z"] {
        let mut value = rows();
        value[1]["date"] = json!(date);
        let error = parse_daily_bars(&raw(&value)).unwrap_err();
        assert_eq!(error.element, Some(2));
        assert!(error.to_string().contains("dates"));
    }
}
#[test]
fn rejects_empty_and_malformed_responses() {
    for bytes in [b"[]".as_slice(), b"{}", b"bad"] {
        assert_eq!(parse_daily_bars(bytes).unwrap_err().element, None);
    }
    for date in ["bad", "2020-02-30T00:00:00Z", "2020-01-02T12:00:00Z"] {
        let mut value = rows();
        value[0]["date"] = json!(date);
        assert_eq!(parse_daily_bars(&raw(&value)).unwrap_err().element, Some(1));
    }
    let mut value = rows();
    value[0]["date"] = json!("2020-01-02T00:00:00-05:00");
    assert_eq!(parse_daily_bars(&raw(&value)).unwrap_err().element, Some(1));
    value[0]["date"] = json!("2020-01-02T00:00:00+00:00");
    assert!(parse_daily_bars(&raw(&value)).is_ok());
    value[0]["date"] = json!("2020-01-02T00:00:00.000Z");
    assert!(parse_daily_bars(&raw(&value)).is_ok());
}
#[test]
fn connection_error_never_exposes_token() {
    let dir = directory();
    let token = dir.join("token");
    let out = dir.join("bars.json");
    fs::write(&token, "SECRET-TOKEN-123").unwrap();
    let start = parse_daily_bars(&raw(&rows())).unwrap()[0].date;
    let error = fetch_daily_bars("http://127.0.0.1:9", "SPY", start, &token, &out).unwrap_err();
    assert!(!format!("{error} {error:?}").contains("SECRET-TOKEN-123"));
    assert!(!out.exists());
    fs::remove_dir_all(dir).unwrap();
}
#[test]
fn validation_failure_does_not_create_or_replace_output() {
    let dir = directory();
    let out = dir.join("bars.json");
    assert!(save_daily_bars(b"[]", &out).is_err());
    assert!(!out.exists());
    fs::write(&out, b"existing").unwrap();
    assert!(save_daily_bars(b"[]", &out).is_err());
    assert_eq!(fs::read(&out).unwrap(), b"existing");
    fs::remove_dir_all(dir).unwrap();
}
const HAND_WRITTEN_BAR: &[u8] = br#"[
  {
    "date": "2020-01-02T00:00:00.000Z",
    "close": 412.34,
    "adjClose": 999.99,
    "divCash": 0.0,
    "splitFactor": 1.0
  },
  {
    "date": "2020-01-03T00:00:00.000Z",
    "close": 413.00,
    "adjClose": 1000.50,
    "divCash": 0.0,
    "splitFactor": 1.0
  }
]"#;

#[test]
fn uses_close_not_adj_close_and_preserves_hand_written_bytes() {
    let bars = parse_daily_bars(HAND_WRITTEN_BAR).unwrap();
    assert_eq!(bars.len(), 2);
    assert_eq!(bars[0].close, 412.34);
    assert_eq!(bars[1].close, 413.0);
    let dir = directory();
    let out = dir.join("bars.json");
    save_daily_bars(HAND_WRITTEN_BAR, &out).unwrap();
    assert_eq!(fs::read(&out).unwrap(), HAND_WRITTEN_BAR);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn validation_error_includes_element_and_reason() {
    let mut value = rows();
    value[2]["close"] = json!(0.0);
    let error = save_daily_bars(&raw(&value), PathBuf::from("unused").as_path()).unwrap_err();
    assert!(matches!(error, FetchError::InvalidResponse(_)));
    assert_eq!(
        error.to_string(),
        "invalid daily bars response: element 3: close must be positive"
    );
}

#[test]
fn saves_exact_bytes_and_reports_hash_without_prices() {
    let dir = directory();
    let out = dir.join("bars.json");
    let bytes = HAND_WRITTEN_BAR;
    let summary = save_daily_bars(bytes, &out).unwrap();
    assert_eq!(fs::read(&out).unwrap(), bytes);
    assert_eq!(
        summary,
        format!(
            "wrote {} bars=2 first=2020-01-02 last=2020-01-03 sha256={:x}",
            out.display(),
            Sha256::digest(bytes)
        )
    );
    assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
    fs::remove_dir_all(dir).unwrap();
}
