#!/usr/bin/env bash
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

id="$(date -u +%Y%m%dT%H%M%SZ)-$$"
run="results/verify-h003-$id"
evidence="/tmp/mikiri-verify-h003-$id"
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

step cargo build --locked -p mikiri --bin h003-compute

printf '$ python3 <generate synthetic Tiingo arrays>\n' >>"$log"
python3 - "$evidence" >>"$log" 2>&1 <<'PY'
import json
import math
import sys
from datetime import date, timedelta
from pathlib import Path

out = Path(sys.argv[1])
start, end = date(2013, 1, 1), date(2026, 11, 10)
dates = []
day = start
while day <= end:
    if day.weekday() < 5:
        dates.append(day)
    day += timedelta(days=1)

factors = {"SPY": {d: 1.0 for d in dates}, "SPXL": {d: 1.0 for d in dates}, "BIL": {d: 1.0 for d in dates}}
factors["SPXL"][date(2013, 4, 2)] = 2.0
factors["SPXL"][date(2015, 12, 1)] = 2.0
factors["SPXL"][date(2017, 5, 1)] = 4.0
factors["BIL"][date(2017, 11, 30)] = 0.5
dividends = {ticker: {d: 0.0 for d in dates} for ticker in factors}
dividends["SPXL"][date(2015, 12, 1)] = 0.08
dividends["SPY"][date(2016, 1, 4)] = 0.12
dividends["BIL"][date(2022, 1, 3)] = 0.05

prices = {}
for ticker, base in (("SPY", 100.0), ("SPXL", 40.0), ("BIL", 90.0)):
    values = []
    economic_close = base
    cumulative_split = 1.0
    for i, current in enumerate(dates):
        cumulative_split *= factors[ticker][current]
        if ticker == "SPY":
            daily_return = (
                0.0008 if current < date(2016, 11, 7)
                else -0.55 if current == date(2016, 11, 7)
                else -0.0002 if current < date(2020, 1, 2)
                else 0.002 if current < date(2020, 9, 1)
                else -0.60 if current == date(2020, 9, 1)
                else -0.0001 if current < date(2021, 1, 4)
                else 0.003 if current < date(2021, 6, 15)
                else -0.50 if current == date(2021, 6, 15)
                else 0.00015 + 0.006 * math.sin(i / 31.0)
            )
        elif ticker == "SPXL":
            daily_return = (
                0.0015 if current < date(2016, 11, 7)
                else -0.98 if current == date(2016, 11, 7)
                else 0.0 if current < date(2020, 1, 2)
                else 0.004 if current < date(2020, 9, 1)
                else -0.90 if current == date(2020, 9, 1)
                else 0.0 if current < date(2021, 1, 4)
                else 0.005 if current < date(2021, 6, 15)
                else -0.40 if current == date(2021, 6, 15)
                else 0.0004 + 0.025 * math.sin(i / 13.0)
            )
        else:
            daily_return = 0.0 if date(2017, 1, 1) <= current < date(2020, 1, 1) else 0.00002
        if i:
            economic_close *= 1.0 + daily_return
        values.append(economic_close / cumulative_split)
    prices[ticker] = values

# Put a dividend on a deterministic day whose prior close signal flips, so the
# driver exercises dividend processing on a rebalance date.
index, signal = [], []
for i, current in enumerate(dates):
    if i:
        ratio = (prices["SPY"][i] * factors["SPY"][current] + dividends["SPY"][current]) / prices["SPY"][i - 1]
        index.append(index[-1] * ratio)
    else:
        index.append(1.0)
    signal.append(i >= 199 and index[i] > sum(index[i - 199 : i + 1]) / 200)
switch_day = next(
    (dates[i] for i in range(201, len(dates))
     if date(2015, 11, 2) <= dates[i] <= date(2026, 10, 30)
     and signal[i - 1] != signal[i - 2]),
    None,
)
if switch_day is None:
    raise SystemExit("synthetic series has no main-window signal switch")
signal_on = {day: signal[i] for i, day in enumerate(dates)}
assert signal_on[date(2015, 10, 30)]
assert not signal_on[date(2016, 11, 7)]
assert all(not signal_on[day] for day in dates if date(2017, 1, 1) <= day < date(2020, 1, 1))
assert any(signal_on[day] for day in dates if date(2020, 1, 2) <= day < date(2020, 9, 1))
assert not signal_on[date(2020, 9, 1)]
assert any(signal_on[day] for day in dates if date(2021, 1, 4) <= day < date(2021, 6, 15))
assert not signal_on[date(2021, 6, 15)]
dividends["SPXL"][switch_day] = 0.07
dividends["BIL"][switch_day] = 0.04

