#!/usr/bin/env bash
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

id="$(date -u +%Y%m%dT%H%M%SZ)-$$"
run="results/verify-h002-$id"
evidence="/tmp/mikiri-verify-h002-$id"
log="$evidence/transcript.txt"
if [[ -e "$run" || -L "$run" || -e "$evidence" || -L "$evidence" ]]; then
  echo "FAIL evidence=$evidence" >&2
  exit 1
fi
mkdir "$evidence"
trap 'status=$?; rm -rf "$run"; if [[ "$status" -ne 0 ]]; then echo "FAIL evidence=$evidence" >&2; fi' EXIT

step() {
  printf '$ %s\n' "$*" >>"$log"
  local status=0
  "$@" >>"$log" 2>&1 || status=$?
  printf 'exit=%s\n' "$status" >>"$log"
  return "$status"
}

step cargo build --locked -p mikiri --bin h002-compute

step uv run --locked --project stats python - "$evidence/ports.parquet" <<'PY'
import sys
from datetime import date

import pyarrow as pa
import pyarrow.parquet as pq

rows = []
for i in range(652):
    year, month = divmod(1971 * 12 + 8 + i, 12)
    day = date(year, month + 1, 28)
    top = 6.125 if i % 2 else 5.875
    for port, ret in zip(("01", "02", "03", "04", "05", "LS"), (1, 2, 3, 4, top, top - 1)):
        rows.append({"signalname": "AnnouncementReturn", "port": port, "date": day, "ret": ret})
schema = pa.schema([
    ("signalname", pa.large_string()), ("port", pa.large_string()),
    ("date", pa.date32()), ("ret", pa.float64()),
])
pq.write_table(pa.Table.from_pylist(rows, schema=schema), sys.argv[1], compression="zstd")
PY

sha256="$(shasum -a 256 "$evidence/ports.parquet" | cut -d ' ' -f 1)"
step target/debug/h002-compute --ports "$evidence/ports.parquet" --sha256 "$sha256" --out "$run"
step uv run --locked --project stats python stats/h002.py --run "$run"
cp "$run/summary.json" "$run/verdict.json" "$evidence/"
(cd "$run" && shasum -a 256 months.parquet summary.json verdict.json) >"$evidence/SHA256SUMS"

step uv run --locked --project stats python - "$evidence" <<'PY'
import json
import sys
from pathlib import Path

evidence = Path(sys.argv[1])
summary = json.loads((evidence / "summary.json").read_text())
verdict = json.loads((evidence / "verdict.json").read_text())
assert (summary["main_n"], summary["recent_n"]) == (370, 120)
assert verdict["match"] is True and verdict["mismatches"] == []
PY

echo "PASS evidence=$evidence"
