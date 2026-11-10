use std::{error::Error, path::Path};

fn run() -> Result<String, Box<dyn Error>> {
    let (mut ports, mut sha256, mut out) = (None, None, None);
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let slot = match flag.as_str() {
            "--ports" => &mut ports,
            "--sha256" => &mut sha256,
            "--out" => &mut out,
            _ => return Err("expected --ports, --sha256 or --out".into()),
        };
        if slot.is_some() {
            return Err("duplicate argument".into());
        }
        *slot = Some(args.next().ok_or("missing argument value")?);
    }
    let out = out.ok_or("missing --out")?;
    let ports = ports.ok_or("missing --ports")?;
    let sha256 = sha256.ok_or("missing --sha256")?;
    mikiri::h002::run(Path::new(&ports), &sha256, Path::new(&out))
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
