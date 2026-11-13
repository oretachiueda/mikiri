use crate::h001::{checked_output_path, publish_outputs};
use arrow_array::{ArrayRef, BooleanArray, Float64Array, Int64Array, RecordBatch, StringArray};
use arrow_schema::{Field, Schema};
use parquet::arrow::ArrowWriter;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fs,
    path::Path,
    sync::Arc,
};
use time::{Date, Month};

pub mod core;
pub type Result<T> = std::result::Result<T, Box<dyn Error>>;
pub const FX: [&str; 9] = [
    "DEXUSEU", "DEXJPUS", "DEXUSUK", "DEXSZUS", "DEXCAUS", "DEXUSAL", "DEXUSNZ", "DEXNOUS",
    "DEXSDUS",
];
pub const AREAS: [&str; 10] = [
    "EA20", "JPN", "GBR", "CHE", "CAN", "AUS", "NZL", "NOR", "SWE", "USA",
];
const RATE_IDS: [&str; 10] = [
    "IR3TIB01EZM156N",
    "IR3TIB01JPM156N",
    "IR3TIB01GBM156N",
    "IR3TIB01CHM156N",
    "IR3TIB01CAM156N",
    "IR3TIB01AUM156N",
    "IR3TIB01NZM156N",
    "IR3TIB01NOM156N",
    "IR3TIB01SEM156N",
    "IR3TIB01USM156N",
];
const OUTPUTS: [&str; 4] = [
    "days.parquet",
    "indicators.parquet",
    "observations.parquet",
    "summary.json",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Dev,
    Holdout,
}
impl Stage {
    pub fn name(self) -> &'static str {
        match self {
            Self::Dev => "dev",
            Self::Holdout => "holdout",
        }
    }
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "dev" => Ok(Self::Dev),
            "holdout" => Ok(Self::Holdout),
            _ => Err("stage must be dev or holdout".into()),
        }
    }
    fn keep(self, date: &str, monthly: bool) -> bool {
        if monthly {
            date < if self == Self::Dev {
                "2024-10-01"
            } else {
                "2026-10-01"
            }
        } else {
            date <= if self == Self::Dev {
                "2024-10-31"
            } else {
                "2026-10-30"
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Observation {
    pub kind: String,
    pub series: String,
    pub date: String,
    pub value: Option<f64>,
    pub close: Option<f64>,
    pub div_cash: Option<f64>,
    pub split_factor: Option<f64>,
}
impl Observation {
    fn new(kind: &str, series: &str, date: &str, value: Option<f64>) -> Self {
        Self {
            kind: kind.into(),
            series: series.into(),
            date: date.into(),
            value,
            close: None,
            div_cash: None,
            split_factor: None,
        }
    }
}

pub struct InputPaths<'a> {
    pub fx_dir: &'a Path,
    pub rates: &'a Path,
    pub spy: &'a Path,
    pub first_release_dir: Option<&'a Path>,
}

fn parse_date(text: &str) -> Result<Date> {
    let parts = text.split('-').collect::<Vec<_>>();
    if parts.len() != 3
        || text.len() != 10
        || parts[0].len() != 4
        || parts[1].len() != 2
        || parts[2].len() != 2
    {
        return Err("invalid ISO date".into());
    }
    let day = Date::from_calendar_date(
        parts[0].parse()?,
        Month::try_from(parts[1].parse::<u8>()?)?,
        parts[2].parse()?,
    )?;
    if day.to_string() != text {
        return Err("invalid ISO date".into());
    }
    Ok(day)
}

fn number(s: &str) -> Result<f64> {
    let value: f64 = s.parse()?;
    if !value.is_finite() {
        return Err("non-finite observation".into());
    }
    Ok(value)
}

fn fred(
    path: &Path,
    series: &str,
    kind: &str,
    stage: Stage,
    monthly: bool,
) -> Result<Vec<Observation>> {
    let text = fs::read_to_string(path)?;
    let mut lines = text.lines();
    if lines.next() != Some(format!("observation_date,{series}").as_str()) {
        return Err("invalid FRED header".into());
    }
    let mut previous = None;
    let mut rows = Vec::new();
    for line in lines {
        let columns = line.split(',').collect::<Vec<_>>();
        if columns.len() != 2 {
            return Err("invalid FRED row".into());
        }
        let date = parse_date(columns[0])?;
        if !stage.keep(columns[0], monthly) {
            continue;
        }
        if previous.is_some_and(|p| p >= date) {
            return Err("FRED dates must increase without duplicates".into());
        }
        previous = Some(date);
        if monthly && date.day() != 1 {
            return Err("monthly FRED date must be first of month".into());
        }
        let value = if columns[1].is_empty() {
            None
        } else {
            Some(number(columns[1])?)
        };
        if !monthly && value.is_some_and(|v| v <= 0.0) {
            return Err("FX spot must be positive".into());
        }
        rows.push(Observation::new(kind, series, columns[0], value));
    }
    Ok(rows)
}

fn oecd(path: &Path, stage: Stage) -> Result<Vec<Observation>> {
    let text = fs::read_to_string(path)?;
    let mut lines = text.lines();
    if lines.next()
        != Some(
            "DATAFLOW,REF_AREA,FREQ,MEASURE,UNIT_MEASURE,ACTIVITY,ADJUSTMENT,TRANSFORMATION,TIME_HORIZ,METHODOLOGY,TIME_PERIOD,OBS_VALUE,OBS_STATUS,UNIT_MULT,DECIMALS,BASE_PER",
        )
    {
        return Err("invalid OECD header".into());
    }
    let mut pairs = BTreeSet::new();
    let mut areas = BTreeSet::new();
    let mut rows = Vec::new();
    for line in lines {
        let c = line.split(',').collect::<Vec<_>>();
        if c.len() != 16 {
            return Err("invalid OECD columns".into());
        }
        let date = format!("{}-01", c[10]);
        parse_date(&date)?;
        if !stage.keep(&date, true) {
            continue;
        }
        if [
            (0, "OECD.SDD.STES:DSD_STES@DF_FINMARK(4.0)"),
            (2, "M"),
            (3, "IR3TIB"),
            (4, "PA"),
            (5, "_Z"),
            (6, "_Z"),
            (7, "_Z"),
            (8, "_Z"),
            (9, "N"),
            (13, "0"),
        ]
        .iter()
        .any(|(i, v)| c[*i] != *v)
            || !AREAS.contains(&c[1])
        {
            return Err("invalid OECD series or units".into());
        }
        if !pairs.insert((c[1].to_string(), date.clone())) {
            return Err("duplicate OECD area/month".into());
        }
        areas.insert(c[1].to_string());
        rows.push(Observation::new("rate", c[1], &date, Some(number(c[11])?)));
    }
    if areas != AREAS.iter().map(|s| s.to_string()).collect() {
        return Err("OECD requires exact ten-area set".into());
    }
    Ok(rows)
}

pub fn load(inputs: &InputPaths<'_>, stage: Stage) -> Result<Vec<Observation>> {
    let mut rows = Vec::new();
    for series in FX {
        rows.extend(fred(
            &inputs.fx_dir.join(format!("{series}.csv")),
            series,
            "fx",
            stage,
            false,
        )?);
    }
    rows.extend(oecd(inputs.rates, stage)?);
    let raw: Value = serde_json::from_slice(&fs::read(inputs.spy)?)?;
    let mut kept = Vec::new();
    for row in raw.as_array().ok_or("expected Tiingo array")? {
        let date = row["date"]
            .as_str()
            .and_then(|s| s.get(..10))
            .ok_or("missing Tiingo date")?;
        parse_date(date)?;
        if stage.keep(date, false) {
            kept.push(row);
        }
    }
    let bars = crate::tiingo::parse_daily_bars(&serde_json::to_vec(&kept)?)?;
    for b in bars {
        let mut row = Observation::new("spy", "SPY", &b.date.to_string(), None);
        row.close = Some(b.close);
        row.div_cash = Some(b.div_cash);
        row.split_factor = Some(b.split_factor);
        rows.push(row);
    }
    if let Some(dir) = inputs.first_release_dir {
        for (ids, kind, monthly) in [
            (&FX[..], "first_fx", false),
            (&RATE_IDS[..], "first_rate", true),
        ] {
            for id in ids {
                let path = dir.join(format!("{id}.csv"));
                if path.try_exists()? {
                    rows.extend(fred(&path, id, kind, stage, monthly)?);
                }
            }
        }
    }
    rows.sort_by(|a, b| (&a.kind, &a.series, &a.date).cmp(&(&b.kind, &b.series, &b.date)));
    Ok(rows)
}

pub fn input_hashes(inputs: &InputPaths<'_>) -> Result<BTreeMap<String, String>> {
    let mut paths = FX
        .iter()
        .map(|s| (format!("{s}.csv"), inputs.fx_dir.join(format!("{s}.csv"))))
        .collect::<Vec<_>>();
    paths.push(("OECD.csv".into(), inputs.rates.to_owned()));
    paths.push(("SPY.json".into(), inputs.spy.to_owned()));
    paths
        .into_iter()
        .map(|(key, path)| Ok((key, format!("{:x}", Sha256::digest(fs::read(path)?)))))
        .collect()
}

fn digest(path: &Path) -> Result<String> {
    Ok(format!("{:x}", Sha256::digest(fs::read(path)?)))
}

fn equivalent(expected: &Value, actual: &Value, field: &str) -> bool {
    match (expected, actual) {
        (Value::Object(a), Value::Object(b)) => {
            a.len() == b.len()
                && a.iter()
                    .all(|(k, v)| b.get(k).is_some_and(|w| equivalent(v, w, k)))
        }
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(v, w)| equivalent(v, w, field))
        }
        (Value::Number(a), Value::Number(b)) => {
            if field.contains("signal") || (a.is_i64() && b.is_i64()) {
                return a == b;
            }
            match (a.as_f64(), b.as_f64()) {
                (Some(x), Some(y)) => {
                    x.is_finite()
                        && y.is_finite()
                        && (x - y).abs()
                            <= if field == "psr_main" {
                                1e-9
                            } else {
                                1e-10 * x.abs().max(y.abs()).max(1.0)
                            }
                }
                _ => false,
            }
        }
        _ => expected == actual,
    }
}

