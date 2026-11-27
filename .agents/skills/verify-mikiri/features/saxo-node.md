# Saxo SIM offline refusals

`saxo-node` accepts `check`, `sim-roundtrip`, and `forward` only for SIM. These checks exercise refusals before a browser or network connection using synthetic files.

## Sub-features

- All three commands reject live, missing arguments, invalid redirects and key files with permissions other than 600.
- An existing kill file stops all three commands before login.

## How to get to it (user POV)

Run `cargo build -p saxo-node --locked`. Use only temporary synthetic files. SIM login and orders are human-run and are not part of this offline recipe.

## Driving it with the shell

```bash
python3 - <<'PY'
from pathlib import Path
import subprocess, tempfile
with tempfile.TemporaryDirectory(prefix='saxo-node-offline-') as d:
    root = Path(d)
    app, account, kill = root/'app', root/'account', root/'kill'
    app.write_text('12345678901234567890123456789012')
    account.write_text('synthetic-account')
    app.chmod(0o600)
    account.chmod(0o600)
    for verb in ('check', 'sim-roundtrip'):
        base = ['target/debug/saxo-node', verb, '--env', 'sim', '--app-key-file', str(app), '--account-key-file', str(account), '--kill-file', str(kill), '--redirect', 'http://localhost:34567/cb']
        def run(args, expected):
            result = subprocess.run(args, capture_output=True, text=True, timeout=5)
            print(f'{verb} exit={result.returncode} stdout={result.stdout!r} stderr={result.stderr!r}')
            assert result.returncode == expected and not result.stdout
        run(['target/debug/saxo-node', verb, '--env', 'live'], 2)
        run(['target/debug/saxo-node', verb], 2)
        bad = base.copy(); bad[-1] = 'https://example.com/cb'; run(bad, 2)
        app.chmod(0o644); run(base, 2); app.chmod(0o600)
        account.chmod(0o644); run(base, 2); account.chmod(0o600)
        kill.touch(); run(base, 4); kill.unlink()
        assert not list(root.glob('*.jsonl'))
print('offline-refusals-ok')
PY
```

## Forward offline refusals

Run this from the repository root after building the default binary. The recipe uses temporary files and leaves the credential paths absent so the CLI cannot authenticate.

```bash
python3 - <<'PY_FORWARD'
import fcntl
import json
from pathlib import Path
import subprocess
import tempfile

with tempfile.TemporaryDirectory(prefix="saxo-forward-offline-") as d:
    root = Path(d)
    plan, journal, kill = (root / name for name in ("plan.toml", "journal.jsonl", "kill"))
    binary = ["target/debug/saxo-node", "forward"]
    sim = binary + [
        "--env", "sim", "--plan", str(plan), "--journal", str(journal),
        "--kill-file", str(kill), "--redirect", "http://localhost:34567/cb",
        "--app-key-file", str(root / "absent-app"),
        "--account-key-file", str(root / "absent-account"),
    ]
    fixture = Path("crates/saxo-node/tests/cli.rs").read_text()
    synthetic = fixture.split('const FORWARD_PLAN: &str = r#"', 1)[1].split('"#;', 1)[0]

    def refuse(label, args, code, message):
        result = subprocess.run(args, capture_output=True, text=True, timeout=5)
        assert (result.returncode, result.stdout) == (code, ""), result
        assert message in result.stderr, result
        print(f"{label} exit={result.returncode} stderr={result.stderr.strip()}")

    refuse("LIVE", binary + ["--env", "live"], 2, "only sim is allowed")
    plan.write_text(synthetic)
    plan.chmod(0o644)
    refuse("plan mode 644", sim, 2, "plan permissions must be 600")
    plan.chmod(0o600)
    plan.write_text(synthetic.replace('expires = "2099-01-01"', 'expires = "1969-01-01"'))
    refuse("expired plan", sim, 2, "expired plan")
    plan.write_text(synthetic)
    row = {
        "version": 1, "event_id": "A-20980101", "leg": "A",
        "tokyo_date": "2098-01-01", "action": "declare", "status": "scheduled",
        "recorded_at": "2098-01-01T03:00:00Z",
    }
    mixed = "".join(
        json.dumps(dict(row, env=env, idem_key=f"A-20980101-ENTRY-{env}")) + "\n"
        for env in ("SIM", "LIVE")
    )
    journal.write_text(mixed)
    refuse("mixed journal environments", sim, 2, "invalid version or environment")
    assert journal.read_text() == mixed
    journal.unlink()
    kill.touch()
    refuse("kill file", sim, 4, "kill file present")
    kill.unlink()
    with journal.with_suffix(".jsonl.lock").open("w") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        refuse("journal already locked", sim, 6, "journal lock unavailable")
    assert not journal.exists()
print("forward-offline-refusals-ok")
PY_FORWARD
```

## Forward with a fake broker

The CLI tests cover LIVE refusal and invalid plan or journal preflight before login. The forward strategy tests run SIM against a loopback Saxo server with a real Nautilus LiveNode. They cover the two-leg trading day, an already completed restart, a working stop restored on restart, and a broker stop fill with zero market exit orders.

```bash
cargo nextest run -p saxo-node -p forward -p saxo --locked
cargo test -p saxo-node --locked forward_export_sample -- --ignored --nocapture
```

The ignored test writes the two-leg synthetic journal to `/tmp/r82-5c-sample.jsonl`.

## Gotchas

Do not use real credentials or connect to Saxo in this recipe. Login requires macOS `open`; the SIM round trip requires separate human verification.
