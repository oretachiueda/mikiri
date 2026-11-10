# Compute H001

`h001-compute` reads daily bars in Tiingo JSON form, computes the H001 cycles and summary, and writes three result files under `results/`. It prints one line with the file hashes and the cycle counts, and never prints a verdict value.

## Sub-features

- `compute-success` writes `bars.parquet`, `cycles.parquet`, and `summary.json`, and prints their hashes with `main_n` and `recent_n`.
- `compute-coverage` rejects bars that do not cover the test period and writes nothing.
- `compute-out-guard` rejects an output path that is not under `results/`.
- `compute-args` rejects missing, unknown, and duplicate arguments.

## How to get to it (user POV)

- Run `target/debug/h001-compute --bars <bars.json> --out results/<run>` from the repository root.

## Driving it with the shell

Preconditions:

- The baseline in `README.md` holds.
- `/tmp/mikiri-one-bar.json` exists. Create it with `printf '[{"date":"2026-10-30T00:00:00Z","close":100,"divCash":0,"splitFactor":1}]' > /tmp/mikiri-one-bar.json`.

- **Success.** Run `.agents/skills/verify-mikiri/drive.sh`. The last line starts with `PASS`. In `transcript.txt` the `h001-compute` step prints one line that holds three `sha256=` values and ends with `main_n=250 recent_n=120`. That line contains no `gate`, `psr`, or `mean`.
- **Short coverage.** Run `target/debug/h001-compute --bars /tmp/mikiri-one-bar.json --out results/verify-short`. Stderr is `bars must start on or before 2005-12-29`, the exit code is `1`, and `results/verify-short` does not exist.
- **Output guard.** Run `target/debug/h001-compute --bars /tmp/mikiri-one-bar.json --out out`, then the same command with `--out results/../x`. Stderr is `output path must be under results/` and the exit code is `1` both times.
- **Arguments.** Run `target/debug/h001-compute`, then `target/debug/h001-compute --nope`, then `target/debug/h001-compute --bars a --bars b --out results/x`. Stderr is `missing --out`, `expected --bars or --out`, and `duplicate argument`. Each exit code is `1`.
- **Harness.** Run `cargo nextest run --locked --test h001_cli`. It runs the built binary and reads the Parquet files back, which a single shell run does not show.

## Gotchas

- `--out` is relative to the current directory and its first part must be `results`. Outside the repository root the files land in the wrong place.
- Stdout stays one line without verdict values. Read verdict values only from `summary.json`.
- The synthetic summary is not a market result. Do not quote its means or PSR as findings.
- Remove `/tmp/mikiri-one-bar.json` after the run.