pub fn require_holdout(
    inputs: &InputPaths<'_>,
    envelope: Option<&Path>,
    ledger: Option<&Path>,
) -> Result<()> {
    let envelope = envelope.ok_or("holdout requires --dev-match")?;
    let matched: Value = serde_json::from_slice(&fs::read(envelope)?)?;
    if matched["match"].as_bool() != Some(true)
        || matched["python_summary"]["stage"] != "dev"
        || matched["rust_summary"]["stage"] != "dev"
    {
        return Err("dev comparison must match".into());
    }
    let dir = envelope
        .parent()
        .ok_or("dev match needs result directory")?;
    let manifest = fs::read_to_string(dir.join("SHA256SUMS"))?;
    let mut expected = String::new();
    for name in OUTPUTS {
        expected.push_str(&format!("{}  {name}\n", digest(&dir.join(name))?));
    }
    if manifest != expected {
        return Err("dev result manifest mismatch".into());
    }
    let result_hash = digest(&dir.join("SHA256SUMS"))?;
    let actual_summary: Value = serde_json::from_slice(&fs::read(dir.join("summary.json"))?)?;
    if matched["results_sha256"] != result_hash
        || matched["rust_summary"] != actual_summary
        || !equivalent(&actual_summary, &matched["python_summary"], "")
    {
        return Err("dev envelope results mismatch".into());
    }
    let lines = fs::read_to_string(ledger.ok_or("holdout requires --ledger")?)?;
    let mut records = Vec::new();
    for line in lines.lines() {
        let v: Value = serde_json::from_str(line)?;
        if v["hypothesis"] == "H005" {
            records.push(v);
        }
    }
    if records
        .iter()
        .any(|v| v["kind"] == "run" && v["stage"] == "holdout")
    {
        return Err("H005 holdout already used".into());
    }
    let (run_index, run) = records
        .iter()
        .enumerate()
        .rev()
        .find(|(_, v)| v["kind"] == "run" && v["stage"] == "dev")
        .ok_or("missing H005 dev run")?;
    if run["results_sha256"] != result_hash
        || run["data_sha256"] != serde_json::to_value(input_hashes(inputs)?)?
    {
        return Err("dev run input or result hashes mismatch".into());
    }
    let (verdict_index, verdict) = records
        .iter()
        .enumerate()
        .skip(run_index + 1)
        .rev()
        .find(|(_, v)| v["kind"] == "verdict" && v["stage"] == "dev")
        .ok_or("missing H005 dev ledger verdict")?;
    if verdict["result"] != "pass"
        || verdict["results_sha256"] != result_hash
        || ["A", "B", "C"]
            .iter()
            .any(|k| verdict["gates"][*k].as_bool() != Some(true))
    {
        return Err("H005 dev ledger gates or results mismatch".into());
    }
    if !records
        .iter()
        .skip(verdict_index + 1)
        .any(|v| v["kind"] == "holdout_open" && v["stage"] == "holdout")
    {
        return Err("missing H005 holdout_open".into());
    }
    Ok(())
}

