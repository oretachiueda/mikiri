---
name: verify-mikiri
description: Drive Mikiri's command-line tools (h001-compute, h002-compute, h003-compute, stats/h001.py, stats/h002.py, stats/h003.py, fetch-tiingo) on synthetic inputs and keep proof of what they did. Use after a change to crates/ or stats/, or when a claim needs real behavior instead of test results. Needs no market data or token.
---

# Verify Mikiri

Mikiri has no server and no UI. This skill drives the command-line tools on synthetic inputs and keeps the output as proof.

This is the repository's only verification skill. Extend it when a feature changes. Do not create another one. The static checks are in `AGENTS.md` §7 and are not repeated here.

Read `features/README.md`, then the feature file that matches your change.

## Launch

Nothing stays running. Launch is one build from the repository root.

```bash
cargo build --locked -p mikiri --bins
```

The binaries are `target/debug/h001-compute`, `target/debug/h002-compute`, `target/debug/h003-compute`, and `target/debug/fetch-tiingo`. The Python batches need no build. `uv run --locked --project stats` creates its environment on first use.

## Doctor

```bash
command -v cargo uv shasum >/dev/null && git check-ignore -q results/x && git check-ignore -q data/x && test -x target/debug/h001-compute && test -x target/debug/h002-compute && test -x target/debug/fetch-tiingo && echo doctor-ok
```

`doctor-ok` means that the tools exist, the binaries are built, and git ignores the directories that a drive writes to. If the line prints nothing, fix that before you drive.

## Drive

```bash
.agents/skills/verify-mikiri/drive.sh
```

The script makes synthetic weekday bars from 2005-11-01 to 2026-10-30, runs `h001-compute` on them, then runs `stats/h001.py` on the result. It passes only when the summary has 250 main cycles, 120 recent cycles, and 1 reference cycle, and the Python recompute matches Rust with no mismatch. The last line is `PASS evidence=<directory>` or `FAIL evidence=<directory>`.

Run `.agents/skills/verify-mikiri/drive-h002.sh` to verify both H002 commands on synthetic monthly portfolios and keep proof in `/tmp/mikiri-verify-h002-<id>/`.

Run `.agents/skills/verify-mikiri/drive-h003.sh` to verify H003 compute and independent Python recompute on synthetic Tiingo-format SPY, SPXL, and BIL arrays and keep proof in `/tmp/mikiri-verify-h003-<id>/`.

Error paths and the other entry points are in `features/`. Each recipe there is a literal command with its expected output.

## Evidence

`drive.sh` writes to `/tmp/mikiri-verify-<id>/`.

- `transcript.txt` holds the build, `h001-compute`, and `stats/h001.py` steps, each with its output and exit code. A failed final count or match check prints to stderr instead.
- `bars.json` is the synthetic input.
- `summary.json` and `verdict.json` are copies of the Rust summary and the Python comparison.
- `SHA256SUMS` holds the hashes of the four result files.

Proof standards:

- Run the built binary or the batch script. A unit test of an internal function is not proof of the tool.
- Record the command, stdout, stderr, and exit code.
- For a failure path, also show that no output file appeared.
- Quote counts, hashes, and `match`. Synthetic values are not market results. Never write them to `ledger/`.

## Cleanup

`drive.sh` deletes its own run directory, `results/verify-<id>`, when it exits. If that directory already exists, the script stops before it writes anything. The evidence directory stays. The shell failure recipes in `features/` write no result files. The pytest mismatch recipe makes a `results/h001-test-*` directory and deletes it when it ends. Never delete another directory under `results/` or `data/`.

## Limits

- Never read `data/`, `live/`, `secrets/`, `.env`, or a token file during verification.
- The first `cargo build` and `uv run` can download crates and Python packages. No other step uses the network.
- A real `fetch-tiingo` download needs a token and network. Run it only when a human asks.
