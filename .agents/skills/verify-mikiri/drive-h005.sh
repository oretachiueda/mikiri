#!/usr/bin/env bash
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"
export PATH="$HOME/.cargo/bin:$HOME/.local/bin:/opt/homebrew/bin:$PATH"
id="$(date -u +%Y%m%dT%H%M%SZ)-$$"
run="results/verify-h005-$id"
evidence="/tmp/mikiri-verify-h005-$id"
[[ ! -e "$run" && ! -L "$run" && ! -e "$evidence" && ! -L "$evidence" ]]
mkdir -p "$run" "$evidence"
trap 'status=$?; rm -rf "$run"; if [[ "$status" -ne 0 ]]; then echo "FAIL evidence=$evidence" >&2; fi' EXIT
export H005_RUN="$run" H005_EVIDENCE="$evidence"
: >"$evidence/transcript.txt"
run_logged() {
    {
        printf '$'
        printf ' %q' "$@"
        printf '\n'
    } >>"$evidence/transcript.txt"
    status=0
    "$@" >>"$evidence/transcript.txt" 2>&1 || status=$?
    printf 'exit=%s\n' "$status" >>"$evidence/transcript.txt"
    return "$status"
}
run_logged cargo build --locked -p mikiri --bin h005-compute
run_logged uv run --locked --project stats python -c 'import pyarrow, pandas'
uv run --locked --project stats python - "$run" "$evidence" <<'PY' >>"$evidence/transcript.txt" 2>&1
import hashlib
import json
import math
import os
import shutil
import subprocess
import sys
from datetime import date, timedelta
from pathlib import Path

root, evidence = map(Path, sys.argv[1:])
inputs = evidence / "inputs"
fx = inputs / "fx"
fx.mkdir(parents=True)
rates = inputs / "OECD.csv"
spy = inputs / "SPY.json"
ledger = inputs / "ledger.jsonl"
ids = {
    "EUR": ("DEXUSEU", False, "EA20"), "JPY": ("DEXJPUS", True, "JPN"),
    "GBP": ("DEXUSUK", False, "GBR"), "CHF": ("DEXSZUS", True, "CHE"),
    "CAD": ("DEXCAUS", True, "CAN"), "AUD": ("DEXUSAL", False, "AUS"),
    "NZD": ("DEXUSNZ", False, "NZL"), "NOK": ("DEXNOUS", True, "NOR"),
    "SEK": ("DEXSDUS", True, "SWE"),
}
areas = {area: currency for currency, (_, _, area) in ids.items()}
areas["USA"] = "USD"

def say(*args):
    print(*args, flush=True)

failure_index = 0

def log_command(args, expect=0, error_substring=None):
    global failure_index
    say("$", " ".join(map(str, args)))
    proc = subprocess.run(list(map(str, args)), text=True, capture_output=True)
    if proc.stdout:
        print(proc.stdout, end="")
    if proc.stderr:
        print(proc.stderr, end="", file=sys.stderr)
    say(f"exit={proc.returncode}")
    if error_substring is not None:
        assert error_substring in proc.stdout + proc.stderr, f"expected error {error_substring!r}, got: {proc.stderr}"
    if proc.returncode != expect:
        failure_index += 1
        debug = evidence / "failure-debug" / f"command-{failure_index}"
        copied = 0
        for verdict_path in root.rglob("verdict.json"):
            relative = verdict_path.parent.relative_to(root)
            destination = debug / relative
            destination.mkdir(parents=True, exist_ok=True)
            for name in ("verdict.json", "summary.json", "observations.parquet", "days.parquet", "indicators.parquet", "SHA256SUMS"):
                source = verdict_path.parent / name
                if source.is_file():
                    shutil.copy2(source, destination / name)
            copied += 1
            try:
                verdict = json.loads(verdict_path.read_text())
            except (OSError, json.JSONDecodeError):
                continue
            mismatches = verdict.get("mismatches", [])
            say(f"failure artifacts copied from {relative}; mismatch_count={len(mismatches)}")
            for mismatch in mismatches[:20]:
                say(json.dumps(mismatch, sort_keys=True))
        if copied == 0:
            say("failure artifacts: no verdict.json existed to copy")
    assert proc.returncode == expect, f"expected exit {expect}, got {proc.returncode}: {proc.stderr}"
    return proc

