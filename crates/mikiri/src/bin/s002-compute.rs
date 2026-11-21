use mikiri::s002;
use std::{error::Error, ffi::OsStr, path::PathBuf};

fn run() -> Result<String, Box<dyn Error>> {
    let mut paths: [Option<PathBuf>; 4] = std::array::from_fn(|_| None);
    let mut args = std::env::args_os().skip(1);
    let mut stage_given = false;
    let mut trace = None;
    while let Some(flag) = args.next() {
        let index = match flag.to_str() {
            Some("--cot") => 0,
            Some("--fred") => 1,
            Some("--spy") => 2,
            Some("--out") => 3,
            Some("--trace") => {
                if trace.is_some() {
                    return Err("duplicate --trace argument".into());
                }
                trace = Some(
                    args.next()
                        .ok_or("missing --trace configuration")?
                        .into_string()
                        .map_err(|_| "--trace configuration must be UTF-8")?,
                );
                continue;
            }
            Some("--holdout") => return Err("S002 holdout is disabled".into()),
            Some("--stage") if !stage_given => {
                stage_given = true;
                match args.next().as_deref().and_then(OsStr::to_str) {
                    Some("dev") => continue,
                    Some("holdout") => return Err("S002 holdout is disabled".into()),
                    _ => return Err("--stage must be dev".into()),
                }
            }
            _ => {
                return Err("expected --cot, --fred, --spy, --out, --trace, or --stage dev".into());
            }
        };
        if paths[index].is_some() {
            return Err("duplicate argument".into());
        }
        paths[index] = Some(PathBuf::from(args.next().ok_or("missing path argument")?));
    }
    let [Some(cot), Some(fred), Some(spy), Some(out)] = paths else {
        return Err("requires --cot <dir> --fred <dir> --spy <file> --out <dir>".into());
    };
    s002::run(&cot, &fred, &spy, &out, trace.as_deref())
}

fn main() {
    match run() {
        Ok(output) => println!("{output}"),
        Err(error) => {
            eprintln!("S002 input or computation failed: {error}");
            std::process::exit(1);
        }
    }
}