fn write_rows(path: &Path, rows: &[Value]) -> Result<()> {
    if rows.is_empty() {
        return Err("cannot write empty result table".into());
    }
    let names = rows
        .iter()
        .flat_map(|r| r.as_object().into_iter().flat_map(|o| o.keys().cloned()))
        .collect::<BTreeSet<_>>();
    let mut fields = Vec::new();
    let mut arrays: Vec<ArrayRef> = Vec::new();
    for name in names {
        let values = rows.iter().map(|r| &r[&name]).collect::<Vec<_>>();
        let example = values.iter().find(|v| !v.is_null());
        let array: ArrayRef = match example {
            Some(v) if v.is_string() => Arc::new(StringArray::from(
                values.iter().map(|v| v.as_str()).collect::<Vec<_>>(),
            )),
            Some(v) if v.is_boolean() => Arc::new(BooleanArray::from(
                values.iter().map(|v| v.as_bool()).collect::<Vec<_>>(),
            )),
            Some(v) if v.is_i64() && values.iter().all(|v| v.is_null() || v.is_i64()) => Arc::new(
                Int64Array::from(values.iter().map(|v| v.as_i64()).collect::<Vec<_>>()),
            ),
            _ => {
                if values.iter().any(|v| !v.is_null() && !v.is_number()) {
                    return Err("result rows must contain scalar values".into());
                }
                Arc::new(Float64Array::from(
                    values.iter().map(|v| v.as_f64()).collect::<Vec<_>>(),
                ))
            }
        };
        fields.push(Field::new(name, array.data_type().clone(), true));
        arrays.push(array);
    }
    let batch = RecordBatch::try_new(Arc::new(Schema::new(fields)), arrays)?;
    let mut writer = ArrowWriter::try_new(fs::File::create(path)?, batch.schema(), None)?;
    writer.write(&batch)?;
    writer.close()?;
    Ok(())
}

