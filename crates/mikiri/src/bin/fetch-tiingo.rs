use mikiri::tiingo::{checked_output_path, fetch_daily_bars};
use std::path::PathBuf;
use time::Date;

fn run() -> Result<String, String> {
    let mut ticker = None;
    let mut start = None;
    let mut out = None;
    let mut token_file = None;
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let slot = match flag.as_str() {
            "--ticker" => &mut ticker,
            "--start" => &mut start,
            "--out" => &mut out,
            "--token-file" => &mut token_file,
            _ => return Err("expected --ticker, --start, --out, or --token-file".into()),
        };
        if slot.is_some() {
            return Err("duplicate argument".into());
        }
        *slot = Some(args.next().ok_or("missing argument value")?);
    }
    let ticker = ticker.ok_or("missing --ticker")?;
    let start = start.ok_or("missing --start")?;
    let format = time::format_description::parse_borrowed::<2>("[year]-[month]-[day]")
        .map_err(|_| "invalid date format")?;
    let start = Date::parse(&start, &format).map_err(|_| "start must be YYYY-MM-DD")?;
    let out = PathBuf::from(out.ok_or("missing --out")?);
    checked_output_path(&out).map_err(|e| e.to_string())?;
    let token_file = match token_file {
        Some(path) => PathBuf::from(path),
        None => PathBuf::from(std::env::var_os("HOME").ok_or("HOME is not set")?)
            .join(".config/mikiri/tiingo.token"),
    };
    fetch_daily_bars("https://api.tiingo.com", &ticker, start, &token_file, &out)
        .map_err(|e| e.to_string())
}
fn main() -> std::process::ExitCode {
    match run() {
        Ok(summary) => {
            println!("{summary}");
            std::process::ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error}");
            std::process::ExitCode::FAILURE
        }
    }
}
