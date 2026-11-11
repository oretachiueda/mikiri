use mikiri::{h001::checked_output_path, h003};
use std::{error::Error, path::PathBuf};

fn run() -> Result<String, Box<dyn Error>> {
    let mut values: [Option<PathBuf>; 4] = [None, None, None, None];
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let index = match flag.as_str() {
            "--spy" => 0,
            "--spxl" => 1,
            "--bil" => 2,
            "--out" => 3,
            _ => return Err("expected --spy, --spxl, --bil, or --out".into()),
        };
        if values[index].is_some() {
            return Err("duplicate argument".into());
        }
        values[index] = Some(PathBuf::from(args.next().ok_or("missing argument value")?));
    }
    let [Some(spy), Some(spxl), Some(bil), Some(out)] = values else {
        return Err("missing --spy, --spxl, --bil, or --out".into());
    };
    checked_output_path(&out)?;
    h003::run([&spy, &spxl, &bil], &out)
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
