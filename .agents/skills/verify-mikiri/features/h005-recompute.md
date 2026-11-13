# Recompute H005 in Python

`stats/h005.py` independently recomputes H005 indicators, portfolio accounting, taxes, report tables, and verdict gates from Rust's persisted original-unit observations, then compares them with the Rust outputs.

## Sub-features

- Dates and signals must match exactly. Other numeric values use `1e-10 * max(1, abs(a), abs(b))`; PSR uses absolute tolerance `1e-9`.
- Dev compares all calculation outputs and gates A/B/C. Holdout additionally verifies the dev match envelope, manifest and results hash, raw input hashes, ledger gates, `holdout_open`, and one-time-use rule.
- A mismatch writes a `match: false` envelope and exits unsuccessfully. It never turns a missing UK holdout rate into a pass.

## How to get to it (user POV)

Run `uv run --locked --project stats python stats/h005.py --stage dev --run results/<dev-run>`. For an authorized holdout, add `--dev-match <dev-run>/verdict.json --ledger ledger/ledger.jsonl --fx-dir <fred-csv-dir> --rates <oecd.csv> --spy <spy.json>`.

## Driving it with the shell

Run `.agents/skills/verify-mikiri/drive-h005.sh`. It requires `match: true` for both stages and verifies synthetic refusals and future-input invariance. It retains the transcript and hashes under the printed evidence directory. An unexpected command failure also copies available results and up to 20 mismatches before cleanup.

## Gotchas

Python consumes the persisted original-unit observations and independently calculates the result. Matching synthetic data is implementation evidence only; it is not an independent research review or a real-data judgment.
