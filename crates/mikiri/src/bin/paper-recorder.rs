use std::path::Path;
fn run() -> Result<String, (i32, String)> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let value = |flag: &str| -> Result<&str, (i32, String)> {
        args.iter()
            .position(|s| s == flag)
            .and_then(|i| args.get(i + 1))
            .map(String::as_str)
            .ok_or((1, format!("missing {flag}")))
    };
    match args.first().map(String::as_str) {
        Some("verify") if args.len() == 3 => mikiri::paper_recorder::verify(Path::new(value("--log")?)),
        Some("record") if args.len() == 9 => mikiri::paper_recorder::record(Path::new(value("--bars")?), Path::new(value("--log")?), value("--ticker")?, value("--today")?),
        _ => Err((1,"usage: paper-recorder record --bars FILE --log FILE --ticker T --today YYYY-MM-DD | verify --log FILE".into())),
    }
}
fn main() {
    match run() {
        Ok(s) => println!("{s}"),
        Err((code, message)) => {
            eprintln!("{message}");
            std::process::exit(code);
        }
    }
}
