use super::{DEV_END, Result};
use jiff::civil::{Date, date};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{Cursor, Read},
    path::Path,
};

pub(super) const LEGS: usize = 8;
const FIRST_REPORT: Date = date(1992, 10, 6);
const SLOT_EPOCH: Date = date(1992, 10, 3);
const CODES: [&str; LEGS] = [
    "097741", "099741", "096742", "092741", "090741", "232741", "112741", "095741",
];
const FIX_IDS: [&str; LEGS] = [
    "DEXJPUS", "DEXUSEU", "DEXUSUK", "DEXSZUS", "DEXCAUS", "DEXUSAL", "DEXUSNZ", "DEXMXUS",
];
const RATE_CODES: [&str; LEGS + 1] = ["JP", "EZ", "GB", "CH", "CA", "AU", "NZ", "MX", "US"];
const COT_FIELDS: [&str; 5] = [
    "Market and Exchange Names",
    "As of Date in Form YYYY-MM-DD",
    "CFTC Contract Market Code",
    "Commercial Positions-Long (All)",
    "Commercial Positions-Short (All)",
];

pub(super) type Series = BTreeMap<Date, f64>;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Report {
    pub as_of: Date,
    pub slot: usize,
    pub hp: f64,
}

pub(super) struct Inputs {
    pub reports: [Vec<Report>; LEGS],
    pub fixes: [Series; LEGS],
    pub rates: [Series; LEGS + 1],
    pub spy: Series,
    pub hashes: BTreeMap<String, String>,
}

pub(super) fn days_between(start: Date, end: Date) -> i64 {
    end.duration_since(start).as_secs() / 86_400
}

fn csv_fields(line: &str) -> Result<Vec<String>> {
    let mut fields = Vec::new();
    let mut chars = line.chars().peekable();
    loop {
        let mut field = String::new();
        while chars.peek() == Some(&' ') {
            chars.next();
        }
        if chars.next_if_eq(&'"').is_some() {
            loop {
                match chars.next() {
                    Some('"') if chars.next_if_eq(&'"').is_some() => field.push('"'),
                    Some('"') => break,
                    Some(c) => field.push(c),
                    None => return Err("unterminated quoted field".into()),
                }
            }
            while chars.peek() == Some(&' ') {
                chars.next();
            }
        } else {
            while chars.peek().is_some_and(|&c| c != ',') {
                let c = chars.next().ok_or("missing field character")?;
                if c == '"' {
                    return Err("quote in unquoted field".into());
                }
                field.push(c);
            }
        }
        fields.push(field.trim().to_owned());
        match chars.next() {
            None => return Ok(fields),
            Some(',') => {}
            Some(_) => return Err("expected delimiter after field".into()),
        }
    }
}

fn iso_date(raw: &str) -> Result<Date> {
    if raw.len() != 10 {
        return Err("expected YYYY-MM-DD date".into());
    }
    Ok(Date::strptime("%Y-%m-%d", raw)?)
}

fn checked_value(value: f64, positive: bool) -> Result<f64> {
    if !value.is_finite() || (positive && value <= 0.0) {
        return Err("invalid price or rate".into());
    }
    Ok(value)
}

fn cot_report(row: &[String], columns: [usize; 5]) -> Result<Option<(usize, Report)>> {
    let [_, as_of, code, long, short] = columns.map(|column| row[column].as_str());
    let as_of = iso_date(as_of)?;
    if !(FIRST_REPORT..=DEV_END).contains(&as_of) {
        return Ok(None);
    }
    let Some(leg) = CODES.iter().position(|&candidate| candidate == code) else {
        return Ok(None);
    };
    if leg == 1 && as_of < date(1999, 1, 1) {
        return Ok(None);
    }
    let long: u64 = long.parse()?;
    let short: u64 = short.parse()?;
    if long == 0 && short == 0 {
        return Err("zero commercial positions".into());
    }
    let (long, short) = (long as f64, short as f64);
    Ok(Some((
        leg,
        Report {
            as_of,
            slot: (days_between(SLOT_EPOCH, as_of) / 7) as usize,
            hp: (short - long) / (short + long),
        },
    )))
}

