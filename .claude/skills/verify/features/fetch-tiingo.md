# Daily-bar collection

## Sub-features

Parse validated dates and numeric fields; collect through curl without token
arguments; publish original bytes atomically; report count, dates and hash.

## How to get to it (user POV)

`cargo run --locked -p mikiri --bin fetch-tiingo -- --ticker SPY --start 2020-01-02 --token-file FAKE_TOKEN_PATH --out TEMP_OUTPUT_PATH`

## Driving it with cargo and a fake curl

Follow the parent skill. Run all eight `tiingo` tests. Then drive the built CLI
with fake curl, comparing saved bytes and SHA-256 to the synthetic input.

## Gotchas

Use explicit fake credentials and isolated temporary paths. The default token
file and production endpoint are outside offline verification. A real provider
response remains unverified until authorized collection after merge.