for ticker in factors:
    bars = []
    for i, current in enumerate(dates):
        close = prices[ticker][i]
        bars.append({
            "date": f"{current}T00:00:00.000Z",
            "close": close,
            "high": close * 1.01,
            "low": close * 0.99,
            "open": close,
            "adjClose": close,
            "adjHigh": close * 1.01,
            "adjLow": close * 0.99,
            "adjOpen": close,
            "divCash": dividends[ticker][current],
            "splitFactor": factors[ticker][current],
            "volume": 1_000_000,
            "adjVolume": 1_000_000,
        })
    (out / f"{ticker.lower()}.json").write_text(json.dumps(bars, separators=(",", ":")))

assert all(len(json.loads((out / f"{ticker.lower()}.json").read_text())[0]) == 13 for ticker in factors)
print(f"synthetic bars={len(dates)} per ticker; SPY trend/crash regimes exercise switching; SPXL split+dividend=2015-12-01, forward split=4 on 2017-05-01; BIL reverse split=0.5; switch-day dividend={switch_day}")
PY
printf 'exit=%s\n' "$?" >>"$log"

step target/debug/h003-compute --spy "$evidence/spy.json" --spxl "$evidence/spxl.json" --bil "$evidence/bil.json" --out "$run"
step uv run --locked --project stats python stats/h003.py --run "$run"
cp "$run/summary.json" "$run/verdict.json" "$evidence/"
(cd "$run" && shasum -a 256 bars.parquet days.parquet summary.json verdict.json) >"$evidence/SHA256SUMS"
(cd "$evidence" && shasum -a 256 spy.json spxl.json bil.json) >>"$evidence/SHA256SUMS"

step uv run --locked --project stats python - "$evidence" "$run" <<'PY'
import json
import sys
from pathlib import Path

evidence = Path(sys.argv[1])
run = Path(sys.argv[2])
sys.path.insert(0, str(Path.cwd() / "stats"))
import h003

summary = json.loads((evidence / "summary.json").read_text())
verdict = json.loads((evidence / "verdict.json").read_text())
assert summary["main_n"] > 2_000, summary["main_n"]
assert summary["recent_n"] > 900, summary["recent_n"]
assert summary["n_cumulative"] == 3, summary["n_cumulative"]
assert isinstance(summary["psr_t"], int) and summary["psr_t"] == summary["main_n"] - 1
assert all(isinstance(summary[key], bool) for key in ("gate_a", "gate_b", "gate_c"))
assert isinstance(summary["main"], dict) and isinstance(summary["recent"], dict)
assert verdict["match"] is True and verdict["mismatches"] == [], verdict["mismatches"]
print(f"counts main_n={summary['main_n']} recent_n={summary['recent_n']} n_cumulative={summary['n_cumulative']} psr_t={summary['psr_t']}; match=true")

bars = h003.read_bars(run / "bars.parquet")
events = []
base_ledger = h003.TaxLedger

class RecordingLedger(base_ledger):
    def settle(self, year):
        income = self.income.get(year, 0.0)
        before = {int(vintage): float(loss) for vintage, loss in self.losses}
        tax = super().settle(year)
        after = {int(vintage): float(loss) for vintage, loss in self.losses}
        events.append({"year": year, "income": income, "before": before, "after": after})
        return tax

h003.TaxLedger = RecordingLedger
try:
    h003.simulate(
        bars["SPY"], bars["SPXL"], bars["BIL"],
        h003.MAIN_START, h003.END,
    )
finally:
    h003.TaxLedger = base_ledger

by_year = {event["year"]: event for event in events}
assert by_year[2016]["income"] < 0 and by_year[2016]["after"][2016] > 0
for year in (2017, 2018, 2019):
    assert by_year[year]["income"] == 0
    assert by_year[year]["before"][2016] == by_year[year]["after"][2016]
assert 2016 in by_year[2020]["before"] and 2016 not in by_year[2020]["after"]
assert by_year[2020]["income"] < 0 and by_year[2020]["after"][2020] > 0
assert by_year[2022]["income"] > 0
assert by_year[2022]["before"][2020] > by_year[2022]["after"][2020] >= 0
print("tax path: 2016 loss survives 2017-2019 and expires in 2020; 2020 loss is used against 2022 gain")
PY

echo "evidence=$evidence"
echo PASS