#[derive(Default)]
struct Cot {
    slots: [BTreeMap<usize, Report>; LEGS],
    seen: BTreeSet<(usize, Date)>,
}

impl Cot {
    fn read_zip(&mut self, bytes: &[u8]) -> Result<()> {
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes))?;
        if archive.len() != 1 {
            return Err("COT archive must contain exactly one text file".into());
        }
        let mut file = archive.by_index(0)?;
        if !file.is_file() || !file.name()?.to_ascii_lowercase().ends_with(".txt") {
            return Err("COT archive member must be a .txt file".into());
        }
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        let text: String = bytes.into_iter().map(char::from).collect();
        let mut lines = text.lines();
        let header = csv_fields(lines.next().ok_or("missing COT header")?)?;
        if header.len() != 129 {
            return Err("expected 129 COT columns".into());
        }
        let mut columns = [0; 5];
        for (index, name) in COT_FIELDS.into_iter().enumerate() {
            let mut matches = header
                .iter()
                .enumerate()
                .filter(|(_, field)| *field == name);
            columns[index] = matches.next().ok_or("missing COT column")?.0;
            if matches.next().is_some() {
                return Err("duplicate COT column".into());
            }
        }
        for line in lines.filter(|line| !line.trim().is_empty()) {
            let row = csv_fields(line)?;
            if row.len() != header.len() {
                return Err("COT row width differs from header".into());
            }
            if let Some((leg, report)) = cot_report(&row, columns)? {
                if !self.seen.insert((leg, report.as_of)) {
                    return Err(format!("duplicate COT {} {}", CODES[leg], report.as_of).into());
                }
                let kept = self.slots[leg].entry(report.slot).or_insert(report);
                if report.as_of > kept.as_of {
                    *kept = report;
                }
            }
        }
        Ok(())
    }
}

fn ordered(previous: &mut Option<Date>, day: Date) -> Result<()> {
    if previous.is_some_and(|last| last >= day) {
        return Err("duplicate or out-of-order observation date".into());
    }
    *previous = Some(day);
    Ok(())
}

fn fred(bytes: &[u8], id: &str, monthly: bool) -> Result<Series> {
    let mut rows = std::str::from_utf8(bytes)?.lines();
    if rows.next() != Some(&format!("observation_date,{id}")) {
        return Err("unexpected FRED header".into());
    }
    let mut data = Series::new();
    let mut previous = None;
    for row in rows {
        let (day, value) = row.split_once(',').ok_or("missing FRED delimiter")?;
        let day = iso_date(day)?;
        if day > DEV_END {
            continue;
        }
        ordered(&mut previous, day)?;
        if monthly && day.day() != 1 {
            return Err("monthly rate date must be the first day of the month".into());
        }
        if !value.is_empty() {
            let value = checked_value(value.parse()?, !monthly)?;
            data.insert(day, if monthly { value / 100.0 } else { value });
        }
    }
    Ok(data)
}

fn spy(bytes: &[u8]) -> Result<Series> {
    let rows: Vec<serde_json::Value> = serde_json::from_slice(bytes)?;
    let mut data = Series::new();
    let mut previous = None;
    for row in rows {
        let raw = row["date"].as_str().ok_or("missing SPY date")?;
        let day = iso_date(raw.get(..10).ok_or("short SPY date")?)?;
        if day > DEV_END {
            continue;
        }
        ordered(&mut previous, day)?;
        let value = row["adjClose"].as_f64().ok_or("missing SPY adjClose")?;
        data.insert(day, checked_value(value, true)?);
    }
    Ok(data)
}

fn bytes(path: &Path, hashes: &mut BTreeMap<String, String>) -> Result<Vec<u8>> {
    let data = fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let key = path.to_str().ok_or("input path must be UTF-8")?;
    hashes.insert(key.to_owned(), format!("{:x}", Sha256::digest(&data)));
    Ok(data)
}

