# Recompute H006 in Python

`stats/h006.py` independently recomputes H006 from HistData zips and SPY.json: cleaning, clock checks, computation days, excluded days, leg walks, random control, report tables, and gates, then compares them with the Rust outputs.

## Sub-features

- Dates, times, structure, booleans, ints, and the random-control start minutes must match exactly. Other numeric values use `1e-10 * max(1, abs(a), abs(b))`; PSR and percentile use absolute `1e-9`; null must match null.
- Every Rust output file is compared: `days.parquet`, `accounts.parquet`, `random.parquet` (all rows), `removed_bars.parquet`, every `summary.json` key (input hashes, counts, clock check, segments, random control, reports), plus `SHA256SUMS` verification.
- Dev compares all calculation outputs and gates A/B/C/D. Holdout additionally verifies the dev match envelope, manifest and results hash, input hashes against the dev ledger run, `holdout_open`, and the one-time-use rule; a used or missing holdout is refused.
- A mismatch writes a `match: false` envelope in the run directory and exits non-zero.

## How to get to it (user POV)

Run `uv run --locked --project stats python stats/h006.py --stage dev --run results/<dev-run> --histdata-dir <histdata-zips> --spy <SPY.json>`. For an authorized holdout, add `--dev-match <dev-run>/verdict.json --ledger ledger/ledger.jsonl`.

## Driving it with the shell

Run `.agents/skills/verify-mikiri/drive-h006.sh` from the repository root. It builds `h006-compute`, regenerates synthetic inputs with `h006_synth.py`, runs the Rust dev stage, runs the Python recompute against the Rust outputs, checks the six holdout refusal paths, runs the authorized synthetic holdout with a synthetic ledger, and proves future-value invariance and tamper detection. Evidence lands in `/tmp/mikiri-verify-h006-<id>`.

## Gotchas

- Known difference (pending adjudication):

`summary.reports.<segment>.same_bar.double_touches`: Python reads prereg R15 literally (every bar in the leg window spanning both the stop and the favorable 1σ level) and reports 1598 dev / 552 recent on synthetic inputs; Rust reports 1457 / 423. Engineered straddle probes show Rust counts a same-bar double touch only at or before the conservative walk's own stop exit, but that reading predicts 545 / 190 on natural data, so the exact Rust rule is not pinned down. See the H006 Python report for the full case.
