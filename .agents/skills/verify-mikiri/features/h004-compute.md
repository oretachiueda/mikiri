# Compute H004

`h004-compute` implements the preregistered RSI(2) SPY/SSO overlay in separate dev and holdout stages. It writes bars, indicators, daily accounting, and a summary under `results/`, and prints hashes and counts.

## Sub-features

- Dev drops rows after 2024-10-31 before validating prices, calculating indicators, or persisting inputs. It computes A/B/C and the doubled-cost A sensitivity.
- Holdout computes only H (2024-11-01 through 2026-10-30), using earlier SPY history for signals. It requires `--dev-verdict` with passed dev A/B/C gates. Opening real holdout remains a separate, ledger-recorded action after dev judgment.
- Input SHA-256 values cover the retained canonical Tiingo JSON rows, so future rows cannot affect dev hashes. Original download hashes for the research ledger remain a separate collection step.
- Signal indicators use the SPY total-return index, inclusive 200- and 5-session means, and seeded Wilder RSI(2). The 200th session starts in SPY. Orders execute at the following session close. Comparisons against means use the mathematically equivalent oldest-to-newest sum of index differences to avoid a rounding-induced signal on a constant index. Nonfinite computed indices, means, AU, AD or RSI halt without output.
- Accounting follows the preregistered split/dividend, rebalance, tax-sale, loss-carry, and liquidation order. Missing SSO dates, invalid values, insufficient history, and undefined PSR fail without output.
- Report-only definitions: entries count buys into SSO, including an initial SSO purchase. An SSO-held day is a return day whose incoming holding is SSO. The holding fraction divides those days by T; mean holding days divides by entries (null when entries are zero). SPY returns on those days use the total-return signal return; their mean and population standard deviation divide by n (null when n is zero).

## How to get to it (user POV)

Run `target/debug/h004-compute --stage dev --spy <spy.json> --sso <sso.json> --out results/<dev-run>`. After independent dev judgment, use `--stage holdout --dev-verdict <dev-verdict.json>` and a new output directory. Inputs are Tiingo daily JSON arrays; keep per-day inputs and outputs private.

## Driving it with the shell

Run `.agents/skills/verify-mikiri/drive-h004.sh`. It creates synthetic 13-field Tiingo arrays with SSO 2:1 splits and split-day dividends. It drives both stages, compares Python indicators and accounting, checks that changing future prices leaves all dev files byte-identical, and drives refusals with no output. The last line is `PASS evidence=<directory>`; the transcript includes commands, output and exit codes, and SHA256SUMS records full hashes.

## Gotchas

Synthetic gates are tool checks, not research results. Never append these runs to the ledger. The driver reads only its newly created `results/verify-h004-*` directory and removes it at exit; evidence under `/tmp/` remains. It never reads existing results or market data.
