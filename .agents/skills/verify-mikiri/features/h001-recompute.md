# Recompute H001 in Python

`stats/h001.py` reads the `bars.parquet` that Rust wrote, recomputes the cycles and summary on its own, compares them with the Rust files, and writes `verdict.json` into the same run directory. Exit code `0` means that every value matched.

## Sub-features

- `recompute-match` writes `verdict.json` with `"match": true` and prints its path and hash.
- `recompute-mismatch` prints `match=false` and exits `1` when a Rust value differs.
- `recompute-guard` refuses a run directory that is not under the repository `results/`.

## How to get to it (user POV)

- Run `uv run --locked --project stats python stats/h001.py --run results/<run>` from the repository root after `h001-compute` wrote that run.

## Driving it with the shell

Preconditions:

- The baseline in `README.md` holds.

- **Match.** Run `.agents/skills/verify-mikiri/drive.sh`. In `transcript.txt` the `stats/h001.py` step prints a line that ends with `match=true`, followed by `exit=0`. The copied `verdict.json` has `"match": true` and an empty `mismatches` list.
- **Guard.** Run `uv run --locked --project stats python stats/h001.py --run /tmp`. Stderr is `H001 input or computation failed` and the exit code is `1`.
- **Mismatch.** Run `(cd stats && uv run --locked pytest tests/test_h001.py -k cli_match_and_mismatch)`. The test runs the same script on a matching run directory and on one with a changed summary value. It passes. It makes a `results/h001-test-*` directory and deletes it when it ends.

## Gotchas

- The script accepts only a directory under this checkout's `results/`. A copy of a run in another place fails the guard.
- `match=true` proves that Rust and Python agree. It does not prove that both follow the preregistration. An independent runner checks that.
- The run directory must hold `bars.parquet`, `cycles.parquet`, and `summary.json` from one `h001-compute` run.
