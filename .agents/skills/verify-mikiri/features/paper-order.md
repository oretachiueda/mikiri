# Guarded demo orders

`paper-order` reconciles the SPY demo position before recording and submitting one market DAY order through Nautilus. `status` reads broker state through the shared IB client without constructing a LiveNode, and works with the TWS Read-Only API enabled. The connection is fixed to 127.0.0.1:7497, client ID 8 (the probe uses 7), strategy `demo`, symbol `SPY`.

## Sub-features

- Required `--config` and `--journal`; buy and sell also require positive integer `--shares`.
- Missing, invalid, expired, or killed limits stop before connection.
- Orders require New York weekdays 09:35–15:55, one D-prefixed managed account, matching journal position, no open SPY order, and known finite USD daily P&L.
- Durable journal keys prevent same-day duplicate intents. Rejected and pending keys remain used.
- Timeout never cancels or resubmits. Shutdown never closes a position.
- Buy/sell use a LiveNode and require Read-Only API OFF. TWS error 321 reports `refused: TWS API is read-only`; status never requests order IDs.
- Status prints `daily_pnl_usd=unknown` when P&L is unavailable, invalid, or times out. Position, journal, and open-order read failures still refuse. Buy/sell reject unknown P&L.
- Errors keep one stdout line and print the full error chain on stderr with account identifiers masked.

## How to get to it (user POV)

Build with `cargo build --locked -p paper-order`. Use temporary synthetic config and journal paths outside the repository for offline verification. A real connection is human-run; this skill never connects to TWS. Status needs Read-Only API ON or OFF; buy/sell need it OFF and explicit human authorization.

## Driving it with the shell

```bash
target/debug/paper-order status
```

Expected: exit 1, stdout `refused: missing --config`, stderr `missing --config`; no files written.

```bash
python3 - <<'PYCODE'
from pathlib import Path
import subprocess
import tempfile
with tempfile.TemporaryDirectory(prefix="mikiri-paper-offline-") as tmp:
    root = Path(tmp)
    config, journal = root / "limits.toml", root / "journal.jsonl"
    command = ["target/debug/paper-order", "status", "--config", str(config), "--journal", str(journal)]
    for expected in ["refused: config file is missing", "refused: kill switch exists", "refused: config has expired"]:
        if "kill" in expected:
            (root / "KILL").touch()
        elif "expired" in expected:
            (root / "KILL").unlink()
            config.write_text('expires_on = "2000-01-01"\nsymbols = ["SPY"]\nmax_position_shares = 1\nmax_daily_orders = 4\nmax_daily_loss_usd = 50.0\n')
        result = subprocess.run(command, capture_output=True, text=True, timeout=5)
        print(f"exit={result.returncode} stdout={result.stdout!r} stderr={result.stderr!r}")
        assert result.returncode == 1 and result.stdout.strip() == expected and result.stderr.strip() == expected.removeprefix("refused: ")
        assert not journal.exists()
PYCODE
```

The recipe exits 0 only if all failures occur before journal creation. Keep the output and exit codes as evidence. Market-hours refusal can be driven with a valid temporary config and a buy command only when the current New York time is outside the window; otherwise skip that real CLI path and record why. Never force a clock or connect during a skill run.

## Gotchas

Never drive buy or sell against TWS from this skill. The startup provider uses Raw symbology for `SPY=STK.SMART`; the default Simplified method resolves a different instrument ID. Account IDs and balances must never appear in output or wire recordings. Exchange holidays are not modeled: a holiday order may remain pending until timeout. An empty journal does not prove a flat broker position; `status` reports the comparison. The daily-P&L stream may not emit its first update within 20 seconds; status remains useful but orders fail closed. Account-summary equity change is not used as trading P&L because deposits, withdrawals, and non-trading valuation changes can affect it. A recorded intent is not a fill: a mismatch after rejection or partial fill needs human reconciliation. Simulated trading does not authorize a real account.
