# Compute H003

`h003-compute` reads Tiingo-format daily-bar JSON arrays for SPY, SPXL, and BIL, computes the preregistered H003 strategy, and writes `bars.parquet`, `days.parquet`, and `summary.json` under `results/`. It prints file hashes and day counts without printing a gate, PSR, wealth, or drawdown result.

## Sub-features

- `compute-success` writes the three run files and reports their hashes with `main_n` and `recent_n`; `n_cumulative` is recorded in `summary.json`.
- `compute-input` reads Tiingo daily-bar JSON arrays, using `date`, `close`, `divCash`, and `splitFactor`. Synthetic fixtures include all 13 Tiingo response fields and types. The command rejects invalid used values, insufficient signal history, and missing main-window SPXL or BIL dates.
- `compute-psr-halt` and `compute-tax-sale-halt` fail closed when the preregistered calculations cannot continue.
- `compute-out-guard` rejects an output path outside the accepted `results/` syntax before writing.

## How to get to it (user POV)

- Run `target/debug/h003-compute --spy <spy.json> --spxl <spxl.json> --bil <bil.json> --out results/<run>` from the repository root.
- Each input is a JSON array in Tiingo daily-bar format. Keep the original input files private; the run output contains per-day values.

## Driving it with the shell

The baseline in [README.md](README.md) holds. Run `.agents/skills/verify-mikiri/drive-h003.sh`. It generates synthetic 13-key Tiingo arrays, runs the Rust command and Python recompute, checks the counts, gates, and match, and checks the three-year tax-loss expiry and carry behavior. It prints the evidence path and ends with the exact line `PASS`. The transcript captures each command, stdout, stderr, and exit code; `SHA256SUMS` covers the synthetic inputs and run outputs.

## Gotchas

- The main window is 2015-11-02 through 2026-10-30; data after the end date is ignored. Input history before the window is required for the 200-day signal.
- The synthetic run exercises splits, dividends, large gains and losses, and both date windows. It verifies tool behavior and implementation agreement, not a market result.
- Keep the evidence directory. The driver removes only its uniquely named `results/verify-h003-*` run directory.
