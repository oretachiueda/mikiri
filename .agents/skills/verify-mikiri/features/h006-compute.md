# Compute H006

`h006-compute` reads HistData EUR/USD minute ZIPs and Tiingo SPY JSON, computes the registered two-leg strategy and matched random control, and writes results for dev or an authorized holdout. Use synthetic inputs only for verification. The clock check prints before leg calculations. A failed check stops the command.

## Sub-features

- Dev reads annual ZIPs through 2024 and ignores rows after 2024-10-31 before parsing. Holdout reads all 29 ZIPs and ignores rows after 2026-10-30. SPY rows use the same respective cutoffs.
- Holdout requires `--dev-match` and `--ledger`. The dev comparison must match, its embedded dev summaries must agree, all dev result files must match `SHA256SUMS`, and the envelope and ledger must bind that manifest hash. The most recent H006 dev run must bind all 29 ZIP hashes and `SPY.json`; the later dev verdict must pass A, B, C and D; a later `holdout_open` must exist; and no H006 holdout run may already exist. Missing, inconsistent, or tampered evidence stops before publication.
- Outputs are published as a new directory atomically. An existing output directory is refused.

## How to get to it (user POV)

```sh
target/release/h006-compute --histdata-dir <zip-directory> --spy <SPY.json> --stage dev --out results/<new-directory>
target/release/h006-compute --histdata-dir <zip-directory> --spy <SPY.json> --stage holdout --out results/<new-directory> --dev-match <dev-directory>/verdict.json --ledger <ledger.jsonl>
```

## Driving it with the shell

Run `.agents/skills/verify-mikiri/drive-h006.sh`. It generates synthetic inputs, drives dev and an authorized synthetic holdout, checks the clock output and manifests, tests holdout refusals and tampering, and changes a 2025 bar to check byte-identical dev output. It also changes a future row inside the opened 2024 ZIP: calculation files stay byte-identical, while the recorded input hash and manifest change because the ZIP hash covers the whole file. The driver runs the Python recomputation when `stats/h006.py` exists; otherwise it prints `SKIP python recompute: stats/h006.py absent`. Evidence, including the input files and transcript, stays in `/tmp/mikiri-verify-h006-<id>/`. Only the driver's own `results/verify-h006-<id>/` directory is removed. The passing gates in its synthetic ledger are test fixtures, not a real verdict or authorization.

To generate inputs separately, run `python3 .agents/skills/verify-mikiri/h006_synth.py --out <new-directory> [--seed N]`.

## Gotchas

The ZIP names are `HISTDATA_COM_ASCII_EURUSD_M1_YYYY.zip` for 2007–2025 and `HISTDATA_COM_ASCII_EURUSD_M1_2026MM.zip` for January–October 2026. The expected member replaces `HISTDATA_COM_` with `DAT_` and `.zip` with `.csv`. The CSV has `YYYYMMDD HHMMSS;open;high;low;close;volume` and New York wall time. The ledger input-hash map uses those ZIP names and `SPY.json`; the dev summary lists only files it opened.

The output files and columns are:

- `days.parquet`: `date`, `us_dst`, `eu_entry_min`, `us_entry_min`, `us_exit_min_boundary`; for each prefix `eu_` and `us_`: `entry_price`, `entry_fallback`, `boundary_exit_price`, `sigma`, `stop_initial`, `flag_min`, `stopped`, `exit_min`, `exit_price`, `r_nostop`, `r`, `a`, `c`; then `u`, `sigma_hat`, `leverage`, `R`. Preparation-period values with insufficient history are null.
- `accounts.parquet`: `segment`, `date`, `equity_start`, `R`, `tax_paid`, `equity_end`, `equity_pretax`.
- `random.parquet`: `round`, `day_index`, `leg` (`eu` or `us`), `K`, `start_min`, `exit_min`, `stopped`, `a`, `c`, `valid`. Nullable fields identify missing windows or prices; `valid` is shared by both legs of a day.
- `removed_bars.parquet`: `timestamp`, as a New York wall-time string.
- `summary.json`: structure below.
- `SHA256SUMS`: SHA-256 and filename for the five files above in name order. Its own SHA-256 is the run result hash.

`summary.json` keys:

- `stage`; `input_sha256` mapping opened names to full SHA-256; `counts` with `bars_input`, `bars_clean`, `removed_bars`, `calculation_days`, `excluded_days`; `first_calculation_day`; `excluded_days` array of `{date, reason}` (`no_bars` or `missing_boundary`).
- `clock_check.winter` and `.summer`: `top` array of `[minute, mean]`, `sunday_median`, `sundays`.
- `segments` array: `segment`, `w`, `b`, `sr`, `cagr_pretax`, `cagr_aftertax`, `drawdown_pretax`, `drawdown_aftertax`, `max_leverage`, `max_leg_notional_to_equity`; `psr`, `psr_threshold`; `gates` with `A`, `B`, `C`, `D`, `H` (null in dev).
- `random_control`: `actual_sr`, `round_sr` array, `excluded_days` array, `percentile`, `gate_d`. A null SR or percentile means the control was not computable.
- `reports.<segment>` for `dev`, `recent` and, in holdout, `holdout`: `main`; `tax_rates` entries `{rate, common}`; `cost_target` entries `{pip, target, common}`; `breakeven` `{common, multiple, pip}`; `tails` `{common, values}` with values `p01`, `p05`, `worst`, `win_loss_ratio`, `win_rate`; `bootstrap` `{common, values}` with values `p05`, `p50`, `p95`; `ablations` entries `{name, common}` (`eu_only`, `us_only`, `s0`); `same_bar` `{common, optimistic_common, double_touches, optimistic_matches}`; `flip` `{common, original_sr, reversed_sr, warning}`; `yearly` `{common, values}` with `years` entries `{year, days, sr}`, `median_sr`, and `winter`/`summer` `{days, sr}`.
- Every `common` and `main` object contains `ruined`, `ruin_date`, `w`, `b`, `cagr_pretax`, `cagr_aftertax`, `drawdown_pretax`, `drawdown_aftertax`, `max_leverage`, `max_leg_notional_to_equity`. A ruined report row has a date and null equity metrics.
- `report_combinations` records each executed table and its parameters; `plateau_in_sample` has exit columns `A`, `B`, `C`, each containing `S0`–`S3`; `holm` explains the single-setting correction.
