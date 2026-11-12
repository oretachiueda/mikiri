use mikiri::{h001::checked_output_path, h004};
use std::{error::Error, path::PathBuf};

fn run() -> Result<String, Box<dyn Error>> {
    let mut values: [Option<PathBuf>; 5] = [None, None, None, None, None];
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let index = match flag.as_str() {
            "--spy" => 0,
            "--sso" => 1,
            "--out" => 2,
            "--stage" => 3,
            "--dev-verdict" => 4,
            _ => return Err("expected --spy, --sso, --out, --stage, or --dev-verdict".into()),
        };
        if values[index].is_some() {
            return Err("duplicate argument".into());
        }
        values[index] = Some(PathBuf::from(args.next().ok_or("missing argument value")?));
    }
    let [Some(spy), Some(sso), Some(out), Some(stage), verdict] = values else {
        return Err("missing --spy, --sso, --out, or --stage".into());
    };
    let stage = h004::Stage::parse(stage.to_str().ok_or("invalid stage")?)?;
    checked_output_path(&out)?;
    h004::run([&spy, &sso], &out, stage, verdict.as_deref())
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
