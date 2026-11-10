# Fetch daily bars from Tiingo

`fetch-tiingo` downloads end-of-day bars for one ticker, validates them, and saves the response under `data/`. It prints a summary with the file hash and never prints prices or the token.

## Sub-features

- `fetch-args` rejects missing arguments and a malformed start date.
- `fetch-out-guard` rejects an output path that is not under `data/`.
- `fetch-token` fails when the token file is unreadable and writes nothing.
- `fetch-download` saves validated bars. It needs a token and network, and this skill does not drive it.

## How to get to it (user POV)

- Run `target/debug/fetch-tiingo --ticker <ticker> --start <YYYY-MM-DD> --out data/raw/tiingo/<ticker>.json` from the repository root. `--token-file <path>` replaces the default token file.

## Driving it with the shell

Preconditions:

- The baseline in `README.md` holds.
- `/tmp/mikiri-no-token` does not exist.

- **Arguments.** Run `target/debug/fetch-tiingo`, then `target/debug/fetch-tiingo --ticker SPY --start 2020-01-01`, then `target/debug/fetch-tiingo --ticker SPY --start 20200101 --out data/raw/tiingo/VERIFY.json`. Stderr is `missing --ticker`, `missing --out`, and `start must be YYYY-MM-DD`. Each exit code is `1`.
- **Output guard.** Run `target/debug/fetch-tiingo --ticker SPY --start 2020-01-01 --out /tmp/x.json --token-file /tmp/mikiri-no-token`. Stderr is `output path must be under data/` and the exit code is `1`.
- **Token.** Run `target/debug/fetch-tiingo --ticker SPY --start 2020-01-01 --out data/raw/tiingo/VERIFY.json --token-file /tmp/mikiri-no-token`. Stderr is `cannot read token file`, the exit code is `1`, and `data/raw/tiingo/VERIFY.json` does not exist.
- **Download.** This skill does not verify it. `cargo nextest run --locked --test tiingo` tests the `fetch_daily_bars` library function for parsing, validation, the saved bytes, and that a connection error does not expose the token. Those tests are not proof of the binary. Report a download as unverified unless a human asked for a real run.

## Gotchas

- Without `--token-file` the tool reads the default token file in the home directory. Always pass a path that does not exist, so that verification never touches a real token.
- A real download writes licensed data into `data/`. Run it only when a human asks, and never copy the file out of `data/`.
