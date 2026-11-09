---
name: verify-mikiri
description: Verify Mikiri H001 computation through the real CLI with synthetic bars, without market data or credentials.
---

# Verify H001 computation

## Launch

Run `cargo build --locked --bin h001-compute`. The binary is `target/debug/h001-compute`. It requires `--bars` and `--out` and exits after computation.

## Doctor

Run `cargo check --workspace --locked --offline`. Do not read market data or credential files for this verification.

## Drive

Run `cargo nextest run --locked --test h001_cli`. This harness invokes the built CLI in isolated temporary directories with synthetic weekday bars. It checks a successful computation through October 30, 2026, invalid coverage, output paths, and duplicate arguments.

Run `cargo nextest run --locked --lib h001::tests` for the calendar, cost, PSR, corporate action, and Parquet checks.

## Evidence

Capture the command output and exit status outside the checkout, for example `/tmp/mikiri-h001-verification.log`. The CLI harness checks the output files, hashes, counts, and absence of judgment values on stdout. These synthetic checks do not establish a market result or replace independent Python recomputation.

## Cleanup

The CLI harness removes its own temporary directories. Keep the captured command output. Do not remove existing `data/`, `results/`, or credential directories.
