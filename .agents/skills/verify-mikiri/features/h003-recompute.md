# Recompute H003 in Python

`stats/h003.py --run results/<run>` independently recomputes H003 from `bars.parquet`, compares the result with `days.parquet` and `summary.json`, and writes `verdict.json`. It fails when any daily value, summary value, PSR, or gate differs beyond the preregistered tolerance.

## Sub-features

- `recompute-match` writes `verdict.json` with `"match": true` when all comparisons pass.
- `recompute-mismatch` exits unsuccessfully when Rust and Python values disagree.
- `recompute-guard` refuses a run directory outside the repository `results/`.

## How to get to it (user POV)

- Run `uv run --locked --project stats python stats/h003.py --run results/<run>` after `h003-compute` writes the run.

## Driving it with the shell

The baseline in [README.md](README.md) holds. Run `.agents/skills/verify-mikiri/drive-h003.sh`. In its `transcript.txt`, the Rust command must exit `0` and report hashes and counts; the Python command must exit `0` and end with `match=true`. The copied `verdict.json` must have an empty `mismatches` list.

## Gotchas

- Python reads the persisted bars and independently calculates the result; it does not consume Rust's daily calculations as inputs.
- A matching synthetic run demonstrates implementation agreement on synthetic data. It is not evidence that the strategy passes its gates on market data.
