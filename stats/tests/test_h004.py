import json
import math
from dataclasses import replace
from datetime import date, timedelta

import pytest

from h004 import (
    END,
    Bar,
    TaxLedger,
    above_mean,
    compare,
    compute,
    matches,
    next_signal,
    psr,
    require_dev_verdict,
    signal_history,
    simulate,
    validate_bars,
)


def raw_bar(day, close, dividend=0.0, split=1.0):
    return {
        "date": f"{day}T00:00:00.000Z",
        "close": float(close),
        "high": float(close * 1.01),
        "low": float(close * 0.99),
        "open": float(close),
        "adjClose": float(close),
        "adjHigh": float(close * 1.01),
        "adjLow": float(close * 0.99),
        "adjOpen": float(close),
        "volume": 1000,
        "adjVolume": 1000,
        "divCash": float(dividend),
        "splitFactor": float(split),
    }


def parse_rows(rows):
    return [
        Bar(
            date.fromisoformat(row["date"][:10]),
            row["close"],
            row["divCash"],
            row["splitFactor"],
        )
        for row in rows
    ]


@pytest.fixture(scope="module")
def synthetic():
    days = []
    current = date(2006, 1, 2)
    while current <= date(2026, 10, 30):
        if current.weekday() < 5 and current != date(2008, 1, 1):
            days.append(current)
        current += timedelta(days=1)
    return {
        "SPY": parse_rows(
            [
                raw_bar(day, 100 * math.exp(i * 0.0003 + 0.06 * math.sin(i / 13)))
                for i, day in enumerate(days)
            ]
        ),
        "SSO": parse_rows(
            [
                raw_bar(day, 40 * math.exp(i * 0.0005 + 0.12 * math.sin(i / 13)))
                for i, day in enumerate(days)
            ]
        ),
    }


def test_index_averages_rsi_seed_recursion_and_zero():
    days = [date(2000, 1, 1) + timedelta(days=i) for i in range(205)]
    bars = parse_rows(
        [
            raw_bar(day, close)
            for day, close in zip(days[:5], [100, 110, 105, 120, 100], strict=True)
        ]
    )
    indicators = signal_history(bars)
    assert [row["index"] for row in indicators] == pytest.approx([1, 1.1, 1.05, 1.2, 1])
    assert indicators[0]["rsi"] is None and indicators[1]["rsi"] is None
    assert indicators[2]["rsi"] == pytest.approx(100 * 0.05 / 0.075)
    assert indicators[3]["rsi"] == pytest.approx(100 * 0.1 / 0.1125)
    assert indicators[4]["m5"] == pytest.approx(1.07)
    flat = signal_history(parse_rows([raw_bar(day, 100) for day in days]))
    assert flat[2]["rsi"] == 50
    assert flat[198]["signal"] is None
    assert flat[199]["signal"] is False and flat[199]["m200"] == 1
    assert all(row["signal"] is False for row in flat[199:])
    distributions = parse_rows([raw_bar(days[0], 100), raw_bar(days[1], 49, 2, 2)])
    assert signal_history(distributions)[1]["index"] == 1


def test_signal_state_machine():
    assert next_signal(False, True, False, 5)
    assert not next_signal(False, False, False, 5)
    assert not next_signal(False, True, False, 5.01)
    assert next_signal(True, False, False, 90)
    assert not next_signal(True, True, True, 3)
    assert next_signal(False, True, True, 3)


def test_stable_mean_boundaries_and_constant_index():
    value = 1.1
    assert not above_mean(value, [value] * 200)
    assert not above_mean(value, [value] * 5)
    below = math.nextafter(value, -math.inf)
    above = math.nextafter(value, math.inf)
    assert above_mean(value, [below, value, value, value, value])
    assert not above_mean(value, [above, value, value, value, value])
    prices = [100.0] + [110.0] * 210
    rows = signal_history(
        parse_rows(
            [
                raw_bar(date(2000, 1, 1) + timedelta(days=i), close)
                for i, close in enumerate(prices)
            ]
        )
    )
    assert rows[-1]["index"] == 1.1
    assert rows[-1]["signal"] is False


def test_nonfinite_computed_indicator_fails():
    bars = parse_rows(
        [
            raw_bar(date(2000, 1, i + 1), close)
            for i, close in enumerate((1.0, 1e307, 1e307))
        ]
    )
    with pytest.raises(ValueError, match="non-finite signal indicator"):
        signal_history(bars)


