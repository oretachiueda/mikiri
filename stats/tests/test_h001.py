import hashlib
import json
import math
import shutil
import subprocess
import sys
import tempfile
from dataclasses import asdict, replace
from datetime import date, timedelta
from pathlib import Path

import pandas as pd
import pyarrow as pa
import pyarrow.parquet as pq
import pytest

from h001 import Bar, compare, compute, cycles, psr, read_bars, run


def fake_bars(start: date, end: date) -> list[Bar]:
    days = [start + timedelta(days=i) for i in range((end - start).days + 1)]
    return [
        Bar(d, 100 + i * 0.01 + math.sin(i / 7), 0.0, 1.0)
        for i, d in enumerate(days)
        if d.weekday() < 5 and d != date(2026, 9, 7)
    ]


def write_bars(path: Path, bars: list[Bar]) -> None:
    pq.write_table(pa.Table.from_pylist([asdict(b) for b in bars]), path)


def test_october_window_and_cycle():
    bars = fake_bars(date(2026, 8, 31), date(2026, 11, 3))
    cycle = next(c for c in cycles(bars) if c.plus1 == date(2026, 10, 1))
    start = next(i for i, b in enumerate(bars) if b.date == date(2026, 9, 30))
    assert [b.date for b in bars[start : start + 4]] == [
        date(2026, 9, 30),
        date(2026, 10, 1),
        date(2026, 10, 2),
        date(2026, 10, 5),
    ]
    assert bars[start + cycle.in_days + cycle.out_days - 1].date == date(2026, 10, 29)
    assert (cycle.in_days, cycle.out_days) == (4, 18)
    returns = [
        (b.close * b.split_factor + b.div_cash) / a.close - 1
        for a, b in zip(
            bars[start - 1 : start + 3], bars[start : start + 4], strict=True
        )
    ]
    assert cycle.in_mean == pytest.approx(sum(returns) / 4, abs=1e-15)


def test_psr_fixed_value():
    assert psr([0.001, -0.002, 0.003, 0.0005, -0.001, 0.002, 0.0015, -0.0005]) == (
        pytest.approx(0.8228899111257035, rel=1e-9)
    )


def test_future_prices_do_not_change_completed_cycles():
    bars = fake_bars(date(2026, 6, 1), date(2026, 11, 3))
    boundary = date(2026, 9, 1)
    changed = [replace(b, close=b.close * 3) if b.date > boundary else b for b in bars]
    original = [c.d for c in cycles(bars) if c.plus1 < date(2026, 9, 1)]
    assert original
    assert original == [c.d for c in cycles(changed) if c.plus1 < date(2026, 9, 1)]


def test_same_day_dividend_and_split():
    bars = fake_bars(date(2026, 8, 1), date(2026, 11, 3))
    index = next(i for i, b in enumerate(bars) if b.date == date(2026, 10, 1))
    changed = bars.copy()
    changed[index] = replace(bars[index], div_cash=2, split_factor=2)
    before = next(c for c in cycles(bars) if c.plus1 == date(2026, 10, 1))
    after = next(c for c in cycles(changed) if c.plus1 == date(2026, 10, 1))
    assert after.in_mean - before.in_mean == pytest.approx(
        (bars[index].close + 2) / bars[index - 1].close / 4
    )


@pytest.mark.parametrize(
    "damage", ["missing", "reverse", "duplicate", "type", "null", "nan", "zero"]
)
def test_invalid_bars(tmp_path, damage):
    bars = fake_bars(date(2026, 8, 1), date(2026, 11, 3))
    frame = pd.DataFrame([asdict(b) for b in bars])
    if damage == "missing":
        frame = frame.drop(columns="close")
    elif damage == "reverse":
        frame = frame.iloc[::-1]
    elif damage == "duplicate":
        frame.loc[1, "date"] = frame.loc[0, "date"]
    elif damage == "type":
        frame["date"] = frame["date"].astype(str)
    elif damage == "null":
        frame.loc[0, "close"] = None
    elif damage == "nan":
        frame.loc[0, "close"] = float("inf")
    else:
        frame.loc[0, "close"] = 0.0
    path = tmp_path / "bars.parquet"
    frame.to_parquet(path)
    with pytest.raises(ValueError):
        read_bars(path)