def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()

def refresh_manifest(directory):
    names = ("days.parquet", "indicators.parquet", "observations.parquet", "summary.json")
    (directory / "SHA256SUMS").write_text("".join(f"{sha(directory / name)}  {name}\n" for name in names))

def days(start, end):
    day = start
    while day <= end:
        if day.weekday() < 5:
            yield day
        day += timedelta(days=1)

calendar = list(days(date(2007, 12, 3), date(2026, 10, 30)))
holidays = {date(y, m, d) for y in range(2007, 2027) for m, d in ((1, 1), (7, 4), (12, 25))}
spy_calendar = [day for day in calendar if day not in holidays]
for idx, (currency, (series, inverse, _)) in enumerate(ids.items()):
    price = 1.0 + idx * 0.07
    rows = [f"observation_date,{series}"]
    for n, day in enumerate(calendar):
        if day in holidays:
            rows.append(f"{day.isoformat()},")
            continue
        drift = 0.00035
        noise = 0.0011 * math.sin(n * (0.071 + idx * 0.0007) + idx) + 0.00055 * math.sin(n * 0.013 + idx * 2)
        spot = price * math.exp(drift + noise)
        price = spot
        raw = 1.0 / spot if inverse else spot
        rows.append(f"{day.isoformat()},{raw:.12f}")
    (fx / f"{series}.csv").write_text("\n".join(rows) + "\n")

# OECD monthly rates use the fixed 16-column Data Explorer schema.
header = "DATAFLOW,REF_AREA,FREQ,MEASURE,UNIT_MEASURE,ACTIVITY,ADJUSTMENT,TRANSFORMATION,TIME_HORIZ,METHODOLOGY,TIME_PERIOD,OBS_VALUE,OBS_STATUS,UNIT_MULT,DECIMALS,BASE_PER"
rate_rows = [header]
month = date(2007, 12, 1)
while month <= date(2026, 9, 1):
    for area_idx, area in enumerate(("USA", "EA20", "JPN", "GBR", "CHE", "CAN", "AUS", "NZL", "NOR", "SWE")):
        value = 2.0 + area_idx * 0.11 + 0.15 * math.sin((month.year - 2007) * 0.71 + month.month * 0.17 + area_idx)
        cols = ["OECD.SDD.STES:DSD_STES@DF_FINMARK(4.0)", area, "M", "IR3TIB", "PA", "_Z", "_Z", "_Z", "_Z", "N", f"{month.year:04d}-{month.month:02d}", f"{value:.6f}", "A", "0", "2", ""]
        rate_rows.append(",".join(cols))
    month = date(month.year + (month.month == 12), 1 if month.month == 12 else month.month + 1, 1)
rates.write_text("\n".join(rate_rows) + "\n")

spy_rows = []
close = 100.0
for n, day in enumerate(spy_calendar):
    div = 0.01 if n % 63 == 0 else 0.0
    close *= math.exp(0.000035 + 0.00008 * math.sin(n * 0.11))
    spy_rows.append({"date": f"{day.isoformat()}T00:00:00.000Z", "close": close,
        "high": close * 1.001, "low": close * 0.999, "open": close,
        "adjClose": close, "adjHigh": close * 1.001, "adjLow": close * 0.999,
        "adjOpen": close, "volume": 1000000, "adjVolume": 1000000,
        "divCash": div, "splitFactor": 1.0})
spy.write_text(json.dumps(spy_rows, separators=(",", ":")) + "\n")
say(f"synthetic inputs: FX={len(ids)} series x {len(calendar)} weekdays with empty holiday values, OECD={len(rate_rows)-1} rows, SPY={len(spy_rows)} session bars")
say("required fixture dates present:", all(any(row["date"].startswith(d) for row in spy_rows) for d in ("2010-01-04", "2019-11-01", "2024-10-31", "2024-11-01", "2026-10-30")))

