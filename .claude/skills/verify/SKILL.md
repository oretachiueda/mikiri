---
name: verify
description: Verify Mikiri's fetch-tiingo CLI and daily-bar parser offline, without real credentials or market data.
---

## Launch

From the repository root run `cargo build --locked -p mikiri --bin fetch-tiingo`.
This is a short-lived CLI; no server is needed.

## Doctor

Run `target/debug/fetch-tiingo` with no arguments. It must exit nonzero with
`missing --ticker`, before reading credentials or starting curl.

## Drive

Run `cargo nextest run --locked -p mikiri --test tiingo` for synthetic parser,
file publication, and refused localhost connection journeys. See
[features](features/README.md). Never read the default token file or call the
real Tiingo API as part of verification.

For the CLI success journey, place a fake executable named curl in a temporary
directory prepended to PATH. It must consume stdin and emit a synthetic Tiingo
JSON array. Pass an explicit fake `--token-file`, `--ticker SPY`,
`--start 2020-01-02`, and a temporary `--out` path. Check the output bytes match
exactly, the summary hash matches `shasum -a 256`, and stdout is one line.

## Evidence

Keep command output, exit codes, byte comparisons and hashes under
`/tmp/mikiri-fetch-evidence/`. Exercise the built CLI; replace only the external
curl boundary for the success journey. Never record real tokens or prices.

## Cleanup

Remove only temporary fake curl, fake token and output directories created by
the run. No processes remain after the short-lived commands. Preserve the
evidence directory.
