# Recompute H004 in Python

`stats/h004.py` independently calculates the H004 signals, indicators, portfolio accounting, taxes and summary from the persisted bars, then writes a match verdict.

## Sub-features

- Compare index, M200, M5, RSI, signal, every daily value and summary metric with Rust. Assets and RSI use relative tolerance 1e-10 and PSR 1e-9; signal disagreement fails. Daily R/Q/d comparisons use absolute difference at most `1e-10 * max(1, abs(a), abs(b))`, as in H003.
- Dev calculates only the development windows. Holdout calculates only H and requires passed dev gates through `--dev-verdict`.
- A mismatch returns failure and records the mismatches in `verdict.json`.

## How to get to it (user POV)

Run `uv run --locked --project stats python stats/h004.py --stage dev --run results/<dev-run>`. For holdout use `--stage holdout --dev-verdict <dev-verdict.json>` with the corresponding Rust holdout run.

## Driving it with the shell

Run `.agents/skills/verify-mikiri/drive-h004.sh`. Both Python commands must exit zero with `match=true`; each verdict must have `match: true` and an empty mismatch list. Retain the evidence directory printed by the driver.

## Gotchas

Python consumes the persisted bars as calculation inputs. Rust indicators, daily values and summary are comparison targets. Matching synthetic data demonstrates implementation agreement; independent model-family review and real-data judgment remain separate requirements.