pub fn run(
    inputs: &InputPaths<'_>,
    out: &Path,
    stage: Stage,
    dev_match: Option<&Path>,
    ledger: Option<&Path>,
) -> Result<String> {
    checked_output_path(out)?;
    if stage == Stage::Holdout {
        require_holdout(inputs, dev_match, ledger)?;
    }
    let rows = load(inputs, stage)?;
    let result = core::compute(&rows, stage)?;
    publish_outputs(out, |tmp| {
        write_rows(
            &tmp.join("observations.parquet"),
            &rows
                .iter()
                .map(serde_json::to_value)
                .collect::<std::result::Result<Vec<_>, _>>()?,
        )?;
        write_rows(&tmp.join("indicators.parquet"), &result.indicators)?;
        write_rows(&tmp.join("days.parquet"), &result.days)?;
        fs::write(
            tmp.join("summary.json"),
            serde_json::to_vec_pretty(&result.summary)?,
        )?;
        let mut manifest = String::new();
        for name in OUTPUTS {
            manifest.push_str(&format!("{}  {name}\n", digest(&tmp.join(name))?));
        }
        fs::write(tmp.join("SHA256SUMS"), manifest)?;
        Ok(())
    })?;
    Ok(format!(
        "{} stage={} results_sha256={}",
        out.display(),
        stage.name(),
        digest(&out.join("SHA256SUMS"))?
    ))
}

#[cfg(test)]
mod tests;