rust = ["target/debug/h005-compute", "--fx-dir", fx, "--rates", rates, "--spy", spy]
py = ["uv", "run", "--locked", "--project", "stats", "python", "stats/h005.py"]
dev = root / "dev"
log_command([*rust, "--stage", "dev", "--out", dev])
log_command([*py, "--stage", "dev", "--run", dev])
dev_verdict = json.loads((dev / "verdict.json").read_text())
summary = json.loads((dev / "summary.json").read_text())
assert dev_verdict["match"] is True and dev_verdict["mismatches"] == []
assert all(summary[key] is True for key in ("gate_a", "gate_b", "gate_c")), summary
for name in ("summary.json", "verdict.json", "SHA256SUMS"):
    shutil.copy2(dev / name, evidence / f"dev-{name}")
    say(f"dev {name} sha256={sha(dev / name)}")
say("dev match=true; gates A/B/C=true")

def copy_inputs(tag):
    dest = evidence / tag
    shutil.copytree(inputs, dest)
    return dest

def compare_dev(tag, arg_index, edit):
    cloned = copy_inputs(tag)
    edit(cloned)
    cloned_fx, cloned_rates, cloned_spy = cloned / "fx", cloned / "OECD.csv", cloned / "SPY.json"
    out = root / f"future-{tag}"
    log_command(["target/debug/h005-compute", "--fx-dir", cloned_fx, "--rates", cloned_rates, "--spy", cloned_spy, "--stage", "dev", "--out", out])
    log_command([*py, "--stage", "dev", "--run", out])
    assert sorted(p.name for p in dev.iterdir()) == sorted(p.name for p in out.iterdir())
    for path in dev.iterdir():
        assert path.read_bytes() == (out / path.name).read_bytes(), f"{tag}: {path.name} differs"
    say(f"{tag} future-value invariance: all dev output files byte-identical")

def change_future_fx(folder):
    path = folder / "fx" / "DEXUSEU.csv"
    lines = path.read_text().splitlines()
    path.write_text("\n".join([lines[0], *[line.rsplit(",", 1)[0] + ",999999.0" if line[:10] > "2024-10-31" else line for line in lines[1:]]]) + "\n")
def change_future_rates(folder):
    path = folder / "OECD.csv"
    lines = path.read_text().splitlines()
    changed = [lines[0]]
    for line in lines[1:]:
        fields = line.split(",")
        if fields[10] >= "2024-10":
            fields[11] = "999999.0"
        changed.append(",".join(fields))
    path.write_text("\n".join(changed) + "\n")
def change_future_spy(folder):
    path = folder / "SPY.json"
    rows = json.loads(path.read_text())
    for row in rows:
        if row["date"][:10] > "2024-10-31":
            row.update(close=-1.0, high=-1.0, low=-1.0, open=-1.0, adjClose=-1.0, adjHigh=-1.0, adjLow=-1.0, adjOpen=-1.0)
    path.write_text(json.dumps(rows, separators=(",", ":")) + "\n")
compare_dev("future-fx", 0, change_future_fx)
compare_dev("future-rates", 1, change_future_rates)
compare_dev("future-spy", 2, change_future_spy)

manifest_hash = sha(dev / "SHA256SUMS")
data_hashes = {p.name: sha(p) for p in sorted(fx.glob("*.csv"))}
data_hashes.update({"OECD.csv": sha(rates), "SPY.json": sha(spy)})
run_line = {"seq": 1, "ts": "2026-11-13T00:00:00Z", "kind": "run", "hypothesis": "H005", "stage": "dev", "configs": 1, "n_cumulative": 5, "code_commit": "0" * 40, "data_sha256": data_hashes, "results_sha256": manifest_hash}
verdict_line = {"seq": 2, "ts": "2026-11-13T00:00:01Z", "kind": "verdict", "hypothesis": "H005", "stage": "dev", "gates": {"A": True, "B": True, "C": True}, "result": "pass", "values": {}, "results_sha256": manifest_hash, "recomputed_by": "synthetic-driver"}
open_line = {"seq": 3, "ts": "2026-11-13T00:00:02Z", "kind": "holdout_open", "hypothesis": "H005", "stage": "holdout", "reason": "Synthetic verification only"}
ledger.write_text("\n".join(json.dumps(row, separators=(",", ":")) for row in (run_line, verdict_line, open_line)) + "\n")
holdout = root / "holdout"
log_command([*rust, "--stage", "holdout", "--out", holdout, "--dev-match", dev / "verdict.json", "--ledger", ledger])
log_command([*py, "--stage", "holdout", "--run", holdout, "--dev-match", dev / "verdict.json", "--ledger", ledger, "--fx-dir", fx, "--rates", rates, "--spy", spy])
hold_verdict = json.loads((holdout / "verdict.json").read_text())
hold_summary = json.loads((holdout / "summary.json").read_text())
assert hold_verdict["match"] is True and hold_verdict["mismatches"] == []
assert hold_summary["gate_h"] is True, hold_summary
for name in ("summary.json", "verdict.json", "SHA256SUMS"):
    shutil.copy2(holdout / name, evidence / f"holdout-{name}")
    say(f"holdout {name} sha256={sha(holdout / name)}")
