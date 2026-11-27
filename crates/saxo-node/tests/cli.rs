use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicUsize, Ordering},
};
static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Fixture {
    dir: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "saxo-node-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&dir).expect("create fixture");
        let f = Self { dir };
        f.write("app", "12345678901234567890123456789012", 0o600);
        f.write("account", "account-key", 0o600);
        f
    }
    fn write(&self, name: &str, content: &str, mode: u32) {
        let path = self.dir.join(name);
        fs::write(&path, content).expect("write fixture");
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).expect("chmod");
    }
    fn command_with_redirect(&self, redirect: &str) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_saxo-node"));
        cmd.args([
            "check",
            "--env",
            "sim",
            "--redirect",
            redirect,
            "--app-key-file",
        ])
        .arg(self.dir.join("app"))
        .arg("--account-key-file")
        .arg(self.dir.join("account"))
        .arg("--kill-file")
        .arg(self.dir.join("kill"));
        cmd
    }
    fn command(&self) -> Command {
        self.command_with_redirect("http://localhost:34567/cb")
    }
    fn status(&self) -> i32 {
        self.command()
            .output()
            .expect("run")
            .status
            .code()
            .expect("exit code")
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}
#[test]
fn live_is_rejected_before_network() {
    let output = Command::new(env!("CARGO_BIN_EXE_saxo-node"))
        .args(["check", "--env", "live"])
        .output()
        .expect("run");
    assert_eq!(output.status.code(), Some(2));
}
#[test]
fn insecure_key_files_are_rejected() {
    let f = Fixture::new();
    f.write("app", "12345678901234567890123456789012", 0o644);
    assert_eq!(f.status(), 2);
    f.write("app", "12345678901234567890123456789012", 0o600);
    f.write("account", "secret", 0o644);
    assert_eq!(f.status(), 2);
}
#[test]
fn empty_and_whitespace_account_keys_are_rejected() {
    let f = Fixture::new();
    for value in ["", "  \n", "abc def", "abc\ndef"] {
        f.write("account", value, 0o600);
        assert_eq!(f.status(), 2);
    }
}
#[test]
fn kill_file_stops_before_login() {
    let f = Fixture::new();
    f.write("kill", "", 0o600);
    assert_eq!(f.status(), 4);
}
#[test]
fn invalid_redirect_is_rejected() {
    let f = Fixture::new();
    assert_eq!(
        f.command_with_redirect("https://example.com/cb")
            .output()
            .expect("run")
            .status
            .code(),
        Some(2)
    );
}
#[test]
fn missing_arguments_are_rejected() {
    for args in [&[][..], &["check"][..], &["check", "--env", "sim"][..]] {
        assert_eq!(
            Command::new(env!("CARGO_BIN_EXE_saxo-node"))
                .args(args)
                .output()
                .expect("run")
                .status
                .code(),
            Some(2)
        );
    }
}
#[test]
fn sim_roundtrip_shares_prelogin_validation() {
    let f = Fixture::new();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_saxo-node"));
    cmd.args([
        "sim-roundtrip",
        "--env",
        "sim",
        "--redirect",
        "http://localhost:34567/cb",
        "--app-key-file",
    ])
    .arg(f.dir.join("app"))
    .arg("--account-key-file")
    .arg(f.dir.join("account"))
    .arg("--kill-file")
    .arg(f.dir.join("kill"));
    f.write("kill", "", 0o600);
    assert_eq!(cmd.output().expect("run").status.code(), Some(4));
    fs::remove_file(f.dir.join("kill")).expect("remove kill");
    f.write("account", "bad key", 0o600);
    assert_eq!(cmd.output().expect("run").status.code(), Some(2));
}
#[test]
fn login_error_includes_cause_without_secrets() {
    let f = Fixture::new();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("address").port();
    let output = f
        .command_with_redirect(&format!("http://localhost:{port}/cb"))
        .output()
        .expect("run");
    assert_eq!(output.status.code(), Some(3));
    let stderr = String::from_utf8(output.stderr).expect("utf8");
    assert!(stderr.contains("login failed:"));
    assert!(stderr.contains("Address already in use") || stderr.contains("address in use"));
    assert!(!stderr.contains("12345678901234567890123456789012"));
    assert!(!stderr.contains("account-key"));
    assert!(!stderr.contains("http://"));
}

