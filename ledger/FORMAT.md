# Ledger format

`ledger/ledger.jsonl` is the public record of every preregistration, run, verdict, and holdout use.

## Rules

- One JSON object per line, UTF-8, LF line endings.
- Append only. Never edit or delete a line. A correction is a new `correction` line.
- `seq` starts at 1 and increases by 1 per line.
- `ts` is UTC in RFC 3339 form, for example `2026-11-08T13:00:00Z`.
- Every hash is SHA-256 as 64 lowercase hex characters.
- `n_cumulative` counts every configuration ever run in this repository. It never decreases.
- `LEDGER.md` is generated from this file. Do not edit it by hand.

## Common fields

| Field | Type | Meaning |
|---|---|---|
| `seq` | integer | Line number in the ledger |
| `ts` | string | When the line was written |
| `kind` | string | One of the kinds below |
| `hypothesis` | string | Hypothesis id, for example `H001` |

## Kinds

| `kind` | Extra fields |
|---|---|
| `prereg` | `doc` (path), `doc_sha256`, `configs` (integer), `dev` and `holdout` (`{"start": date, "end": date}`) |
| `run` | `stage` (`dev` or `holdout`), `configs` (integer counted toward N), `n_cumulative`, `code_commit` (40 hex), `data_sha256` (object, file name to hash), `results_sha256` |
| `verdict` | `stage`, `gates` (object, gate letter to boolean), `result` (`pass` or `reject`), `values` (object of reported numbers), `results_sha256`, `recomputed_by` (model family of the independent runner) |
| `holdout_open` | `stage` is always `holdout`; `reason` |
| `correction` | `corrects_seq` (integer), `reason`, `fields` (object of corrected values) |

Dates inside `dev` and `holdout` are `YYYY-MM-DD`. `holdout` is `null` when the preregistration puts the recent period into the main test and says why. A `prereg` line is written in the same commit as its document, so `doc_sha256` hashes the merged file bytes.
