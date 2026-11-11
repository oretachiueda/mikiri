use std::{net::TcpListener, process::Command};

#[test]
fn closed_port_fails_without_disclosing_details() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    assert!(![7497, 4002].contains(&port));
    drop(listener);
    let output = Command::new(env!("CARGO_BIN_EXE_ib-probe"))
        .env_remove("IBAPI_RECORDING_DIR")
        .env_remove("IBAPI_RAW_CAPTURE_DIR")
        .args(["--port", &port.to_string()])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert_eq!(output.stderr, b"ib-probe: connection failed\n");
}

#[test]
fn refuses_wire_recording_before_connection() {
    for name in ["IBAPI_RECORDING_DIR", "IBAPI_RAW_CAPTURE_DIR"] {
        let output = Command::new(env!("CARGO_BIN_EXE_ib-probe"))
            .env(name, "")
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        assert_eq!(
            output.stderr,
            b"ib-probe: IBAPI recording environment must be unset\n"
        );
    }
}