pub(super) fn read(cot_dir: &Path, fred_dir: &Path, spy_path: &Path) -> Result<Inputs> {
    let mut hashes = BTreeMap::new();
    let mut cot = Cot::default();
    let mut zip_count = 0;
    for entry in fs::read_dir(cot_dir)? {
        let path = entry?.path();
        if path.extension().is_some_and(|ext| ext == "zip") {
            cot.read_zip(&bytes(&path, &mut hashes)?)
                .map_err(|error| format!("{}: {error}", path.display()))?;
            zip_count += 1;
        }
    }
    if zip_count == 0 {
        return Err("no .zip inputs in COT directory".into());
    }
    let mut fixes = std::array::from_fn(|_| Series::new());
    for (series, id) in fixes.iter_mut().zip(FIX_IDS) {
        *series = fred(
            &bytes(&fred_dir.join(format!("{id}.csv")), &mut hashes)?,
            id,
            false,
        )
        .map_err(|error| format!("{id}: {error}"))?;
    }
    let mut rates = std::array::from_fn(|_| Series::new());
    for (series, code) in rates.iter_mut().zip(RATE_CODES) {
        let id = format!("IRSTCI01{code}M156N");
        *series = fred(
            &bytes(&fred_dir.join(format!("{id}.csv")), &mut hashes)?,
            &id,
            true,
        )
        .map_err(|error| format!("{id}: {error}"))?;
    }
    let spy = spy(&bytes(spy_path, &mut hashes)?)?;
    Ok(Inputs {
        reports: cot.slots.map(|slots| slots.into_values().collect()),
        fixes,
        rates,
        spy,
        hashes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn archive(rows: &[(&str, &str, &str, &str)]) -> Vec<u8> {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        zip.start_file("legacy.txt", zip::write::SimpleFileOptions::default())
            .unwrap();
        let extra = ",".repeat(124);
        writeln!(zip, "{}{extra}", COT_FIELDS.join(", ")).unwrap();
        for (as_of, code, long, short) in rows {
            zip.write_all(b"\"March\xe9, \"\"FX\"\"\"").unwrap();
            writeln!(zip, ",{as_of},\"{code}\",{long},{short}{extra}").unwrap();
        }
        zip.finish().unwrap().into_inner()
    }

    #[test]
    fn later_as_of_wins_slot_even_in_reverse_file_order() {
        let mut cot = Cot::default();
        cot.read_zip(&archive(&[("1997-12-19", "097741", "5", "15")]))
            .unwrap();
        cot.read_zip(&archive(&[("1997-12-16", "097741", "15", "5")]))
            .unwrap();
        let kept = cot.slots[0].values().next().unwrap();
        assert_eq!(cot.slots[0].len(), 1);
        assert_eq!(kept.as_of, date(1997, 12, 19));
        assert_eq!(kept.slot, 271);
        assert_eq!(kept.hp, 0.5);
        assert!(
            cot.read_zip(&archive(&[("1997-12-16", "097741", "15", "5")]))
                .is_err()
        );
    }

    #[test]
    fn ecu_is_excluded_and_dev_rows_are_screened_before_values() {
        let mut cot = Cot::default();
        cot.read_zip(&archive(&[
            ("1986-01-15", "099741", "5", "15"),
            ("1998-12-29", "099741", "5", "15"),
            ("1999-01-05", "099741", "15", "5"),
            ("2024-11-05", "099741", "", ""),
        ]))
        .unwrap();
        assert_eq!(cot.slots[1].len(), 1);
        assert_eq!(cot.slots[1].values().next().unwrap().hp, -0.5);
    }

    #[test]
    fn fred_and_spy_ignore_future_values_and_reject_invalid_dev_prices() {
        let input = b"observation_date,DEXJPUS\n2024-10-29,\n2024-10-30,150\n2024-11-01,bad\n";
        assert_eq!(fred(input, "DEXJPUS", false).unwrap().len(), 1);
        for row in ["2024-10-29,140", "2024-10-30,151", "2024-10-31,0"] {
            let input = format!("observation_date,DEXJPUS\n2024-10-30,150\n{row}\n");
            assert!(fred(input.as_bytes(), "DEXJPUS", false).is_err());
        }
        assert_eq!(spy(br#"[{"date":"2024-10-30T00:00:00Z","adjClose":6},{"date":"2024-11-01T00:00:00Z"}]"#)
            .unwrap()[&date(2024, 10, 30)], 6.0);
    }
}