@pytest.fixture
def full_bars():
    return fake_bars(date(2005, 11, 1), date(2026, 11, 3))


def test_coverage_and_counts(full_bars, monkeypatch):
    calculated, summary = compute(full_bars)
    assert summary["main_n"] == 250
    assert summary["recent_n"] == 120
    assert calculated[-1].plus1 == date(2026, 10, 1)
    with pytest.raises(ValueError, match="coverage"):
        compute(full_bars[100:])
    with pytest.raises(ValueError, match="coverage"):
        compute(full_bars[:-20])
    with pytest.raises(ValueError, match="gap"):
        compute(full_bars[:100] + full_bars[110:])
    monkeypatch.setattr("h001.cycles", lambda _: calculated[:-1])
    with pytest.raises(ValueError, match="250 main"):
        compute(full_bars)


@pytest.mark.parametrize("values", [[], [1.0], [1.0, 1.0], [float("nan"), 1]])
def test_invalid_psr(values):
    with pytest.raises(ValueError):
        psr(values)


def test_comparison(full_bars):
    calculated, summary = compute(full_bars)
    rust = [asdict(c) for c in calculated]
    assert compare(calculated, summary, rust, summary) == []
    rust[0]["d"] += 1e-6
    assert compare(calculated, summary, rust, summary) == ["cycles[0].d: mismatch"]
    assert compare(
        calculated,
        summary,
        [asdict(c) for c in calculated],
        summary | {"gate_a": not summary["gate_a"]},
    ) == ["summary.gate_a: mismatch"]
    assert compare(calculated, summary, rust, summary | {"psr_main": float("nan")})
    assert compare(calculated, summary, rust[:-1], summary)


def test_cli_match_and_mismatch(full_bars):
    root = Path(__file__).resolve().parents[2]
    results = root / "results"
    results.mkdir(exist_ok=True)
    directory = Path(tempfile.mkdtemp(prefix="h001-test-", dir=results))
    try:
        write_bars(directory / "bars.parquet", full_bars)
        calculated, summary = compute(full_bars)
        pd.DataFrame([asdict(c) for c in calculated]).to_parquet(
            directory / "cycles.parquet"
        )
        (directory / "summary.json").write_text(json.dumps(summary))
        for mismatch in (False, True):
            if mismatch:
                summary["mean_d_main"] = 0.01
                (directory / "summary.json").write_text(json.dumps(summary))
            completed = subprocess.run(
                [sys.executable, str(root / "stats/h001.py"), "--run", str(directory)],
                capture_output=True,
                text=True,
                check=False,
            )
            assert completed.returncode == int(mismatch), completed.stderr
            verdict_path = directory / "verdict.json"
            verdict = json.loads(verdict_path.read_text())
            assert verdict["match"] is not mismatch
            digest = hashlib.sha256(verdict_path.read_bytes()).hexdigest()
            assert (
                completed.stdout
                == f"{verdict_path} sha256={digest} match={str(not mismatch).lower()}\n"
            )
            assert bool(verdict["mismatches"]) is mismatch
            assert set(verdict) == {
                "python_summary",
                "rust_summary",
                "match",
                "mismatches",
            }
        assert not list(directory.glob("tmp*"))
    finally:
        shutil.rmtree(directory)


def test_run_rejects_outside_results_and_symlink(tmp_path):
    with pytest.raises(ValueError, match="under repository results"):
        run(tmp_path)
    root = Path(__file__).resolve().parents[2]
    results = root / "results"
    results.mkdir(exist_ok=True)
    directory = Path(tempfile.mkdtemp(prefix="h001-path-test-", dir=results))
    try:
        link = directory / "escape"
        link.symlink_to(tmp_path, target_is_directory=True)
        with pytest.raises(ValueError, match="under repository results"):
            run(link)
    finally:
        shutil.rmtree(directory)
