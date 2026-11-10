# Screen H002

`h002-compute` verifies the input hash, computes monthly returns and a summary, and writes both under `results/`. The Python batch recomputes the screen from the monthly Parquet and writes `verdict.json`.

## Sub-features

- The Rust command writes `months.parquet` and `summary.json` and prints both paths and hashes with the main and recent month counts.
- The Rust command rejects a hash mismatch or an output path outside the accepted `results/` syntax before writing files.
- The Python batch prints the verdict path, hash, and match status. A mismatch exits with code `1`.

## How to get to it (user POV)

- Run `target/debug/h002-compute --ports <ports.parquet> --sha256 <expected-hash> --out results/<run>` from the repository root. Use the hash in the preregistration for a research run.
- Run `uv run --locked --project stats python stats/h002.py --run results/<run>` after Rust writes the run.

## Driving it with the shell

The baseline in [README.md](README.md) holds. Run the success recipe first. Set `evidence` to the directory it prints and `wrong="$(printf '%064d' 0)"`. The paths `results/verify-h002-bad-hash` and `out` must be absent before running the failure recipes.

- **Success.** Run `.agents/skills/verify-mikiri/drive-h002.sh`. The last line starts with `PASS evidence=`. In the transcript the Rust line ends with `main_n=370 recent_n=120`, the Python line ends with `match=true`, and both commands exit with code `0`.
- **Hash mismatch.** Run `target/debug/h002-compute --ports "$evidence/ports.parquet" --sha256 "$wrong" --out results/verify-h002-bad-hash`. Stderr is `input SHA-256 mismatch`, the exit code is `1`, and the output path stays absent.
- **Output guard.** Run `target/debug/h002-compute --ports "$evidence/ports.parquet" --sha256 "$wrong" --out out`. Stderr is `output path must be under results/`, the exit code is `1`, and the output path stays absent.

## Gotchas

- The drive removes its own run directory when it exits. It keeps the synthetic input, summary, verdict, transcript, and output hashes in the evidence directory.
- The Python batch can check the form of the input hash and excluded reference count. The original input and excluded rows are outside its run directory.
- The synthetic run verifies the CLI behavior and agreement between the implementations. It supplies no market evidence.
