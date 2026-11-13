# Compute H005

`h005-compute` calculates the preregistered FX time-series momentum strategy on nine synthetic currency series, OECD-style monthly rates, and Tiingo-format SPY bars. It separates development and authorized holdout stages and writes observations, indicators, daily accounting, a summary, and a result manifest.

## Sub-features

- Dev excludes daily rows after 2024-10-31 and rate observations from 2024-10 onward before calculation. The manager's cutoff resolution is that observations at or after 2024-11-01 are outside dev; the 2024-10 monthly rate is published only from 2024-11-01 and is not needed by dev.
- Holdout calculates only 2024-11-01 through 2026-10-30. It requires an independently matching dev envelope, matching output and raw-input hashes, passing dev gates A/B/C in the ledger, a `holdout_open` entry, and no prior H005 holdout run.
- FX, rate, SPY, date, and accounting outputs are validated. Missing UK rates that prevent holdout computation stop the run and cannot produce a passing H gate.
- Optional first-release FRED-format inputs affect report-only vintage comparisons. Missing files and values are reported unavailable and do not change verdict gates.

## How to get to it (user POV)

Run `target/debug/h005-compute --fx-dir <fred-csv-dir> --rates <oecd.csv> --spy <spy.json> --stage dev --out results/<dev-run>`. A holdout run also needs `--dev-match <dev-run>/verdict.json --ledger ledger/ledger.jsonl`. Keep all source inputs and outputs private.

## Driving it with the shell

Run `.agents/skills/verify-mikiri/drive-h005.sh`. It generates exact-format synthetic inputs, drives Rust and Python for dev and authorized holdout, checks synthetic refusal paths, future-value invariance for FX/SPY/rates, and tampered-output detection. Evidence stays in `/tmp/mikiri-verify-h005-<id>/`; the driver's own `results/verify-h005-<id>` directory is removed at exit. An unexpected command failure copies available result files and the first 20 mismatches to `failure-debug/` before cleanup.

## Gotchas

Ledger `data_sha256` keys are the nine `<DEX_ID>.csv` names, `OECD.csv`, and `SPY.json`. These hashes cover the full original files. Dev outputs contain filtered observations, whose hash is in `SHA256SUMS` with the three calculation result files.

The driver also compares both stages with available synthetic first-release FX and rate files. Malformed optional files are input errors; absent files and empty values remain unavailable.

Synthetic outcomes verify software behavior only. They are not market results, must not be written to the ledger, and do not authorize opening a real holdout.
