# Guarded demo orders

`paper-order` reconciles the SPY demo position before recording and submitting one market DAY order through Nautilus. `status` reads broker state through the shared IB client without constructing a LiveNode, and works with the TWS Read-Only API enabled. The connection is fixed to 127.0.0.1:7497, client ID 8 (the probe uses 7), strategy `demo`, symbol `SPY`.

## Sub-features

- All commands require `--config`, `--journal`, and `--baseline`; buy and sell also require positive integer `--shares`.
- Missing, invalid, expired, or killed limits stop before connection.
- Orders require New York weekdays 09:35–15:55, one D-prefixed managed account, matching journal position, no open SPY order, and known finite USD daily P&L.
- Any SPY stock/USD row with a missing or different account identity refuses the read; a disconnected open-order stream also refuses.
- Durable journal keys prevent same-day duplicate intents. Rejected and pending keys remain used.
- Timeout never cancels or resubmits. Shutdown never closes a position.
- Buy/sell use a LiveNode and require Read-Only API OFF. TWS error 321 reports `refused: TWS API is read-only`; status never requests order IDs.
- Daily P&L is current verified USD `EquityWithLoanValue` minus that UTC day's baseline. The account summary must contain exactly one finite USD `EquityWithLoanValue` row. After the journal opens, status atomically initializes a missing baseline or rolls an older baseline forward to today's equity; either case reports P&L as `0.00`. A same-day baseline is subtracted from current equity. Missing current equity makes P&L unknown. A baseline dated after today, corrupt JSON, or unknown JSON fields also makes P&L unknown and emits a masked stderr diagnostic; corrupt baseline data is left untouched. The stored baseline contains only `day` and `equity_usd`; USD and row uniqueness are validated from the current broker response, not the file. Unknown P&L blocks orders.
- Baseline diagnostics are masked on stderr. Account IDs and raw balances never appear in output.
- Stored terminal outcomes survive shutdown errors. Shutdown errors go to masked stderr and force exit 1. The journal stays locked until node disposal, including early failures.
- Rejected/denied/canceled/expired events report their masked reason; if the protocol has no reason field, the status is used.
- Errors keep one stdout line and print the full error chain on stderr with account identifiers masked.

## How to get to it (user POV)

Build with `cargo build --locked -p paper-order`. All commands require `--config`, `--journal`, and `--baseline`; buy/sell additionally require `--shares`. Use temporary synthetic paths outside the repository for offline verification. A real connection is human-run; this skill never connects to TWS. Status needs Read-Only API ON or OFF; buy/sell need it OFF and explicit human authorization.

## Driving it with the shell

```bash
python3 - <<'PYCODE'
from pathlib import Path
import subprocess
import tempfile
with tempfile.TemporaryDirectory(prefix="mikiri-paper-offline-") as tmp:
    root = Path(tmp)
    config, journal, baseline = root / "limits.toml", root / "journal.jsonl", root / "baseline.json"
    command = ["target/debug/paper-order", "status", "--config", str(config), "--journal", str(journal), "--baseline", str(baseline)]
    without_baseline = ["target/debug/paper-order", "status", "--config", str(config), "--journal", str(journal)]
    missing_baseline = subprocess.run(without_baseline, capture_output=True, text=True, timeout=5)
    print(f"exit={missing_baseline.returncode} stdout={missing_baseline.stdout!r} stderr={missing_baseline.stderr!r}")
    assert missing_baseline.returncode == 1 and missing_baseline.stdout.strip() == "refused: missing --baseline" and missing_baseline.stderr.strip() == "missing --baseline"
    assert not journal.exists() and not baseline.exists()
    for expected in ["refused: config file is missing", "refused: kill switch exists", "refused: config has expired"]:
        if "kill" in expected:
            (root / "KILL").touch()
        elif "expired" in expected:
            (root / "KILL").unlink()
            config.write_text('expires_on = "2000-01-01"\nsymbols = ["SPY"]\nmax_position_shares = 1\nmax_daily_orders = 4\nmax_daily_loss_usd = 50.0\n')
        result = subprocess.run(command, capture_output=True, text=True, timeout=5)
        print(f"exit={result.returncode} stdout={result.stdout!r} stderr={result.stderr!r}")
        assert result.returncode == 1 and result.stdout.strip() == expected and result.stderr.strip() == expected.removeprefix("refused: ")
        assert not journal.exists() and not baseline.exists()
PYCODE
```

The recipe exits 0 only if all failures occur before journal or baseline creation. Keep the output and exit codes as evidence. Market-hours refusal can be driven with valid temporary config, journal, and baseline paths plus a buy command only when the current New York time is outside the window; otherwise skip that real CLI path and record why. Never force a clock or connect during a skill run.

## Gotchas

Never drive buy or sell against TWS from this skill. The startup provider uses Raw symbology for `SPY=STK.SMART`; the default Simplified method resolves a different instrument ID. Account IDs and balances must never appear in output or wire recordings. Exchange holidays are not modeled: a holiday order may remain pending until timeout. An empty journal does not prove a flat broker position; `status` reports the comparison. The first-seen equity baseline misses losses from earlier in that UTC day, and deposits or withdrawals distort the P&L difference. Set `KILL` on transfer days; because the kill switch also blocks status, verify the settled baseline with `status` right after the market opens once the switch is cleared, then revisit this baseline before live use. Dedicated P&L stream updates are not used for this field; unavailable summary values still block orders. A recorded intent is not a fill: a mismatch after rejection or partial fill needs human reconciliation. Simulated trading does not authorize a real account.