say("holdout match=true; gate H=true")

vintages = evidence / "first-release"
vintages.mkdir()
for series, _, _ in ids.values():
    shutil.copy2(fx / f"{series}.csv", vintages / f"{series}.csv")
rate_ids = dict(zip(("EA20", "JPN", "GBR", "CHE", "CAN", "AUS", "NZL", "NOR", "SWE", "USA"),
                   ("EZ", "JP", "GB", "CH", "CA", "AU", "NZ", "NO", "SE", "US")))
for area, code in rate_ids.items():
    series = f"IR3TIB01{code}M156N"
    observations = [line.split(",") for line in rates.read_text().splitlines()[1:]]
    start = "2013-04" if area in {"USA", "CAN"} else "2013-05"
    available = {row[10]: row[11] for row in observations if row[1] == area and row[10] >= start}
    if area == "USA":
        available["2020-04"] = ""
    values = [f"{month}-01,{value}" for month, value in sorted(available.items())]
    (vintages / f"{series}.csv").write_text(f"observation_date,{series}\n" + "\n".join(values) + "\n")
vintage_dev = root / "vintage-dev"
log_command([*rust, "--stage", "dev", "--out", vintage_dev, "--first-release-dir", vintages])
log_command([*py, "--stage", "dev", "--run", vintage_dev])
vintage_summary = json.loads((vintage_dev / "summary.json").read_text())
assert vintage_summary["report_tables"] == summary["report_tables"]
for row in vintage_summary["vintage"]["spot"] + vintage_summary["vintage"]["rate_signals"]:
    assert row["comparable_months"] > 0 and row["signal_flips"] == 0
for row in vintage_summary["vintage"]["rate_signals"]:
    expected = "2014-05-30" if row["currency"] == "CAD" else "2014-06-30"
    assert row["available_start"] == expected, row
say("staggered first-release rate starts: CAD=2014-05-30; other currencies=2014-06-30")
vintage_hash = sha(vintage_dev / "SHA256SUMS")
vintage_ledger = root / "vintage-ledger.jsonl"
vintage_ledger.write_text("".join(json.dumps(row) + "\n" for row in (
    dict(run_line, results_sha256=vintage_hash), dict(verdict_line, results_sha256=vintage_hash), open_line)))
vintage_holdout = root / "vintage-holdout"
log_command([*rust, "--stage", "holdout", "--out", vintage_holdout, "--first-release-dir", vintages,
             "--dev-match", vintage_dev / "verdict.json", "--ledger", vintage_ledger])
log_command([*py, "--stage", "holdout", "--run", vintage_holdout, "--dev-match", vintage_dev / "verdict.json",
             "--ledger", vintage_ledger, "--fx-dir", fx, "--rates", rates, "--spy", spy])
for stage, directory in (("dev", vintage_dev), ("holdout", vintage_holdout)):
    envelope = json.loads((directory / "verdict.json").read_text())
    assert envelope["match"] is True and envelope["mismatches"] == []
    for name in ("summary.json", "verdict.json", "SHA256SUMS", "observations.parquet", "indicators.parquet", "days.parquet"):
        shutil.copy2(directory / name, evidence / f"{stage}-vintage-{name}")
say("optional first-release FX and rates: both stages match=true; unchanged trading reports; no synthetic signal flips")

