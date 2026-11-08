# Mikiri

Research and fully automated execution of US equity strategies that trade on daily signals and hold for days to weeks.

Status: scaffolding. No strategy has passed its preregistered test, and nothing trades live.

## Layout

- `crates/`: Rust. Data, backtests, and orders on NautilusTrader.
- `stats/`: Python batch that turns Rust results into verdicts. It never runs in the live process.

## Checks

```bash
pre-commit install
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo nextest run --workspace --locked --no-tests=pass
cargo deny --locked check
(cd stats && uv run --locked ruff check && uv run --locked ty check)
```

Rules for coding agents live in `AGENTS.md`.
