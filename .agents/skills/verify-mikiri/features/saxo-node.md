# Saxo SIM offline refusals

`saxo-node` accepts `check` and `sim-roundtrip` only for SIM. These checks exercise refusals before a browser or network connection using synthetic files.

## Sub-features

- Both commands reject live, missing arguments, invalid redirects and key files with permissions other than 600.
- An existing kill file stops either command before login.

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

## Gotchas

Do not use real credentials or connect to Saxo in this recipe. Login requires macOS `open`; the SIM round trip requires separate human verification.
