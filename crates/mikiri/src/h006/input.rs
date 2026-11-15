use jiff::{
    civil::{Date, DateTime, Weekday},
    tz::{AmbiguousOffset, TimeZone},
};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fs::{self, File},
    io::{BufRead, BufReader},
    path::Path,
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

#[derive(Clone, Copy, Debug)]
pub struct Bar {
    pub timestamp: DateTime,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Dev,
    Holdout,
}

pub struct Inputs {
    pub bars: Vec<Bar>,
    pub hashes: BTreeMap<String, String>,
}

fn zones() -> Result<(TimeZone, TimeZone, TimeZone)> {
    Ok((
        TimeZone::get("America/New_York")?,
        TimeZone::get("Europe/Dublin")?,
        TimeZone::get("Asia/Tokyo")?,
    ))
}

fn ny_timestamp(dt: DateTime, ny: &TimeZone) -> Result<jiff::Timestamp> {
    let ambiguous = ny.to_ambiguous_zoned(dt);
    if matches!(ambiguous.offset(), AmbiguousOffset::Gap { .. }) {
        return Err("nonexistent New York timestamp".into());
    }
    Ok(ambiguous.compatible()?.timestamp())
}

pub fn read(dir: &Path, stage: Stage) -> Result<Inputs> {
    let mut names = BTreeSet::new();
    for year in 2007..=2025 {
        names.insert(format!("HISTDATA_COM_ASCII_EURUSD_M1_{year}.zip"));
    }
    for month in 1..=10 {
        names.insert(format!("HISTDATA_COM_ASCII_EURUSD_M1_2026{month:02}.zip"));
    }
    let entries: BTreeSet<String> = fs::read_dir(dir)?
        .map(|e| {
            Ok(e?
                .file_name()
                .into_string()
                .map_err(|_| "non-UTF8 input name")?)
        })
        .collect::<Result<_>>()?;
    let required: BTreeSet<_> = names
        .iter()
        .filter(|name| {
            stage == Stage::Holdout || name.as_str() <= "HISTDATA_COM_ASCII_EURUSD_M1_2024.zip"
        })
        .cloned()
        .collect();
    if !entries.is_subset(&names) || !required.is_subset(&entries) {
        return Err("unexpected or missing HistData zip names".into());
    }
    let (ny, _, tokyo) = zones()?;
    let mut bars = Vec::new();
    let mut hashes = BTreeMap::new();
    for name in names {
        let id = name
            .strip_prefix("HISTDATA_COM_ASCII_EURUSD_M1_")
            .ok_or("invalid zip name")?
            .strip_suffix(".zip")
            .ok_or("invalid zip name")?;
        if stage == Stage::Dev && id > "2024" {
            continue;
        }
        let path = dir.join(&name);
        let mut file = File::open(&path)?;
        let mut hash = Sha256::new();
        std::io::copy(&mut file, &mut hash)?;
        hashes.insert(name.clone(), format!("{:x}", hash.finalize()));
        let mut zip = zip::ZipArchive::new(File::open(path)?)?;
        let member = format!("DAT_ASCII_EURUSD_M1_{id}.csv");
        let mut csv = BufReader::new(zip.by_name(&member)?);
        let mut line = String::new();
        loop {
            line.clear();
            if csv.read_line(&mut line)? == 0 {
                break;
            }
            let cutoff = if stage == Stage::Dev {
                "20241031"
            } else {
                "20261030"
            };
            if line.get(..8).is_some_and(|date| date > cutoff) {
                continue;
            }
            let fields: Vec<_> = line.trim_end_matches(['\r', '\n']).split(';').collect();
            if fields.len() != 6
                || fields[0].len() != 15
                || fields[0].as_bytes()[8] != b' '
                || !fields[0]
                    .bytes()
                    .enumerate()
                    .all(|(i, b)| i == 8 || b.is_ascii_digit())
            {
                return Err("malformed HistData row".into());
            }
            let timestamp = DateTime::strptime("%Y%m%d %H%M%S", fields[0])?;
            if timestamp.second() != 0 {
                return Err("timestamp is not a minute boundary".into());
            }
            if bars.last().is_some_and(|b: &Bar| b.timestamp >= timestamp) {
                return Err("duplicate or out-of-order timestamp".into());
            }
            let instant = ny_timestamp(timestamp, &ny)?;
            if ny.to_datetime(instant) != timestamp
                || ny.to_datetime(ny_timestamp(tokyo.to_datetime(instant), &tokyo)?) != timestamp
            {
                return Err("timestamp round trip failed".into());
            }
            let price = |s: &str| -> Result<f64> {
                let x: f64 = s.parse()?;
                if !x.is_finite() || x <= 0.0 {
                    return Err("price must be positive and finite".into());
                }
                Ok(x)
            };
            let _: f64 = fields[5].parse()?;
            bars.push(Bar {
                timestamp,
                open: price(fields[1])?,
                high: price(fields[2])?,
                low: price(fields[3])?,
                close: price(fields[4])?,
            });
        }
    }
    Ok(Inputs { bars, hashes })
}

