# Paper recorder

`paper-recorder` records a fixed one-share toy book from a local Tiingo-format file, verifies its hash chain, and reports data alerts without placing orders or using a network.

## Sub-features

- `record` appends new dates, rejects revisions, missing XNYS sessions, stale inputs, and ticker mismatches.
- Repeating the same input appends nothing.
- `verify` checks the full chain; a damaged chain exits 3.

## How to get to it (user POV)

- Run `target/debug/paper-recorder record --bars <file> --log <jsonl> --ticker <ticker> --today <YYYY-MM-DD>`.
- Run `target/debug/paper-recorder verify --log <jsonl>`.

## Driving it with the shell

Use a new empty temporary directory and synthetic input only. Keep the command, stdout, stderr, exit code, and file hashes in the evidence directory. Run from the repository root after the baseline build:

```bash
proof=$(mktemp -d /tmp/mikiri-verify-recorder-XXXXXX)
printf '[{"date":"2024-01-02T00:00:00Z","close":10,"divCash":0,"splitFactor":1},{"date":"2024-01-03T00:00:00Z","close":11,"divCash":0,"splitFactor":1}]\n' > "$proof/bars.json"
target/debug/paper-recorder record --bars "$proof/bars.json" --log "$proof/log.jsonl" --ticker TEST --today 2024-01-03
target/debug/paper-recorder record --bars "$proof/bars.json" --log "$proof/log.jsonl" --ticker TEST --today 2024-01-03
target/debug/paper-recorder verify --log "$proof/log.jsonl"
shasum -a 256 "$proof/bars.json" "$proof/log.jsonl"
```

Expected: exit 0 for each recorder command, with `appended=2`, `appended=0`, and `lines=2`, respectively; the last hash is the same on all three. For a failure path, change a byte in a copy of the log, run `verify` on that copy, and confirm stderr `invalid log chain` and exit 3. Record the copy's hash before and after to show it was not changed.

## Gotchas

- Never drive with real market data or read a token. The recorder itself does not fetch data.
- Logs contain closes. Keep real logs private and never add them to this repository or ledger.
