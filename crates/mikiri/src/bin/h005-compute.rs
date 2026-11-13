use mikiri::h005::{self, InputPaths, Stage};
use std::{error::Error, path::PathBuf};

fn run() -> Result<String, Box<dyn Error>> {
    let mut paths: [Option<PathBuf>; 8] = std::array::from_fn(|_| None);
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let i=match flag.as_str() {
            "--fx-dir"=>0, "--rates"=>1, "--spy"=>2, "--out"=>3,
            "--stage"=>4, "--first-release-dir"=>5, "--dev-match"=>6, "--ledger"=>7,
            _=>return Err("expected --fx-dir, --rates, --spy, --out, --stage, --first-release-dir, --dev-match, or --ledger".into()),
        };
        if paths[i].is_some() {
            return Err("duplicate argument".into());
        }
        paths[i] = Some(PathBuf::from(args.next().ok_or("missing argument value")?));
    }
    let [
        Some(fx_dir),
        Some(rates),
        Some(spy),
        Some(out),
        Some(stage),
        first_release_dir,
        dev_match,
        ledger,
    ] = paths
    else {
        return Err("requires --fx-dir, --rates, --spy, --out, --stage".into());
    };
    let stage = Stage::parse(stage.to_str().ok_or("invalid stage")?)?;
    h005::run(
        &InputPaths {
            fx_dir: &fx_dir,
            rates: &rates,
            spy: &spy,
            first_release_dir: first_release_dir.as_deref(),
        },
        &out,
        stage,
        dev_match.as_deref(),
        ledger.as_deref(),
    )
}
fn main() {
    match run() {
        Ok(report) => println!("{report}"),
        Err(e) => {
            eprintln!("H005 input or computation failed: {e}");
            std::process::exit(1);
        }
    }
}