def test_tax_profit_carry_expiry_and_dividend_only():
    ledger = TaxLedger(2020)
    ledger.add(2020, 100)
    assert ledger.settle(2020) == pytest.approx(20.315)
    ledger = TaxLedger(2020)
    ledger.add(2020, -100)
    assert ledger.settle(2020) == 0
    ledger.add(2021, 40)
    assert ledger.settle(2021) == 0
    ledger.add(2023, 30)
    assert ledger.settle(2023) == 0
    ledger.add(2024, 30)
    assert ledger.settle(2024) == pytest.approx(30 * 0.20315)
    dividends = TaxLedger(2020)
    dividends.add(2020, 10)
    assert dividends.settle(2020) == pytest.approx(2.0315)


def test_dev_future_cutoff_and_wealth_invariance(synthetic):
    original_days, original = compute(synthetic)
    altered = {
        ticker: [
            replace(bar, close=-1.0, split_factor=0.0, div_cash=-1.0)
            if bar.date > END
            else bar
            for bar in rows
        ]
        for ticker, rows in synthetic.items()
    }
    days, summary = compute(altered)
    assert (days, summary) == (original_days, original)
    assert original["n_cumulative"] == 4
    assert original["main"]["entry_count"] > 0
    boundary = date(2020, 5, 4)
    altered = {
        ticker: [
            replace(bar, close=bar.close * 2) if bar.date > boundary else bar
            for bar in rows
        ]
        for ticker, rows in synthetic.items()
    }
    changed, _ = compute(altered)
    assert [row for row in original_days if row["date"] <= boundary] == [
        row for row in changed if row["date"] <= boundary
    ]


def test_missing_sso_date_and_insufficient_warmup(synthetic):
    missing = synthetic | {
        "SSO": [bar for bar in synthetic["SSO"] if bar.date != date(2020, 5, 4)]
    }
    with pytest.raises(ValueError, match="missing"):
        compute(missing)
    short = synthetic | {
        "SPY": [bar for bar in synthetic["SPY"] if bar.date >= date(2007, 12, 1)]
    }
    with pytest.raises(ValueError, match="200"):
        compute(short)


@pytest.mark.parametrize(
    "close,dividend,split",
    [
        (0, 0, 1),
        (-1, 0, 1),
        (float("inf"), 0, 1),
        (float("nan"), 0, 1),
        (1, -1, 1),
        (1, 0, 0),
        (1, 0, -1),
    ],
)
def test_invalid_input(close, dividend, split):
    with pytest.raises(ValueError, match="invalid bar"):
        validate_bars(parse_rows([raw_bar(date(2020, 1, 1), close, dividend, split)]))


def test_holdout_requires_passing_dev_before_data(tmp_path):
    with pytest.raises(ValueError, match="dev verdict"):
        compute({}, "holdout")
    path = tmp_path / "dev.json"
    summary = {"stage": "dev", "gate_a": True, "gate_b": True, "gate_c": True}
    passing = {"match": True, "python_summary": summary}
    path.write_text(json.dumps(passing))
    require_dev_verdict(path)
    for invalid in (
        summary,
        {"match": True, **summary},
        {"match": False, "python_summary": summary},
        {"python_summary": summary},
        {"match": 1, "python_summary": summary},
    ):
        path.write_text(json.dumps(invalid))
        with pytest.raises(ValueError):
            require_dev_verdict(path)
    for key in ("gate_a", "gate_b", "gate_c"):
        for value in (False, 1, None):
            path.write_text(
                json.dumps({"match": True, "python_summary": summary | {key: value}})
            )
            with pytest.raises(ValueError, match="A, B, C"):
                require_dev_verdict(path)
    path.write_text(
        json.dumps({"match": True, "python_summary": summary | {"stage": "holdout"}})
    )
    with pytest.raises(ValueError, match="A, B, C"):
        require_dev_verdict(path)


def test_holdout_is_fresh_and_has_no_dev_gates(synthetic, tmp_path):
    path = tmp_path / "dev.json"
    path.write_text(
        json.dumps(
            {
                "match": True,
                "python_summary": {
                    "stage": "dev",
                    "gate_a": True,
                    "gate_b": True,
                    "gate_c": True,
                },
            }
        )
    )
    days, result = compute(synthetic, "holdout", path)
    assert days[0]["date"] == date(2024, 11, 1)
    assert days[0]["U"] < 1 and days[0]["U"] > 0.999
    assert result.keys() == {"stage", "n_cumulative", "holdout_n", "gate_h", "holdout"}
    assert result["holdout"]["tax_by_year"].keys() == {"2024", "2025", "2026"}


