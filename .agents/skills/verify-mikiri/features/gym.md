# Synthetic trap gym

The gym runs synthetic scenarios without market data and reports their results as JSON.

## Sub-features

Reference scenarios G1–G5 and the stop-fill mutant.

## How to get to it (user POV)

Run from the repository root with the locked stats environment.

## Driving it with the shell

```bash
(cd stats && uv run --locked python gym.py)
```

Expected: one JSON object with `seed`, `files`, and `scenarios`; G1–G5 each have `passed: true`; exit code 0.

```bash
(cd stats && uv run --locked python gym.py --mutant stop_fill_at_level)
```

Expected: one JSON object with G4 `passed: false`; exit code 1.

## Gotchas

The seed and numerical values change when a seeded file changes. Temporary inputs are removed after each run.