pub fn clean(bars: &[Bar]) -> (Vec<Bar>, Vec<DateTime>) {
    let mut kept = Vec::with_capacity(bars.len());
    let mut removed = Vec::new();
    for (i, b) in bars.iter().enumerate() {
        let discard = if i >= 30 && i + 30 < bars.len() {
            let mut close: Vec<_> = bars[i - 30..i]
                .iter()
                .chain(&bars[i + 1..=i + 30])
                .map(|x| x.close)
                .collect();
            close.sort_unstable_by(f64::total_cmp);
            let median = (close[29] + close[30]) / 2.0;
            median > 0.0
                && [b.open, b.high, b.low, b.close]
                    .iter()
                    .any(|v| (v - median).abs() > 0.03 * median)
        } else {
            false
        };
        if discard {
            removed.push(b.timestamp);
        } else {
            kept.push(*b);
        }
    }
    (kept, removed)
}

#[derive(Debug)]
pub struct Day {
    pub date: Date,
    pub eu_min: i16,
    pub us_exit_min: i16,
    pub prices: [f64; 3],
    pub fallback: [bool; 3],
}

pub fn boundary_minutes(date: Date) -> Result<(i16, i16)> {
    let (ny, dublin, tokyo) = zones()?;
    let eu = ny.to_datetime(dublin.to_zoned(date.at(7, 0, 0, 0))?.timestamp());
    let exit = ny.to_datetime(
        tokyo
            .to_zoned(date.tomorrow()?.at(3, 45, 0, 0))?
            .timestamp(),
    );
    if eu.date() != date || exit.date() != date {
        return Err("boundary outside NY date".into());
    }
    Ok((
        i16::from(eu.hour()) * 60 + i16::from(eu.minute()),
        i16::from(exit.hour()) * 60 + i16::from(exit.minute()),
    ))
}

pub fn days(bars: &[Bar]) -> Result<Vec<Day>> {
    let mut grouped: BTreeMap<Date, Vec<&Bar>> = BTreeMap::new();
    for b in bars {
        grouped.entry(b.timestamp.date()).or_default().push(b);
    }
    let mut days = Vec::new();
    for (date, bars) in grouped {
        if matches!(date.weekday(), Weekday::Saturday | Weekday::Sunday) {
            continue;
        }
        let (eu, exit) = boundary_minutes(date)?;
        let mut prices = [0.0; 3];
        let mut fallback = [false; 3];
        let mut valid = true;
        for (i, minute) in [eu, 480, exit].into_iter().enumerate() {
            let b = bars.iter().rev().find(|b| {
                let m = i16::from(b.timestamp.hour()) * 60 + i16::from(b.timestamp.minute());
                m <= minute && m >= minute - 5
            });
            if let Some(b) = b {
                prices[i] = b.open;
                fallback[i] =
                    i16::from(b.timestamp.hour()) * 60 + i16::from(b.timestamp.minute()) != minute;
            } else {
                valid = false;
            }
        }
        if valid {
            days.push(Day {
                date,
                eu_min: eu,
                us_exit_min: exit,
                prices,
                fallback,
            });
        }
    }
    Ok(days)
}

