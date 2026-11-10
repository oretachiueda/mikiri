#!/usr/bin/env bash
# Runs h001-compute and stats/h001.py on synthetic bars and keeps the proof.
# Usage: .agents/skills/verify-mikiri/drive.sh
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

id="$(date -u +%Y%m%dT%H%M%SZ)-$$"
run="results/verify-$id"
evidence="/tmp/mikiri-verify-$id"
log="$evidence/transcript.txt"
[ ! -e "$run" ] || { echo "FAIL $run already exists" >&2; exit 1; }
mkdir -p "$evidence"
trap 'status=$?; rm -rf "$run"; [ "$status" -eq 0 ] || echo "FAIL evidence=$evidence" >&2' EXIT

step() {
  printf '$ %s\n' "$*" >>"$log"
  local status=0
  "$@" >>"$log" 2>&1 || status=$?
  printf 'exit=%s\n' "$status" >>"$log"
  return "$status"
}

step cargo build --locked -p mikiri --bins

# Same dates and values as synthetic_json in crates/mikiri/tests/h001_cli.rs, without the unused adjClose.
uv run --locked --project stats python - "$evidence/bars.json" <<'EOF'
import json
import sys
from datetime import date, timedelta

day, bars = date(2005, 11, 1), []
while day <= date(2026, 10, 30):
    if day.weekday() < 5:
        dividend = day.timetuple().tm_yday / 100
        bars.append({"date": f"{day}T00:00:00Z", "close": 100.0, "divCash": dividend, "splitFactor": 1.0})
    day += timedelta(days=1)
with open(sys.argv[1], "w") as file:
    json.dump(bars, file)
EOF

step target/debug/h001-compute --bars "$evidence/bars.json" --out "$run"
step uv run --locked --project stats python stats/h001.py --run "$run"
cp "$run/summary.json" "$run/verdict.json" "$evidence/"
(cd "$run" && shasum -a 256 bars.parquet cycles.parquet summary.json verdict.json) >"$evidence/SHA256SUMS"

uv run --locked --project stats python - "$evidence" <<'EOF'
import json
import sys

with open(f"{sys.argv[1]}/summary.json") as file:
    summary = json.load(file)
with open(f"{sys.argv[1]}/verdict.json") as file:
    verdict = json.load(file)
counts = (summary["main_n"], summary["recent_n"], summary["reference_n"])
assert counts == (250, 120, 1), counts
assert verdict["match"] is True and verdict["mismatches"] == [], verdict["mismatches"]
EOF

echo "PASS evidence=$evidence"
