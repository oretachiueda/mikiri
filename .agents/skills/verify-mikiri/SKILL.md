---
name: verify-mikiri
description: Drive Mikiri's command-line tools (h001-compute, stats/h001.py, fetch-tiingo) on synthetic bars and keep proof of what they did. Use after a change to crates/ or stats/, or when a claim needs real behavior instead of test results. Needs no market data, token, or network.
---

# Verify Mikiri

Mikiri has no server and no UI. A user runs two Rust binaries and one Python batch from the repository root. This skill drives them on synthetic bars and keeps the output as proof.

This is the repository's only verification skill. Extend it when a feature changes. Do not create another one. The static checks are in `AGENTS.md` §7 and are not repeated here.

Read `features/README.md`, then the feature file that matches your change.

## Launch

Nothing stays running. Launch is one build from the repository root.

```bash
cargo build --locked -p mikiri --bins
```

The binaries are `target/debug/h001-compute` and `target/debug/fetch-tiingo`. The Python batch needs no build. `uv run --locked --project stats` creates its environment on first use.

## Doctor

```bash
command -v cargo uv shasum >/dev/null && git check-ignore -q results/x && git check-ignore -q data/x && test -x target/debug/h001-compute && test -x target/debug/fetch-tiingo && echo doctor-ok
```

`doctor-ok` means that the tools exist, the binaries are built, and git ignores the directories that a drive writes to. If the line prints nothing, fix that before you drive.

## Drive

```bash
.agents/skills/verify-mikiri/drive.sh
```

The script makes synthetic weekday bars from 2005-11-01 to 2026-10-30, runs `h001-compute` on them, then runs `stats/h001.py` on the result. It passes only when the summary has 250 main cycles, 120 recent cycles, and 1 reference cycle, and the Python recompute matches Rust with no mismatch. The last line is `PASS evidence=<directory>` or `FAIL evidence=<directory>`.

Error paths and the other entry points are in `features/`. Each recipe there is a literal command with its expected output.

## Evidence

`drive.sh` writes to `/tmp/mikiri-verify-<id>/`.

- `transcript.txt` holds each command, its output, and its exit code.
- `bars.json` is the synthetic input.
- `summary.json` and `verdict.json` are copies of the Rust summary and the Python comparison.
- `SHA256SUMS` holds the hashes of the four result files.

Proof standards:

- Run the built binary or the batch script. A unit test of an internal function is not proof of the tool.
- Record the command, stdout, stderr, and exit code.
- For a failure path, also show that no output file appeared.
- Quote counts, hashes, and `match`. Synthetic values are not market results. Never write them to `ledger/`.

## Cleanup

`drive.sh` deletes its own run directory, `results/verify-<id>`, when it exits. The evidence directory stays. The failure recipes in `features/` write no result files. Never delete another directory under `results/` or `data/`.

## Limits

- Never read `data/`, `live/`, `secrets/`, `.env`, or a token file during verification.
- A real `fetch-tiingo` download needs a token and network. Run it only when a human asks.