tampered = root / "tampered"
shutil.copytree(dev, tampered)
import pyarrow.parquet as pq
import pyarrow as pa
table = pq.read_table(tampered / "indicators.parquet")
rows = table.to_pylist()
rows[300]["S"] *= 1.01
pq.write_table(pa.Table.from_pylist(rows, schema=table.schema), tampered / "indicators.parquet")
refresh_manifest(tampered)
log_command([*py, "--stage", "dev", "--run", tampered], expect=1, error_substring="match=false")
assert json.loads((tampered / "verdict.json").read_text())["match"] is False
say("tampered indicator: Python reports match=false")

uk_gap = copy_inputs("uk-gap")
uk_rates = uk_gap / "OECD.csv"
lines = uk_rates.read_text().splitlines()
uk_rates.write_text("\n".join([lines[0], *[line for line in lines[1:] if not (line.split(",")[1] == "GBR" and line.split(",")[10] == "2026-03")]]) + "\n")
uk_dev = root / "uk-gap-dev"
log_command(["target/debug/h005-compute", "--fx-dir", uk_gap / "fx", "--rates", uk_rates, "--spy", uk_gap / "SPY.json", "--stage", "dev", "--out", uk_dev])
log_command([*py, "--stage", "dev", "--run", uk_dev])
assert json.loads((uk_dev / "verdict.json").read_text())["match"] is True
uk_hashes = {p.name: sha(p) for p in sorted((uk_gap / "fx").glob("*.csv"))}
uk_hashes.update({"OECD.csv": sha(uk_rates), "SPY.json": sha(uk_gap / "SPY.json")})
uk_manifest_hash = sha(uk_dev / "SHA256SUMS")
uk_run = dict(run_line, data_sha256=uk_hashes, results_sha256=uk_manifest_hash)
uk_verdict = dict(verdict_line, results_sha256=uk_manifest_hash)
uk_ledger = root / "uk-gap-ledger.jsonl"
uk_ledger.write_text("\n".join(json.dumps(row, separators=(",", ":")) for row in (uk_run, uk_verdict, open_line)) + "\n")
uk_holdout = root / "refuse-uk-rust"
log_command(["target/debug/h005-compute", "--fx-dir", uk_gap / "fx", "--rates", uk_rates, "--spy", uk_gap / "SPY.json", "--stage", "holdout", "--out", uk_holdout, "--dev-match", uk_dev / "verdict.json", "--ledger", uk_ledger], expect=1, error_substring="rate gap exceeds five business days")
assert not uk_holdout.exists()
uk_template = root / "refuse-uk-python"
shutil.copytree(holdout, uk_template)
uk_table = pq.read_table(uk_template / "observations.parquet")
uk_rows = [row for row in uk_table.to_pylist() if not (row["kind"] == "rate" and row["series"] == "GBR" and row["date"].startswith("2026-03-"))]
pq.write_table(pa.Table.from_pylist(uk_rows, schema=uk_table.schema), uk_template / "observations.parquet")
refresh_manifest(uk_template)
uk_before = {p.name: p.read_bytes() for p in uk_template.iterdir()}
log_command([*py, "--stage", "holdout", "--run", uk_template, "--dev-match", uk_dev / "verdict.json", "--ledger", uk_ledger, "--fx-dir", uk_gap / "fx", "--rates", uk_rates, "--spy", uk_gap / "SPY.json"], expect=1, error_substring="rate unavailable beyond five business days")
assert {p.name: p.read_bytes() for p in uk_template.iterdir()} == uk_before
say("missing UK 2026-03 rate: Rust and Python refuse holdout without changing output")

