# Recompute H006 in Python

`stats/h006.py` independently recomputes H006 from HistData zips and SPY.json: byte-identical repeat dropping (correction 1), cleaning, clock checks, computation days, excluded days, leg walks, random control, report tables, and gates, then compares them with the Rust outputs.

## Sub-features

- Dates, times, structure, booleans, ints, and the random-control start minutes must match exactly. Other numeric values use `1e-10 * max(1, abs(a), abs(b))`; PSR and percentile use absolute `1e-9`; null must match null.
- Every Rust output file is compared: `days.parquet`, `accounts.parquet`, `random.parquet` (all rows), `removed_bars.parquet`, every `summary.json` key (input hashes, counts, `duplicate_lines_removed`, clock check, segments, random control, reports), plus `SHA256SUMS` verification.
- Correction 1 is implemented independently in Python's own zip reader: after the stage date cut, a line whose whole bytes (newline and trailing `\r` excluded) repeat the first line of an already-seen timestamp is dropped; a repeated timestamp with different bytes, or a backward timestamp, is an explicit error. The dropped count and timestamps are compared with the Rust summary key exactly (integers and strings).
- Dev compares all calculation outputs and gates A/B/C/D. Holdout additionally verifies the dev match envelope, manifest and results hash, input hashes against the dev ledger run, `holdout_open`, and the one-time-use rule; a used or missing holdout is refused.
- A mismatch writes a `match: false` envelope in the run directory and exits non-zero.

## How to get to it (user POV)

Run `uv run --locked --project stats python stats/h006.py --stage dev --run results/<dev-run> --histdata-dir <histdata-zips> --spy <SPY.json>`. For an authorized holdout, add `--dev-match <dev-run>/verdict.json --ledger ledger/ledger.jsonl`.

## Driving it with the shell

Run `.agents/skills/verify-mikiri/drive-h006.sh` from the repository root. It builds `h006-compute`, regenerates synthetic inputs with `h006_synth.py`, runs the Rust dev stage, runs the Python recompute against the Rust outputs, checks the six holdout refusal paths, runs the authorized synthetic holdout with a synthetic ledger, and proves future-value invariance and tamper detection. Evidence lands in `/tmp/mikiri-verify-h006-<id>`.

## Gotchas

- The holdout dev-run input-hash check hashes every zip present in the histdata directory plus SPY.json (matching Rust), not just the stage's cutoff zips.
- The dev-envelope summary check inside the holdout compares with the same numeric tolerance as the match verdict, not exact JSON equality; the two summaries differ in the last float digits.
- The whole dev recompute takes about 37 s and about 1.8 GB peak RSS on the synthetic full history.
