//! S002 development-period COT hedging-pressure search.

use crate::h001::{output_hashes, publish_output_directory};
use jiff::civil::{Date, date};
use market::{Market, Sizing, retax, simulate};
use serde_json::json;
use signals::{Config, schedule};
use statistics::{GRID_SIZE, after, annual, expected_max_sr, max_drawdown, offsets, psr, sr};
use std::{
    cmp::Ordering,
    error::Error,
    fs::{self, File},
    io::{BufWriter, Write},
    path::Path,
    process::Command,
};

mod input;
mod market;
mod signals;
mod statistics;

type Result<T> = std::result::Result<T, Box<dyn Error>>;
const DEV_START: Date = date(1996, 1, 2);
const DEV_END: Date = date(2024, 10, 31);
const RECENT_START: Date = date(2019, 11, 1);
const PSR_BAR: f64 = 1.0 - 0.05 / 276.0;

fn mean(values: &[f64]) -> f64 {
    let Some(&first) = values.first() else {
        return f64::NAN;
    };
    first + values.iter().map(|x| x - first).sum::<f64>() / values.len() as f64
}

fn sample_variance(values: &[f64]) -> f64 {
    if values.len() < 2 {
        return f64::NAN;
    }
    let mean = mean(values);
    values.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / (values.len() - 1) as f64
}

struct Row {
    config: Config,
    id: String,
    sr_after_daily: f64,
    sr_before: f64,
    trades_per_year: f64,
    max_gross: f64,
    max_dd: f64,
    psr: f64,
}

fn clean_commit() -> Option<String> {
    let git = |args: &[&str]| {
        let result = Command::new("git")
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .args(args)
            .output()
            .ok()?;
        result.status.success().then_some(result.stdout)
    };
    if !git(&["status", "--porcelain"])?.is_empty() {
        return None;
    }
    Some(
        String::from_utf8(git(&["rev-parse", "HEAD"])?)
            .ok()?
            .trim()
            .to_owned(),
    )
}

fn rows(input: &input::Inputs, market: &Market, sizing: &mut Sizing) -> Result<Vec<Row>> {
    let duration = input::days_between(
        market.dates[market.start],
        market.dates[market.dates.len() - 1],
    ) as f64
        / 365.0;
    Config::all()
        .map(|config| {
            let id = config.id();
            let schedule = schedule(&market.dates, &input.reports, config, market.start)
                .map_err(|error| format!("{id}: {error}"))?;
            let account = simulate(market, sizing, &schedule, None)
                .map_err(|error| format!("{id}: {error}"))?;
            let after = after(&account);
            let before: Vec<_> = account.days.iter().map(|d| d.before).collect();
            let sr_after_daily = sr(&after).map_err(|error| format!("{id}: {error}"))?;
            Ok(Row {
                config,
                id,
                sr_after_daily,
                sr_before: annual(sr(&before)?),
                trades_per_year: account.fills as f64 / duration,
                max_gross: account.max_gross,
                max_dd: max_drawdown(&after),
                psr: psr(&after, sr_after_daily, 0.0)?,
            })
        })
        .collect()
}

fn winner_order(a: &Row, b: &Row) -> Ordering {
    if a.sr_after_daily == b.sr_after_daily {
        return a.id.cmp(&b.id);
    }
    b.sr_after_daily.total_cmp(&a.sr_after_daily)
}

/// Run S002 dev, recent-window tax replay, and the circular-shift gate.
pub fn run(cot: &Path, fred: &Path, spy: &Path, out: &Path) -> Result<String> {
    let input = input::read(cot, fred, spy)?;
    let market = Market::new(&input)?;
    let dates = &market.dates[market.start..];
    let offsets = offsets(dates.len())?;
    let recent = market.dates.partition_point(|&day| day < RECENT_START);
    let b_dev = market.baseline(market.start)?;
    let b_recent = market.baseline(recent)?;
    let mut sizing = Sizing::default();
    let rows = rows(&input, &market, &mut sizing)?;
    let winner = rows
        .iter()
        .min_by(|a, b| winner_order(a, b))
        .ok_or("empty configuration grid")?;
    let mut sr_grid: Vec<_> = rows.iter().map(|row| row.sr_after_daily).collect();
    let variance = sample_variance(&sr_grid);
    sr_grid.sort_unstable_by(f64::total_cmp);
    let median = annual((sr_grid[GRID_SIZE / 2 - 1] + sr_grid[GRID_SIZE / 2]) / 2.0);
    let sr0 = expected_max_sr(variance);
    let schedule = schedule(&market.dates, &input.reports, winner.config, market.start)?;
    let selected = simulate(&market, &mut sizing, &schedule, None)?;
    let w_dev = selected.equity;
    let w_recent = retax(
        &market.dates[recent..],
        &selected.days[recent - market.start..],
    )?;
    let dsr = psr(&after(&selected), winner.sr_after_daily, sr0)?;
    let mut below = 0;
    let mut skipped = 0;
    for &offset in &offsets {
        let run = simulate(&market, &mut sizing, &schedule, Some(offset))
            .map_err(|error| format!("D offset {offset}: {error}"))?;
        let sr = sr(&after(&run)).map_err(|error| format!("D offset {offset}: {error}"))?;
        below += usize::from(sr < winner.sr_after_daily);
        skipped += run.skipped;
    }
    let summary = json!({
        "T": dates.len(),
        "dev_start": DEV_START.to_string(),
        "dev_end": DEV_END.to_string(),
        "winner": winner.id,
        "winner_sr_after_annual": annual(winner.sr_after_daily),
        "gates": {
            "A": w_dev > b_dev, "B1": winner.psr >= PSR_BAR, "B2": dsr >= 0.95,
            "C": w_recent > b_recent, "D": below >= 950,
        },
        "values": {
            "W_dev": w_dev, "B_dev": b_dev, "psr": winner.psr, "psr_bar": PSR_BAR,
            "dsr": dsr, "sr0_daily": sr0, "V": variance, "W_C": w_recent, "B_C": b_recent,
            "d_count_below": below, "d_offsets_first10": offsets[..10],
            "d_skipped_actions": skipped,
        },
        "single_trial_psr": winner.psr,
        "single_trial_bar_pass": winner.psr >= 0.95,
        "median_sr_after": median,
        "max_sr_after": annual(winner.sr_after_daily),
        "input_sha256": input.hashes,
        "code": clean_commit(),
    });
    publish_output_directory(out, |dir| {
        let mut csv = BufWriter::new(File::create(dir.join("results.csv"))?);
        writeln!(
            csv,
            "config,sr_after,sr_before,trades_per_year,max_gross_L,max_dd,psr"
        )?;
        for row in &rows {
            writeln!(
                csv,
                "{},{},{},{},{},{},{}",
                row.id,
                annual(row.sr_after_daily),
                row.sr_before,
                row.trades_per_year,
                row.max_gross,
                row.max_dd,
                row.psr
            )?;
        }
        csv.flush()?;
        fs::write(
            dir.join("summary.json"),
            serde_json::to_vec_pretty(&summary)?,
        )?;
        Ok(())
    })?;
    output_hashes(out, &["results.csv", "summary.json"])
}