refusal_cases = ["no-envelope", "envelope-false", "no-dev-run", "no-verdict", "failed-gate", "bad-ledger-hash", "bad-envelope-hash", "divergent-python-summary", "no-open", "used-holdout", "changed-input"]
base_rows = [run_line, verdict_line, open_line]
for case in refusal_cases:
    case_ledger = root / f"ledger-{case}.jsonl"
    rows = list(base_rows)
    envelope_path = dev / "verdict.json"
    envelope = json.loads(envelope_path.read_text())
    if case == "no-dev-run":
        rows = [dict(verdict_line, seq=1), dict(open_line, seq=2)]
    elif case == "no-verdict":
        rows = [run_line, dict(open_line, seq=2)]
    elif case == "failed-gate":
        rows[1] = dict(verdict_line, gates={"A": False, "B": True, "C": True}, result="reject")
    elif case == "bad-ledger-hash":
        rows[1] = dict(verdict_line, results_sha256="f" * 64)
    elif case == "no-open":
        rows = [run_line, verdict_line]
    elif case == "used-holdout":
        rows = [run_line, verdict_line, open_line, {"seq": 4, "ts": "2026-11-13T00:00:03Z", "kind": "run", "hypothesis": "H005", "stage": "holdout", "configs": 0, "n_cumulative": 5, "code_commit": "0" * 40, "data_sha256": data_hashes, "results_sha256": "e" * 64}]
    elif case == "envelope-false":
        envelope["match"] = False
    elif case == "bad-envelope-hash":
        envelope["results_sha256"] = "f" * 64
    envelope_dir = root / f"dev-match-{case}"
    shutil.copytree(dev, envelope_dir)
    envelope_file = envelope_dir / "verdict.json"
    if case == "divergent-python-summary":
        envelope["python_summary"]["main"]["W"] = -987654321.0
    envelope_file.write_text(json.dumps(envelope))
    case_ledger.write_text("\n".join(json.dumps(row, separators=(",", ":")) for row in rows) + "\n")
    case_fx, case_rates, case_spy = fx, rates, spy
    if case == "changed-input":
        changed = copy_inputs("changed-input")
        p = changed / "fx" / "DEXUSEU.csv"
        lines = p.read_text().splitlines()
        lines[1] = lines[1].rsplit(",", 1)[0] + ",7.654321"
        p.write_text("\n".join(lines) + "\n")
        case_fx, case_rates, case_spy = changed / "fx", changed / "OECD.csv", changed / "SPY.json"
    expected_errors = {
        "no-envelope": {"rust": "holdout requires --dev-match", "python": "holdout requires dev match envelope and ledger"},
        "envelope-false": {"rust": "dev comparison must match", "python": "dev independent recomputation must match"},
        "no-dev-run": {"rust": "missing H005 dev run", "python": "holdout requires matching ledger dev run"},
        "no-verdict": {"rust": "missing H005 dev ledger verdict", "python": "holdout requires matching ledger passing verdict"},
        "failed-gate": {"rust": "H005 dev ledger gates or results mismatch", "python": "holdout requires passing ledger gates and results"},
        "bad-ledger-hash": {"rust": "H005 dev ledger gates or results mismatch", "python": "holdout requires passing ledger gates and results"},
        "bad-envelope-hash": {"rust": "dev envelope results mismatch", "python": "dev envelope results hash mismatch"},
        "divergent-python-summary": {"rust": "dev envelope results mismatch", "python": "dev envelope summary mismatch"},
        "no-open": {"rust": "missing H005 holdout_open", "python": "holdout must be explicitly opened after dev verdict"},
        "used-holdout": {"rust": "H005 holdout already used", "python": "holdout already consumed"},
        "changed-input": {"rust": "dev run input or result hashes mismatch", "python": "holdout raw input or dev results hash mismatch"},
    }
    for cli in ("rust", "python"):
        dest = root / f"refuse-{case}-{cli}"
        if cli == "rust":
            cmd = ["target/debug/h005-compute", "--fx-dir", case_fx, "--rates", case_rates, "--spy", case_spy, "--stage", "holdout", "--out", dest]
            if case != "no-envelope":
                cmd.extend(["--dev-match", envelope_file])
            cmd.extend(["--ledger", case_ledger])
        else:
            template = root / f"refusal-template-{case}-{cli}"
            shutil.copytree(holdout, template)
            before = {p.name: p.read_bytes() for p in template.iterdir()}
            cmd = [*py, "--stage", "holdout", "--run", template]
            if case != "no-envelope":
                cmd.extend(["--dev-match", envelope_file])
            cmd.extend(["--ledger", case_ledger, "--fx-dir", case_fx, "--rates", case_rates, "--spy", case_spy])
        log_command(cmd, expect=1, error_substring=expected_errors[case][cli])
        if cli == "rust":
            assert not dest.exists(), f"{case}: Rust wrote output despite refusal"
        else:
            assert {p.name: p.read_bytes() for p in template.iterdir()} == before, f"{case}: Python changed output despite refusal"
    say(f"holdout refusal {case}: Rust and Python exit 1; no result writes")

