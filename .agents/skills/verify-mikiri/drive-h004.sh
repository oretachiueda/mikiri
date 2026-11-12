#!/usr/bin/env bash
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"
id="$(date -u +%Y%m%dT%H%M%SZ)-$$"
run="results/verify-h004-$id"
evidence="/tmp/mikiri-verify-h004-$id"
[[ ! -e "$run" && ! -L "$run" && ! -e "$evidence" && ! -L "$evidence" ]]
mkdir "$evidence"
trap 'status=$?; rm -rf "$run"; if [[ "$status" -ne 0 ]]; then echo "FAIL evidence=$evidence" >&2; fi' EXIT
export H004_RUN="$run" H004_EVIDENCE="$evidence"
printf '$ cargo build --locked -p mikiri --bin h004-compute\n' >"$evidence/transcript.txt"
status=0
cargo build --locked -p mikiri --bin h004-compute >>"$evidence/transcript.txt" 2>&1 || status=$?
printf 'exit=%s\n' "$status" >>"$evidence/transcript.txt"
[[ "$status" -eq 0 ]]
uv run --locked --project stats python - <<'PY' >>"$evidence/transcript.txt" 2>&1
import hashlib
import json
import math
import os
import subprocess
import shutil

import pyarrow as pa
import pyarrow.parquet as pq
from datetime import date, timedelta
from pathlib import Path

root = Path(os.environ["H004_RUN"])
out = Path(os.environ["H004_EVIDENCE"])
root.mkdir()

def command(args, success=True):
    print("$ " + " ".join(map(str, args)), flush=True)
    process = subprocess.run(list(map(str, args)), text=True, capture_output=True)
    print(process.stdout, end="")
    print(process.stderr, end="")
    print(f"exit={process.returncode}", flush=True)
    if process.returncode and success:
        for path in root.rglob("verdict.json"):
            (out / f"failed-{path.parent.name}-verdict.json").write_bytes(path.read_bytes())
            verdict = json.loads(path.read_text())
            print("mismatches:", verdict.get("mismatches", [])[:20], flush=True)
    assert (process.returncode == 0) == success
    return process

calendar = []
day = date(2006, 1, 2)
while day <= date(2026, 11, 10):
    if day.weekday() < 5 and day != date(2008, 1, 1):
        calendar.append(day)
    day += timedelta(days=1)
for ticker in ("spy", "sso"):
    bars = []
    price = 100.0
    for i, day in enumerate(calendar):
        split = 2.0 if ticker == "sso" and str(day) in {"2015-05-20", "2020-08-18", "2022-01-13"} else 1.0
        dividend = 0.01 if i % 63 == 0 or split != 1.0 else 0.0
        daily = (0.001 if i % 60 < 50 else -0.003) if ticker == "spy" else 0.02 + 0.002 * math.sin(i)
        if i:
            price = (price * (1 + daily) - dividend) / split
        bars.append({"date": f"{day}T00:00:00.000Z", "close": price,
                     "high": price * 1.01, "low": price * 0.99, "open": price,
                     "adjClose": price, "adjHigh": price * 1.01,
                     "adjLow": price * 0.99, "adjOpen": price,
                     "volume": 1000000, "adjVolume": 1000000,
                     "divCash": dividend, "splitFactor": split})
    (out / f"{ticker}.json").write_text(json.dumps(bars))
print(f"synthetic Tiingo bars={len(calendar)} per ticker; SSO 2:1 splits including split+dividend")

binary = ["target/debug/h004-compute", "--spy", out / "spy.json", "--sso", out / "sso.json"]
for stage in ("dev", "holdout"):
    destination = root / stage
    extra = ["--dev-verdict", root / "dev/verdict.json"] if stage == "holdout" else []
    command([*binary, "--stage", stage, "--out", destination, *extra])
    command(["uv", "run", "--locked", "--project", "stats", "python", "stats/h004.py",
             "--stage", stage, "--run", destination, *extra])
    verdict = json.loads((destination / "verdict.json").read_text())
    assert verdict["match"] is True and verdict["mismatches"] == []
    summary = json.loads((destination / "summary.json").read_text())
    assert summary["stage"] == stage and summary["n_cumulative"] == 4
    for name in ("summary.json", "verdict.json"):
        (out / f"{stage}-{name}").write_bytes((destination / name).read_bytes())
    print(f"stage={stage} match=true")

for field in ("rsi", "signal"):
    tampered = root / f"tampered-{field}"
    shutil.copytree(root / "dev", tampered)
    table = pq.read_table(tampered / "indicators.parquet")
    rows = table.to_pylist()
    if field == "signal":
        rows[300][field] = not rows[300][field]
    else:
        rows[300][field] += 1.0
    pq.write_table(pa.Table.from_pylist(rows, schema=table.schema), tampered / "indicators.parquet")
    command(["uv", "run", "--locked", "--project", "stats", "python", "stats/h004.py",
             "--stage", "dev", "--run", tampered], success=False)
    assert json.loads((tampered / "verdict.json").read_text())["match"] is False
print("tampered RSI/signal: independent recompute refuses both")

original = json.loads((out / "spy.json").read_text())
for row in original:
    if row["date"][:10] > "2024-10-31":
        row["close"] = -1.0
(out / "future.json").write_text(json.dumps(original))
future = root / "future"
command(["target/debug/h004-compute", "--spy", out / "future.json", "--sso", out / "sso.json",
         "--stage", "dev", "--out", future])
for path in (root / "dev").iterdir():
    if path.name != "verdict.json":
        assert path.read_bytes() == (future / path.name).read_bytes(), path.name
print("future-price invariance: every Rust dev output byte-identical, even invalid future prices")

refused = root / "refused"
command([*binary, "--stage", "holdout", "--out", refused], success=False)
assert not refused.exists()
(out / "failed-dev.json").write_text(json.dumps({"stage": "dev", "gate_a": False, "gate_b": True, "gate_c": True}))
command([*binary, "--stage", "holdout", "--out", refused,
         "--dev-verdict", out / "failed-dev.json"], success=False)
assert not refused.exists()

for field, value in (("close", 0), ("splitFactor", 0), ("divCash", -1)):
    invalid = json.loads((out / "sso.json").read_text())
    next(row for row in invalid if row["date"][:10] == "2015-05-20")[field] = value
    (out / "invalid.json").write_text(json.dumps(invalid))
    command(["target/debug/h004-compute", "--spy", out / "spy.json", "--sso", out / "invalid.json",
             "--stage", "dev", "--out", refused], success=False)
    assert not refused.exists()
invalid = json.loads((out / "sso.json").read_text())
invalid = [row for row in invalid if row["date"][:10] != "2015-05-20"]
(out / "invalid.json").write_text(json.dumps(invalid))
command(["target/debug/h004-compute", "--spy", out / "spy.json", "--sso", out / "invalid.json",
         "--stage", "dev", "--out", refused], success=False)
assert not refused.exists()
print("refusals: missing/failed dev verdict, invalid prices/splits/dividends, missing SSO date; no outputs")

with (out / "SHA256SUMS").open("w") as hashes:
    for directory in (root / "dev", root / "holdout"):
        for path in sorted(directory.iterdir()):
            hashes.write(f"{hashlib.sha256(path.read_bytes()).hexdigest()}  {directory.name}/{path.name}\n")
print("PASS both stages; indicators/days/summary match")
PY
cat "$evidence/transcript.txt"
echo "PASS evidence=$evidence"
