# Mikiri verification map

This directory is the maintained source for verifying what a user of Mikiri can do. Read this index, then use the matching feature file as the recipe.

## Features

- [`h001-compute.md`](h001-compute.md): compute the H001 cycles and summary from daily bars.
- [`h001-recompute.md`](h001-recompute.md): recompute H001 in Python and compare it with Rust.
- [`h002-screen.md`](h002-screen.md): compute the H002 monthly screen in Rust and compare it with Python.
- [`h003-compute.md`](h003-compute.md): compute the H003 leveraged-trend strategy and summary from Tiingo-format daily bars.
- [`h003-recompute.md`](h003-recompute.md): recompute H003 in Python and compare every reported and daily value with Rust.
- [`fetch-tiingo.md`](fetch-tiingo.md): download daily bars from Tiingo.

## Baseline preconditions

- Run every command from the repository root.
- `cargo build --locked -p mikiri --bins` has finished.
- The Doctor line in `../SKILL.md` prints `doctor-ok`.

## Driving conventions

- Treat every command as literal.
- Use synthetic inputs only. Never read `data/`, `live/`, `secrets/`, `.env`, or a token file.
- A failure recipe states the exact stderr line and exit code. A different message is a finding.

## Proof and skip reporting

- CLI proof is the command, stdout, stderr, and exit code.
- A success proof also names the files written and their hashes.
- A failure proof also shows that no output file appeared.
- Report a path that you could not drive with the command you tried and the reason. Do not report it as verified through another path.

## Feature entry contract

Each feature file has an H1 title, one paragraph on the user-visible behavior, and four H2 sections in this order: `Sub-features`, `How to get to it (user POV)`, `Driving it with the shell`, `Gotchas`.
