use mikiri::h006::{self, InputPaths, input::Stage};
use std::{error::Error, path::PathBuf};
fn run() -> Result<String, Box<dyn Error>> {
    let mut paths: [Option<PathBuf>; 6] = std::array::from_fn(|_| None);
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let i = match flag.as_str() {
            "--histdata-dir" => 0,
            "--spy" => 1,
            "--stage" => 2,
            "--out" => 3,
            "--dev-match" => 4,
            "--ledger" => 5,
            _ => {
                return Err(
                    "expected --histdata-dir, --spy, --stage, --out, --dev-match, or --ledger"
                        .into(),
                );
            }
        };
        if paths[i].is_some() {
            return Err("duplicate argument".into());
        }
        paths[i] = Some(PathBuf::from(args.next().ok_or("missing argument value")?));
    }
    let [
        Some(histdata),
        Some(spy),
        Some(stage),
        Some(out),
        dev_match,
        ledger,
    ] = paths
    else {
        return Err("requires --histdata-dir, --spy, --stage, --out".into());
    };
    let stage = match stage.to_str() {
        Some("dev") => Stage::Dev,
        Some("holdout") => Stage::Holdout,
        _ => return Err("stage must be dev or holdout".into()),
    };
    h006::run(
        &InputPaths {
            histdata_dir: &histdata,
            spy: &spy,
        },
        &out,
        stage,
        dev_match.as_deref(),
        ledger.as_deref(),
    )
}
fn main() {
    match run() {
        Ok(result) => println!("{result}"),
        Err(error) => {
            eprintln!("H006 input or computation failed: {error}");
            std::process::exit(1);
        }
    }
}