#[derive(Debug, serde::Serialize)]
pub struct SeasonClock {
    pub top: Vec<(i16, f64)>,
    pub sunday_median: f64,
    pub sundays: usize,
}

pub fn clock_check(bars: &[Bar]) -> Result<[SeasonClock; 2]> {
    let mut sums = [[(0.0_f64, 0_u64); 180]; 2];
    let mut sundays: [BTreeMap<Date, i16>; 2] = Default::default();
    for b in bars {
        let month = b.timestamp.month();
        let season = match month {
            1 | 2 => 0,
            7 | 8 => 1,
            _ => continue,
        };
        let minute = i16::from(b.timestamp.hour()) * 60 + i16::from(b.timestamp.minute());
        if b.timestamp.weekday() == Weekday::Sunday {
            sundays[season].entry(b.timestamp.date()).or_insert(minute);
        }
    }
    for pair in bars.windows(2) {
        let a = pair[0];
        let b = pair[1];
        let season = match a.timestamp.month() {
            1 | 2 => 0,
            7 | 8 => 1,
            _ => continue,
        };
        let minute = i16::from(a.timestamp.hour()) * 60 + i16::from(a.timestamp.minute());
        if (420..600).contains(&minute) && b.timestamp.duration_since(a.timestamp).as_secs() == 60 {
            let slot = &mut sums[season][(minute - 420) as usize];
            slot.0 += (b.open / a.open).ln().abs();
            slot.1 += 1;
        }
    }
    let mut result = Vec::new();
    let mut passed = true;
    for (s, days) in sums.iter().zip(sundays) {
        let mut top: Vec<_> = s
            .iter()
            .enumerate()
            .filter(|(_, (_, n))| *n > 0)
            .map(|(i, (sum, n))| (i as i16 + 420, sum / *n as f64))
            .collect();
        top.sort_unstable_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        let mut starts: Vec<_> = days.into_values().collect();
        starts.sort_unstable();
        if starts.is_empty() || top.len() < 2 {
            return Err("clock check lacks observations".into());
        }
        let median =
            (f64::from(starts[(starts.len() - 1) / 2]) + f64::from(starts[starts.len() / 2])) / 2.0;
        let valid = top[0].0 == 510 && top[0].1 > top[1].1 && (1020.0..=1030.0).contains(&median);
        top.truncate(5);
        let clock = SeasonClock {
            top,
            sunday_median: median,
            sundays: starts.len(),
        };
        passed &= valid;
        result.push(clock);
    }
    if !passed {
        return Err(format!("clock check failed: {result:?}").into());
    }
    result.try_into().map_err(|_| "clock check failed".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::date;

    fn bar(dt: &str, p: f64) -> Bar {
        Bar {
            timestamp: dt.parse().unwrap(),
            open: p,
            high: p,
            low: p,
            close: p,
        }
    }

    #[test]
    fn boundary_times_and_dst_transitions() {
        for (d, eu, exit) in [
            (date(2020, 7, 15), 120, 885),
            (date(2020, 1, 15), 120, 825),
            (date(2024, 3, 18), 180, 885),
            (date(2024, 10, 28), 180, 885),
            (date(2024, 6, 10), 120, 885),
            (date(2024, 1, 8), 120, 825),
        ] {
            assert_eq!(boundary_minutes(d).unwrap(), (eu, exit));
        }
        let (ny, _, tokyo) = zones().unwrap();
        let mut day = date(2007, 1, 1);
        while day <= date(2024, 10, 31) {
            let (_, minute) = boundary_minutes(day).unwrap();
            let local = day.at((minute / 60) as i8, (minute % 60) as i8, 0, 0);
            assert_eq!(
                tokyo.to_datetime(ny_timestamp(local, &ny).unwrap()),
                day.tomorrow().unwrap().at(3, 45, 0, 0)
            );
            day = day.tomorrow().unwrap();
        }
        for (d, before, after) in [
            (date(2007, 3, 11), 825, 885),
            (date(2007, 11, 4), 885, 825),
            (date(2024, 3, 10), 825, 885),
            (date(2024, 11, 3), 885, 825),
        ] {
            let prev = d.yesterday().unwrap();
            let next = d.tomorrow().unwrap();
            for (day, minute) in [(prev, before), (next, after)] {
                let dt = day.at((minute / 60) as i8, (minute % 60) as i8, 0, 0);
                assert_eq!(
                    tokyo.to_datetime(ny_timestamp(dt, &ny).unwrap()),
                    day.tomorrow().unwrap().at(3, 45, 0, 0)
                );
            }
            assert!(ny_timestamp(d.at(2, 30, 0, 0), &ny).is_err() == (d.month() == 3));
        }
    }

    #[test]
    fn cleaning_uses_original_neighbours_and_keeps_edges() {
        let mut rows: Vec<_> = (0..=120)
            .map(|i| Bar {
                timestamp: date(2023, 12, 31)
                    .at(23, 0, 0, 0)
                    .checked_add(jiff::Span::new().minutes(i))
                    .unwrap(),
                open: 1.,
                high: 1.,
                low: 1.,
                close: 1.,
            })
            .collect();
        rows[0].high = 2.;
        rows[40].high = 2.;
        rows[60].close = 2.;
        rows[120].low = 0.5;
        let (kept, removed) = clean(&rows);
        assert_eq!(removed, vec![rows[40].timestamp, rows[60].timestamp]);
        assert_eq!(kept.len(), 119);
        assert_eq!(kept[0].timestamp, rows[0].timestamp);
        assert_eq!(kept.last().unwrap().timestamp, rows[120].timestamp);
        let values = "144124340403241134433111430014020234333431200131232343424434120241244401422003302031023300440342424102000440132421022213334344044231232424203420344102";
        let rows: Vec<_> = values
            .bytes()
            .enumerate()
            .map(|(i, v)| {
                let p = 1.0 + f64::from(v - b'0') * 0.02;
                Bar {
                    timestamp: date(2023, 12, 31)
                        .at(23, 0, 0, 0)
                        .checked_add(jiff::Span::new().minutes(i as i64))
                        .unwrap(),
                    open: p,
                    high: p,
                    low: p,
                    close: p,
                }
            })
            .collect();
        let (kept, removed) = clean(&rows);
        assert_eq!(removed.len(), 38);
        assert!(!clean(&kept).1.is_empty());
    }

    #[test]
    fn fallback_and_missing_day() {
        let rows = [
            bar("2024-06-10T01:56:00", 1.),
            bar("2024-06-10T08:00:00", 2.),
            bar("2024-06-10T14:45:00", 3.),
            bar("2024-06-11T08:00:00", 1.),
        ];
        let d = days(&rows).unwrap();
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].prices, [1., 2., 3.]);
        assert_eq!(d[0].fallback, [true, false, false]);
    }

    #[test]
    fn ny_gap_fold_and_roundtrip() {
        let (ny, _, tokyo) = zones().unwrap();
        assert!(ny_timestamp("2024-03-10T02:30:00".parse().unwrap(), &ny).is_err());
        for s in [
            "2024-11-03T01:30:00",
            "2024-07-15T14:45:00",
            "2024-01-15T13:45:00",
        ] {
            let d: DateTime = s.parse().unwrap();
            let t = ny_timestamp(d, &ny).unwrap();
            assert_eq!(
                ny.to_datetime(ny_timestamp(tokyo.to_datetime(t), &tokyo).unwrap()),
                d
            );
        }
        assert_eq!(
            ny_timestamp("2024-11-03T01:30:00".parse().unwrap(), &ny)
                .unwrap()
                .to_string(),
            "2024-11-03T05:30:00Z"
        );
    }
    fn zip_file(path: &Path, member: &str, content: &str) {
        use std::io::Write;
        let mut zip = zip::ZipWriter::new(File::create(path).unwrap());
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        zip.start_file(member, options).unwrap();
        zip.write_all(content.as_bytes()).unwrap();
        zip.finish().unwrap();
    }

    #[test]
    fn input_zip_validation_and_cutoff() {
        let root = std::env::temp_dir().join(format!("mikiri-h006-input-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        for year in 2007..=2024 {
            let zip = root.join(format!("HISTDATA_COM_ASCII_EURUSD_M1_{year}.zip"));
            let member = format!("DAT_ASCII_EURUSD_M1_{year}.csv");
            let content = if year == 2007 {
                "20070102 010000;1;1;1;1;0\n"
            } else {
                ""
            };
            zip_file(&zip, &member, content);
        }
        let path = root.join("HISTDATA_COM_ASCII_EURUSD_M1_2007.zip");
        let member = "DAT_ASCII_EURUSD_M1_2007.csv";
        assert_eq!(read(&root, Stage::Dev).unwrap().bars.len(), 1);
        for bad in [
            "bad\n",
            "20070102 010000;0;1;1;1;0\n",
            "20070102 010000;NaN;1;1;1;0\n",
            "20070102 010000;inf;1;1;1;0\n",
            "20070102 010000;1;1;1;1;0\n20070102 010000;1;1;1;1;0\n",
            "20070102 010100;1;1;1;1;0\n20070102 010000;1;1;1;1;0\n",
            "20240310 023000;1;1;1;1;0\n",
        ] {
            zip_file(&path, member, bad);
            assert!(read(&root, Stage::Dev).is_err(), "{bad}");
        }
        zip_file(&path, "wrong.csv", "");
        assert!(read(&root, Stage::Dev).is_err());
        zip_file(
            &path,
            member,
            "20070102 010000;1;1;1;1;0\n20241101 invalid\n",
        );
        let input = read(&root, Stage::Dev).unwrap();
        assert_eq!(input.bars.len(), 1);
        assert_eq!(input.hashes.len(), 18);
        assert!(read(&root, Stage::Holdout).is_err());
        zip_file(&path, member, "20070102 010000;1;1;1;1;0\n");
        let later = root.join("HISTDATA_COM_ASCII_EURUSD_M1_2025.zip");
        fs::write(&later, "not a zip").unwrap();
        assert_eq!(read(&root, Stage::Dev).unwrap().hashes.len(), 18);
        zip_file(
            &later,
            "DAT_ASCII_EURUSD_M1_2025.csv",
            "20250102 010000;1;1;1;1;0\n",
        );
        for month in 1..=10 {
            let path = root.join(format!("HISTDATA_COM_ASCII_EURUSD_M1_2026{month:02}.zip"));
            let member = format!("DAT_ASCII_EURUSD_M1_2026{month:02}.csv");
            let content = if month == 10 {
                "20261030 010000;1;1;1;1;0\n20261031 malformed\n"
            } else {
                ""
            };
            zip_file(&path, &member, content);
        }
        let holdout = read(&root, Stage::Holdout).unwrap();
        assert_eq!(holdout.bars.len(), 3);
        assert_eq!(holdout.hashes.len(), 29);
        fs::write(root.join("extra"), "").unwrap();
        assert!(read(&root, Stage::Dev).is_err());
        fs::remove_file(root.join("extra")).unwrap();
        fs::remove_file(&path).unwrap();
        assert!(read(&root, Stage::Dev).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn clock_check_pass_and_fail() {
        let mut bars = Vec::new();
        for (sunday, monday) in [
            ("2024-01-07T17:00:00", "2024-01-08"),
            ("2024-07-07T17:00:00", "2024-07-08"),
        ] {
            bars.push(bar(sunday, 1.));
            for (time, p) in [
                ("08:29:00", 1.),
                ("08:30:00", 1.),
                ("08:31:00", 1.1),
                ("08:32:00", 1.101),
            ] {
                bars.push(bar(&format!("{monday}T{time}"), p));
            }
        }
        let report = clock_check(&bars).unwrap();
        assert_eq!(report[0].top[0].0, 510);
        assert_eq!(report[1].sunday_median, 1020.);
        bars[2].open = 1.1;
        assert!(clock_check(&bars).is_err());
    }
}
