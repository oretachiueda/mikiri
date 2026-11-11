# IB connection probe

`ib-probe` reads server version, managed-account count, open-position count, and the SPY contract ID from localhost TWS, then disconnects. It prints only those fields, makes no orders, and writes no files.

## Sub-features

- `--port <u16>` defaults to 7497; zero is rejected.
- `--client-id <u32>` defaults to 7; values above 2147483647 are rejected by the signed IB API boundary.
- Host is fixed to 127.0.0.1. Duplicate and unknown arguments fail.
- The connection has a five-second timeout, no reconnection, and a fifteen-second overall limit.
- Wire-recording environment variables must be unset. Errors never include broker response text.

## How to get to it (user POV)

Run `cargo build --locked -p ib-probe` from the repository root. The binary is `target/debug/ib-probe`. No token or market data is needed. The following offline recipes never use ports 7497 or 4002.

## Driving it with the shell

```bash
target/debug/ib-probe --port 65536
```

Expected: exit 1, empty stdout, stderr `ib-probe: invalid port`.

```bash
target/debug/ib-probe --client-id 2147483648
```

Expected: exit 1, empty stdout, stderr `ib-probe: client ID exceeds IB API range`.

```bash
IBAPI_RECORDING_DIR= target/debug/ib-probe --port 1
```

Expected: exit 1, empty stdout, stderr `ib-probe: IBAPI recording environment must be unset`.

```bash
python3 - <<'PYCODE'
import os
import socket
import subprocess
with socket.socket() as listener:
    listener.bind(("127.0.0.1", 0))
    port = listener.getsockname()[1]
assert port not in (7497, 4002)
env = dict(os.environ)
for name in ("IBAPI_RECORDING_DIR", "IBAPI_RAW_CAPTURE_DIR"):
    env.pop(name, None)
r = subprocess.run(["target/debug/ib-probe", "--port", str(port)], env=env,
                   capture_output=True, text=True, timeout=20)
print(f"port={port} exit={r.returncode} stdout={r.stdout!r} stderr={r.stderr!r}")
assert r.returncode == 1 and r.stdout == ""
assert r.stderr == "ib-probe: connection failed\n"
PYCODE
```

Expected: exit 0 from the recipe and the probe's exit 1 recorded. These commands write no output files. Keep stdout, stderr, and exit codes as evidence.

## Gotchas

The separate crate keeps Nautilus out of the research binaries. A real success requires human-run TWS with API enabled, Read-Only API on, localhost only, and port 7497. The verification skill does not connect to it. Only run a real probe with explicit authorization. Never record raw broker traffic, account identifiers, or balances. TWS must support the protocol required by the exactly pinned adapter. Simulated connectivity does not approve live trading.