def test_split_plus_dividend_is_pre_split_units(synthetic, monkeypatch):
    import h004

    spy = synthetic["SPY"]
    end = date(2020, 8, 19)
    start = date(2020, 8, 17)
    sso = [
        replace(bar, close=100.0, split_factor=1.0, div_cash=0.0)
        for bar in synthetic["SSO"]
    ]
    sso = [
        replace(bar, close=50.0, split_factor=2.0, div_cash=2.0)
        if bar.date == date(2020, 8, 18)
        else replace(bar, close=50.0)
        if bar.date > date(2020, 8, 18)
        else bar
        for bar in sso
    ]
    original = h004.signal_history
    monkeypatch.setattr(
        h004,
        "signal_history",
        lambda rows: [row | {"signal": True} for row in original(rows)],
    )
    days, result = simulate(spy, sso, start, end)
    shares = 1 / 1.0005 / 100
    assert days[1]["V"] == pytest.approx((shares * 2 + shares * 2 / 50) * 50)
    assert result["entry_count"] == 1
    assert result["mean_holding_days"] == 2


def test_undefined_psr_and_relative_comparison():
    with pytest.raises(ValueError, match="variance"):
        psr([0.0, 0.0])
    assert matches(0.5, 0.5 + 2e-11, 1e-10)
    assert not matches(0.5, 0.5 + 6e-11, 1e-10)
    assert not matches(True, 1, 0)
    assert compare([], {"gate_a": True}, [], {"gate_a": 1}) == [
        "summary.gate_a: mismatch"
    ]


def test_recompute_rejects_stage_disagreement(synthetic):
    import shutil
    import tempfile
    from pathlib import Path

    import pyarrow as pa
    import pyarrow.parquet as pq

    import h004

    results = Path(__file__).resolve().parents[2] / "results"
    results.mkdir(exist_ok=True)
    directory = Path(tempfile.mkdtemp(prefix="verify-h004-python-", dir=results))
    try:
        days, summary = compute(synthetic)
        summary["input_sha256"] = {"SPY": "a" * 64, "SSO": "b" * 64}
        bars = [
            {
                "ticker": ticker,
                "date": bar.date,
                "close": bar.close,
                "div_cash": bar.div_cash,
                "split_factor": bar.split_factor,
            }
            for ticker, rows in synthetic.items()
            for bar in rows
            if bar.date <= END
        ]
        pq.write_table(pa.Table.from_pylist(bars), directory / "bars.parquet")
        pq.write_table(pa.Table.from_pylist(days), directory / "days.parquet")
        pq.write_table(
            pa.Table.from_pylist(
                signal_history([bar for bar in synthetic["SPY"] if bar.date <= END])
            ),
            directory / "indicators.parquet",
        )
        (directory / "summary.json").write_text(json.dumps(summary))
        assert h004.run(directory)[2] is True
        summary["stage"] = "holdout"
        (directory / "summary.json").write_text(json.dumps(summary))
        assert h004.run(directory)[2] is False
        verdict = json.loads((directory / "verdict.json").read_text())
        assert "summary.stage: mismatch" in verdict["mismatches"]
    finally:
        shutil.rmtree(directory)


def test_daily_returns_have_absolute_floor_assets_stay_relative():
    day = {"R": 0.0, "Q": 0.0, "d": 0.0, "V": 0.0001}
    assert compare([day], {}, [day | {"d": 1e-15}], {}) == []
    assert compare([day], {}, [day | {"R": 2e-10}], {}) == ["days[0].R: mismatch"]
    assert compare([day], {}, [day | {"V": 0.0001 + 1e-13}], {}) == [
        "days[0].V: mismatch"
    ]


def test_indicator_path_exits_before_considering_reentry():
    prices = [100.0] * 200 + [200.0] * 50 + [150.0] * 5 + [150.01, 150.011]
    bars = parse_rows(
        [
            raw_bar(date(2000, 1, 1) + timedelta(days=i), price)
            for i, price in enumerate(prices)
        ]
    )
    history = signal_history(bars)
    assert history[254]["signal"] is True
    for i in (255, 256):
        row = history[i]
        assert row["index"] > row["m5"] and row["index"] > row["m200"]
        assert row["rsi"] <= 5
    assert history[255]["signal"] is False
    assert history[256]["signal"] is True
