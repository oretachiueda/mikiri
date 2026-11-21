use super::{Result, input::LEGS};
use jiff::civil::Date;
use std::io::Write;

const LEG_NAMES: [&str; LEGS] = ["JPY", "EUR", "GBP", "CHF", "CAD", "AUD", "NZD", "MXN"];

pub(super) struct TraceRow {
    pub date: Date,
    pub leg: usize,
    pub signal_day: bool,
    pub report_as_of: Option<Date>,
    pub fill: bool,
    pub leverage: f64,
    pub notional: f64,
    pub prev_equity: f64,
    pub spot_ret: f64,
    pub carry_markup_amount: f64,
    pub spread_cost_amount: f64,
    pub leg_pnl: f64,
    pub daily_return: f64,
}

pub(super) fn write_csv(mut csv: impl Write, rows: &[TraceRow]) -> Result<()> {
    writeln!(
        csv,
        "date,leg,signal_day_flag,report_as_of,fill_flag,direction,L,notional,prev_equity,spot_ret,carry_markup_amount,spread_cost_amount,leg_pnl,daily_return"
    )?;
    for row in rows {
        let report = row
            .report_as_of
            .map(|date| date.to_string())
            .unwrap_or_default();
        let direction = i8::from(row.notional > 0.0) - i8::from(row.notional < 0.0);
        writeln!(
            csv,
            "{},{},{},{},{},{},{:?},{:?},{:?},{:?},{:?},{:?},{:?},{:?}",
            row.date,
            LEG_NAMES[row.leg],
            row.signal_day,
            report,
            row.fill,
            direction,
            row.leverage,
            row.notional,
            row.prev_equity,
            row.spot_ret,
            row.carry_markup_amount,
            row.spread_cost_amount,
            row.leg_pnl,
            row.daily_return,
        )?;
    }
    csv.flush()?;
    Ok(())
}
