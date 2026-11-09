use mikiri::{
    h001::{checked_output_path, compute, write_outputs},
    tiingo::parse_daily_bars,
};
use std::{error::Error, path::PathBuf};
fn run() -> Result<String, Box<dyn Error>> {
    let (mut bars, mut out) = (None, None);
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let slot = match flag.as_str() {
            "--bars" => &mut bars,
            "--out" => &mut out,
            _ => return Err("expected --bars or --out".into()),
        };
        if slot.is_some() {
            return Err("duplicate argument".into());
        }
        *slot = Some(PathBuf::from(args.next().ok_or("missing argument value")?));
    }
    let out = out.ok_or("missing --out")?;
    checked_output_path(&out)?;
    let bars = parse_daily_bars(&std::fs::read(bars.ok_or("missing --bars")?)?)?;
    let (cycles, summary) = compute(&bars)?;
    write_outputs(&bars, &cycles, &summary, &out)
}
fn main() -> std::process::ExitCode {
    match run() {
        Ok(report) => {
            println!("{report}");
            std::process::ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error}");
            std::process::ExitCode::FAILURE
        }
    }
}