gap_src = fx / "DEXUSEU.csv"
base = gap_src.read_text().splitlines()
for count, label in ((5, "fx-five-day-gap"), (6, "fx-sixth-day-gap")):
    altered = copy_inputs(label)
    p = altered / "fx" / "DEXUSEU.csv"
    rows = p.read_text().splitlines()
    target_start = next(i for i, line in enumerate(rows) if line.startswith("2018-02-05,"))
    for i in range(target_start, target_start + count):
        fields = rows[i].split(",")
        rows[i] = fields[0] + ","
    p.write_text("\n".join(rows) + "\n")
    dest = root / label
    status = 0 if count == 5 else 1
    log_command(["target/debug/h005-compute", "--fx-dir", altered / "fx", "--rates", altered / "OECD.csv", "--spy", altered / "SPY.json", "--stage", "dev", "--out", dest], expect=status, error_substring="FX gap exceeds five business days" if count == 6 else None)
    if count == 6:
        assert not dest.exists()
say("FX gap: five weekdays accepted and sixth weekday refused")

invalid_edits = {
    "missing-fred-column": lambda folder: (folder / "fx" / "DEXUSEU.csv").write_text("date,wrong\n2008-01-02,1\n"),
    "duplicate-fred-date": lambda folder: (folder / "fx" / "DEXUSEU.csv").write_text((folder / "fx" / "DEXUSEU.csv").read_text().replace("2008-01-03,", "2008-01-02,", 1)),
    "zero-fx": lambda folder: mutate_csv_value(folder / "fx" / "DEXUSEU.csv", "2008-01-02", 0),
    "negative-spy": lambda folder: mutate_spy(folder / "SPY.json", "2008-01-02", -1),
    "duplicate-rate": lambda folder: duplicate_rate(folder / "OECD.csv"),
}
def mutate_csv_value(path, day, value):
    lines = path.read_text().splitlines()
    for i, line in enumerate(lines):
        if line.startswith(day + ","):
            lines[i] = day + "," + str(value)
            break
    path.write_text("\n".join(lines) + "\n")
def mutate_spy(path, day, value):
    rows = json.loads(path.read_text())
    next(row for row in rows if row["date"].startswith(day))["close"] = value
    path.write_text(json.dumps(rows, separators=(",", ":")) + "\n")
def duplicate_rate(path):
    lines = path.read_text().splitlines()
    row = next(line for line in lines[1:] if line.split(",")[1:3] == ["GBR", "M"])
    lines.append(row)
    path.write_text("\n".join(lines) + "\n")
for label, edit in invalid_edits.items():
    altered = copy_inputs(label)
    edit(altered)
    out = root / f"invalid-{label}"
    invalid_errors = {
        "missing-fred-column": "invalid FRED header",
        "duplicate-fred-date": "FRED dates must increase without duplicates",
        "zero-fx": "FX spot must be positive",
        "negative-spy": "close must be positive",
        "duplicate-rate": "duplicate OECD area/month",
    }
    log_command(["target/debug/h005-compute", "--fx-dir", altered / "fx", "--rates", altered / "OECD.csv", "--spy", altered / "SPY.json", "--stage", "dev", "--out", out], expect=1, error_substring=invalid_errors[label])
    assert not out.exists(), f"{label}: Rust wrote output despite invalid input"
    say(f"invalid input {label}: Rust exits 1 without output")

bad_observations = root / "invalid-observations"
shutil.copytree(dev, bad_observations)
table = pq.read_table(bad_observations / "observations.parquet")
rows = table.to_pylist()
row = next(row for row in rows if row["kind"] == "fx" and row["series"] == "DEXUSEU" and row["date"] == "2008-01-02")
row["value"] = 0.0
pq.write_table(pa.Table.from_pylist(rows, schema=table.schema), bad_observations / "observations.parquet")
refresh_manifest(bad_observations)
log_command([*py, "--stage", "dev", "--run", bad_observations], expect=1, error_substring="invalid observation value")
say("invalid persisted FX observation: Python exits 1")

say("PASS synthetic H005 dev/holdout, Python match, gates, tamper, invariance, and refusal paths")
PY
cat "$evidence/transcript.txt"
echo "PASS evidence=$evidence"