const FORWARD_PLAN: &str = r#"
instrument_id = "SYNTH.SAXO"
price_increment_milli = 5
utc_offset_seconds = 3600
base_capital_milli = 1000000
halt_threshold_milli = 100
expires = "2099-01-01"
event_dates = ["2098-01-01"]
[blackout]
start = "01:00:00"
end = "02:00:00"
[sizing]
unit_cap = 100
absolute_cap = 200
multiplier = 2
lot_size = 10
minimum_units = 10
[[legs]]
name = "A"
direction = "buy"
max_spread_milli = 9
[legs.entry]
start = "03:10:00"
end = "03:20:00"
[legs.exit]
start = "04:10:00"
end = "04:20:00"
[legs.stop]
enabled = false
width_ppm = 0
[legs.order_limits]
entry = 1
stop = 0
exit = 1
cancel = 1
"#;
impl Fixture {
    fn forward_command(&self) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_saxo-node"));
        c.args([
            "forward",
            "--env",
            "sim",
            "--dry-run",
            "--redirect",
            "http://localhost:34567/cb",
            "--plan",
        ])
        .arg(self.dir.join("plan"))
        .arg("--journal")
        .arg(self.dir.join("journal"))
        .arg("--app-key-file")
        .arg(self.dir.join("absent-app"))
        .arg("--account-key-file")
        .arg(self.dir.join("absent-account"))
        .arg("--kill-file")
        .arg(self.dir.join("kill"));
        c
    }
    fn forward_output(&self) -> (i32, String) {
        let output = self.forward_command().output().expect("run");
        (
            output.status.code().expect("exit"),
            String::from_utf8(output.stderr).expect("utf8"),
        )
    }
}
#[test]
fn forward_rejects_live_and_preflights_without_dry_run() {
    let output = Command::new(env!("CARGO_BIN_EXE_saxo-node"))
        .args(["forward", "--env", "live"])
        .output()
        .expect("run");
    assert_eq!(output.status.code(), Some(2));
    let output = Command::new(env!("CARGO_BIN_EXE_saxo-node"))
        .args([
            "forward",
            "--env",
            "sim",
            "--redirect",
            "http://localhost:34567/cb",
            "--plan",
            "missing",
            "--journal",
            "missing",
            "--app-key-file",
            "missing",
            "--account-key-file",
            "missing",
            "--kill-file",
            "missing",
        ])
        .output()
        .expect("run");
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("forward preflight failed"));
}
#[test]
fn forward_rejects_bad_plan_before_reading_credentials() {
    let f = Fixture::new();
    let (code, err) = f.forward_output();
    assert_eq!(code, 2);
    assert!(err.contains("forward preflight failed"));
    f.write("plan", FORWARD_PLAN, 0o644);
    let (code, err) = f.forward_output();
    assert_eq!(code, 2);
    assert!(err.contains("permissions"));
    f.write(
        "plan",
        &FORWARD_PLAN.replace("2099-01-01", "1969-01-01"),
        0o600,
    );
    let (code, err) = f.forward_output();
    assert_eq!(code, 2);
    assert!(err.contains("expired plan"));
    f.write("plan", &format!("{FORWARD_PLAN}\nunknown = true\n"), 0o600);
    let (code, err) = f.forward_output();
    assert_eq!(code, 2);
    assert!(err.contains("unknown field"));
}
#[test]
fn forward_rejects_kill_before_preflight_and_login() {
    let f = Fixture::new();
    f.write("kill", "", 0o600);
    let (code, err) = f.forward_output();
    assert_eq!(code, 4);
    assert!(err.contains("kill file present"));
}
#[test]
fn forward_rejects_corrupt_and_mixed_environment_journals_before_login() {
    let f = Fixture::new();
    f.write("plan", FORWARD_PLAN, 0o600);
    f.write("journal", "{broken}\n", 0o600);
    let (code, err) = f.forward_output();
    assert_eq!(code, 2);
    assert!(err.contains("journal:"));
    let other = serde_json::json!({"version":1,"env":"LIVE","event_id":"A-20980101","leg":"A","tokyo_date":"2098-01-01","action":"declare","idem_key":"A-20980101-ENTRY-LIVE","status":"scheduled","recorded_at":"2098-01-01T03:15:00+01:00"});
    f.write("journal", &format!("{other}\n"), 0o600);
    let (code, err) = f.forward_output();
    assert_eq!(code, 2);
    assert!(err.contains("environment"));
    f.write("journal", "", 0o600);
    let (code, err) = f.forward_output();
    assert_eq!(code, 2);
    assert!(err.contains("invalid app key file"));
}
#[test]
fn forward_rejects_invalid_until() {
    let f = Fixture::new();
    f.write("plan", FORWARD_PLAN, 0o600);
    let output = f
        .forward_command()
        .args(["--until", "25:90:00"])
        .output()
        .expect("run");
    assert_eq!(output.status.code(), Some(2));
}
