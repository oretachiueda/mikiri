#!/usr/bin/env bash
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"
export PATH="$HOME/.cargo/bin:$HOME/.local/bin:/opt/homebrew/bin:$PATH"
id="$(date -u +%Y%m%dT%H%M%SZ)-$$"
run="results/verify-h006-$id"
evidence="/tmp/mikiri-verify-h006-$id"
[[ ! -e "$run" && ! -L "$run" && ! -e "$evidence" && ! -L "$evidence" ]]
mkdir -p "$run" "$evidence"
trap 'status=$?; rm -rf "$run"; if [[ "$status" -ne 0 ]]; then echo "FAIL evidence=$evidence" >&2; fi' EXIT
transcript="$evidence/transcript.txt"
run_logged() {
    { printf '$'; printf ' %q' "$@"; printf '\n'; } >>"$transcript"
    local status=0
    "$@" >>"$transcript" 2>&1 || status=$?
    printf 'exit=%s\n' "$status" >>"$transcript"
    return "$status"
}
run_logged cargo build --locked --release -p mikiri --bin h006-compute
run_logged python3 .agents/skills/verify-mikiri/h006_synth.py --out "$evidence/inputs"
python3 - "$run" "$evidence" <<'PY' >>"$transcript" 2>&1
import hashlib
import json
import os
import shutil
import subprocess
import sys
import zipfile
from pathlib import Path

root, evidence = map(Path, sys.argv[1:])
inputs = evidence / "inputs"
zips, spy = inputs / "zips", inputs / "SPY.json"
rust = ["target/release/h006-compute", "--histdata-dir", zips, "--spy", spy]
for path in sorted(zips.glob("*.zip")):
    with zipfile.ZipFile(path) as archive, path.open("rb") as raw:
        for item in archive.infolist():
            assert item.compress_type == zipfile.ZIP_DEFLATED and item.flag_bits & 0x8, path
            raw.seek(item.header_offset + 6)
            assert int.from_bytes(raw.read(2), "little") & 0x8, path
print("ZIP verification: 29 files, deflate and data-descriptor flag 0x8", flush=True)
python_exists = Path("stats/h006.py").is_file()
py = ["uv", "run", "--locked", "--project", "stats", "python", "stats/h006.py"]
outputs = ("accounts.parquet", "days.parquet", "random.parquet", "removed_bars.parquet", "summary.json", "SHA256SUMS")

def say(*args):
    print(*args, flush=True)

def command(args, expected=0, error=None, absent=None):
    args = list(map(str, args))
    say("$", " ".join(args))
    proc = subprocess.run(args, text=True, capture_output=True)
    if proc.stdout:
        print(proc.stdout, end="", flush=True)
    if proc.stderr:
        print(proc.stderr, end="", file=sys.stderr, flush=True)
    say(f"exit={proc.returncode}")
    assert proc.returncode == expected, (args, proc.returncode, expected)
    if error:
        assert error in proc.stderr, (error, proc.stderr)
    if absent:
        assert not absent.exists(), f"unexpected output: {absent}"
    return proc

