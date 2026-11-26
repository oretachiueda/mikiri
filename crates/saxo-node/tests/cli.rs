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
