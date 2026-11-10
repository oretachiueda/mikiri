import hashlib
import json
import shutil
import subprocess
import sys
import tempfile
from datetime import date
from pathlib import Path

import pyarrow as pa
import pyarrow.parquet as pq
import pytest

ROOT = Path(__file__).resolve().parents[2]


def months() -> list[dict]:
    rows = []
    for offset in range(371):
        year, month = divmod(1993 * 12 + 11 + offset, 12)
        p05 = 6 + (0.125 if offset % 2 else -0.125)
        rows.append(
            {
                "month_end": date(year, month + 1, 28),
                "period": "main" if offset else "reference",
                "recent": offset >= 251,
                "p01": 1.0,
                "p02": 2.0,
                "p03": 3.0,
                "p04": 4.0,
                "p05": p05,
                "ls": p05 - 1,
                "e": p05 - (10 + p05) / 5,
            }
        )
    return rows


def cli(run: Path) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, str(ROOT / "stats/h002.py"), "--run", str(run)],
        capture_output=True,
        text=True,
        check=False,
    )


@pytest.fixture
def directory():
    results = ROOT / "results"
    results.mkdir(exist_ok=True)
    run = Path(tempfile.mkdtemp(prefix="h002-test-", dir=results))
    try:
        yield run.resolve()
    finally:
        shutil.rmtree(run)


def test_cli_match_and_mismatches(directory):
    pq.write_table(pa.Table.from_pylist(months()), directory / "months.parquet")
    original = {
        "input_sha256": "0" * 64,
        "main_n": 370,
        "recent_n": 120,
        "reference_n": 1,
        "reference_excluded": 0,
        "mean_e_main": 2.8,
        "mean_e_recent": 2.8,
        "mean_e_reference": 2.7,
        "psr_main": 1.0,
        "annual_e_recent": 33.6,
        "gate_a": True,
        "gate_b": True,
        "gate_c": True,
        "gate_d": True,
        "gate_d_cost2x": True,
        "mean_ls_main": 5.0,
        "mean_ls_recent": 5.0,
        "port_means_main": [1, 2, 3, 4, 6],
        "port_means_recent": [1, 2, 3, 4, 6],
        "port_means_reference": [1, 2, 3, 4, 5.875],
    }
    for change, mismatch in [
        ({"psr_main": 1.0 - 5e-10}, None),
        ({"psr_main": 1.0 - 2e-9}, "psr_main"),
        ({"mean_e_main": 2.8 + 2e-12}, "mean_e_main"),
        ({"gate_d": 1}, "gate_d"),
        ({"port_means_main": [1, 2, 3, 4, 6.01]}, "port_means_main"),
        ({"recent_n": 119}, "recent_n"),
        ({"input_sha256": "bad"}, "input_sha256"),
    ]:
        (directory / "summary.json").write_text(json.dumps(original | change))
        completed = cli(directory)
        assert completed.returncode == int(mismatch is not None), completed.stderr
        assert completed.stderr == ""
        target = directory / "verdict.json"
        payload = target.read_bytes()
        verdict = json.loads(payload)
        assert verdict["match"] is (mismatch is None)
        assert set(verdict) == {"python_summary", "rust_summary", "match", "mismatches"}
        if mismatch is None:
            assert verdict["mismatches"] == []
        else:
            assert any(
                item.startswith(f"summary.{mismatch}:")
                for item in verdict["mismatches"]
            )
        digest = hashlib.sha256(payload).hexdigest()
        assert (
            completed.stdout
            == f"{target} sha256={digest} match={str(mismatch is None).lower()}\n"
        )
    assert not list(directory.glob("tmp*"))


def test_cli_rejects_missing_recent_month(directory):
    pq.write_table(pa.Table.from_pylist(months()[:-1]), directory / "months.parquet")
    completed = cli(directory)
    assert completed.returncode == 1
    assert completed.stderr == "H002 input or computation failed\n"
    assert completed.stdout == ""
    assert not (directory / "verdict.json").exists()


def test_cli_rejects_run_outside_results(tmp_path):
    completed = cli(tmp_path)
    assert completed.returncode == 1
    assert completed.stderr == "H002 input or computation failed\n"
    assert completed.stdout == ""
    assert not (tmp_path / "verdict.json").exists()


def test_recomputed_month_fields_detect_mismatches():
    from h002 import compute

    rows = months()
    rows[0].update(period="main", recent=True, e=2.75)
    _, mismatches = compute(rows)
    assert mismatches == [
        "months[0].period: mismatch",
        "months[0].recent: mismatch",
        "months[0].e: mismatch",
    ]