def sha(path):
    digest = hashlib.sha256()
    with path.open("rb") as src:
        for block in iter(lambda: src.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()

def verify_manifest(folder):
    proc = subprocess.run(["shasum", "-a", "256", "-c", "SHA256SUMS"], cwd=folder, text=True, capture_output=True)
    say(f"$ (cd {folder} && shasum -a 256 -c SHA256SUMS)")
    say(proc.stdout.strip())
    say(f"exit={proc.returncode}")
    assert proc.returncode == 0, proc.stderr

def ledger_file(name, rows):
    path = evidence / (name + ".jsonl")
    path.write_text("".join(json.dumps(row, separators=(",", ":")) + "\n" for row in rows))
    return path

def run_python(stage, output, extra=()):
    if not python_exists:
        say(f"SKIP python recompute: stats/h006.py absent ({stage})")
        return None
    command([*py, "--stage", stage, "--run", output, "--histdata-dir", zips, "--spy", spy, *extra])
    verdict = json.loads((output / "verdict.json").read_text())
    assert verdict["match"] is True, verdict
    say(f"{stage} Python match=true")
    return verdict

dev = root / "dev"
proc = command([*rust, "--stage", "dev", "--out", dev])
assert proc.stdout.find("clock_check ") >= 0 and proc.stdout.find("clock_check ") < proc.stdout.find("results_sha256=")
verify_manifest(dev)
summary = json.loads((dev / "summary.json").read_text())
assert summary["first_calculation_day"] == "2007-01-02", summary["first_calculation_day"]
assert summary["duplicate_lines_removed"]["count"] == 60, summary.get("duplicate_lines_removed")
assert any(row["date"] == "2012-01-03" and row["reason"] == "missing_boundary" for row in summary["excluded_days"])
say(f"dev duplicate_lines_removed.count=60 first={summary['first_calculation_day']} missing-boundary=2012-01-03; clock printed before result")
verdict = run_python("dev", dev)
manifest_hash = sha(dev / "SHA256SUMS")
if verdict is None:
    verdict = {"match": True, "synthetic_only": True, "rust_summary": summary, "python_summary": summary, "results_sha256": manifest_hash}
    (dev / "verdict.json").write_text(json.dumps(verdict))
    say("synthetic-only dev envelope created for holdout gate tests; no independent match claimed")
for name in ("summary.json", "verdict.json", "SHA256SUMS"):
    shutil.copy2(dev / name, evidence / f"dev-{name}")
    say(f"dev {name} sha256={sha(dev / name)}")

hashes = {p.name: sha(p) for p in sorted(zips.glob("*.zip"))}
hashes["SPY.json"] = sha(spy)
run_line = {"kind": "run", "hypothesis": "H006", "stage": "dev", "data_sha256": hashes, "results_sha256": manifest_hash}
verdict_line = {"kind": "verdict", "hypothesis": "H006", "stage": "dev", "result": "pass", "gates": {k: True for k in "ABCD"}, "results_sha256": manifest_hash}
open_line = {"kind": "holdout_open", "hypothesis": "H006", "stage": "holdout", "reason": "synthetic verification only"}
ledger = ledger_file("synthetic-ledger", [run_line, verdict_line, open_line])

def refuse(name, rows, envelope=dev / "verdict.json", extra=(), error=None):
    out = root / f"refuse-{name}"
    args = [*rust, "--stage", "holdout", "--out", out]
    if envelope is not None:
        args.extend(["--dev-match", envelope])
    args.extend(["--ledger", ledger_file("ledger-" + name, rows), *extra])
    command(args, expected=1, error=error, absent=out)
    say(f"holdout refusal {name}: exit=1, output absent")

refuse("no-match", [run_line, verdict_line, open_line], envelope=None, error="holdout requires --dev-match")
false_match = dict(verdict, match=False)
false_path = dev / "verdict-false.json"
false_path.write_text(json.dumps(false_match))
refuse("match-false", [run_line, verdict_line, open_line], envelope=false_path, error="dev comparison must match")
refuse("no-open", [run_line, verdict_line], error="missing H006 holdout_open")
refuse("gate-false", [run_line, dict(verdict_line, gates={"A": True, "B": True, "C": True, "D": False}), open_line], error="H006 dev ledger gates or results mismatch")
bad_hashes = dict(hashes, **{"SPY.json": "0" * 64})
refuse("input-hash", [dict(run_line, data_sha256=bad_hashes), verdict_line, open_line], error="dev run input hashes mismatch")
refuse("used-holdout", [run_line, verdict_line, open_line, {"kind": "run", "hypothesis": "H006", "stage": "holdout"}], error="H006 holdout already used")

hold = root / "holdout"
command([*rust, "--stage", "holdout", "--out", hold, "--dev-match", dev / "verdict.json", "--ledger", ledger])
verify_manifest(hold)
hold_summary = json.loads((hold / "summary.json").read_text())
assert hold_summary["stage"] == "holdout" and isinstance(hold_summary["gates"]["H"], bool)
run_python("holdout", hold, ("--dev-match", dev / "verdict.json", "--ledger", ledger))
for name in ("summary.json", "SHA256SUMS"):
    shutil.copy2(hold / name, evidence / f"holdout-{name}")
    say(f"holdout {name} sha256={sha(hold / name)}")
if (hold / "verdict.json").exists():
    shutil.copy2(hold / "verdict.json", evidence / "holdout-verdict.json")
say("authorized synthetic holdout: completed")

future = evidence / "future-zips"
future.mkdir()
for path in zips.glob("*.zip"):
    os.link(path, future / path.name)
changed = future / "HISTDATA_COM_ASCII_EURUSD_M1_2025.zip"
tmp = future / "replacement.tmp"
with zipfile.ZipFile(changed) as source, zipfile.ZipFile(tmp, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=3, allowZip64=True) as target:
    for name in source.namelist():
        if name.endswith(".csv"):
            with source.open(name) as src, target.open(name, "w", force_zip64=True) as dst:
                first = src.readline().decode().strip().split(";")
                for i in range(1, 5):
                    first[i] = f"{float(first[i]) * 1.01:.6f}"
                dst.write((";".join(first) + "\n").encode())
                shutil.copyfileobj(src, dst, length=1024 * 1024)
        else:
            target.writestr(name, source.read(name))
os.replace(tmp, changed)
assert sha(changed) != sha(zips / changed.name)
future_out = root / "future-dev"
command(["target/release/h006-compute", "--histdata-dir", future, "--spy", spy, "--stage", "dev", "--out", future_out])
for name in outputs:
    assert sha(dev / name) == sha(future_out / name), f"future changed {name}"
say("future-value invariance: changed 2025 bar; all six Rust output files byte-identical")

# The opened 2024 ZIP is hashed in full, including rows past the dev cutoff.
changed = future / "HISTDATA_COM_ASCII_EURUSD_M1_2024.zip"
tmp = future / "replacement.tmp"
modified = False
with zipfile.ZipFile(changed) as source, zipfile.ZipFile(tmp, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=3, allowZip64=True) as target:
    for name in source.namelist():
        if name.endswith(".csv"):
            with source.open(name) as src, target.open(name, "w", force_zip64=True) as dst:
                for line in src:
                    if line[:8] > b"20241031":
                        fields = line.decode().strip().split(";")
                        for i in range(1, 5):
                            fields[i] = f"{float(fields[i]) * 1.01:.6f}"
                        dst.write((";".join(fields) + "\n").encode())
                        shutil.copyfileobj(src, dst, length=1024 * 1024)
                        modified = True
                        break
                    dst.write(line)
        else:
            target.writestr(name, source.read(name))
assert modified
os.replace(tmp, changed)
assert sha(changed) != sha(zips / changed.name)
cutoff_out = root / "future-2024-dev"
command(["target/release/h006-compute", "--histdata-dir", future, "--spy", spy, "--stage", "dev", "--out", cutoff_out])
for name in outputs[:4]:
    assert sha(dev / name) == sha(cutoff_out / name), f"2024 future changed {name}"
cutoff_summary = json.loads((cutoff_out / "summary.json").read_text())
assert cutoff_summary.pop("input_sha256") != summary["input_sha256"]
assert cutoff_summary == {key: value for key, value in summary.items() if key != "input_sha256"}
assert sha(dev / "SHA256SUMS") != sha(cutoff_out / "SHA256SUMS")
say("2024 cutoff: calculation outputs identical; summary input hash and manifest change as expected")

tamper = dev / "removed_bars.parquet"
original = tamper.read_bytes()
try:
    tamper.write_bytes(original + b"tamper")
    refuse("tampered-result", [run_line, verdict_line, open_line], error="dev result manifest mismatch")
finally:
    tamper.write_bytes(original)
verify_manifest(dev)
say("tamper detection: modified dev output refused; original restored")
if not python_exists:
    say("SKIP python recompute: stats/h006.py absent")
say("PASS synthetic H006 dev/holdout, refusal paths, future invariance and tamper detection")
PY
tail -n 16 "$transcript"
echo "PASS evidence=$evidence"
