import math
from dataclasses import replace
from datetime import date

import pytest

from h003 import (
    Bar,
    TaxLedger,
    compare,
    matches,
    signal_history,
    simulate,
    validate_bars,
)


def raw_bar(
    day: date, close: float, div_cash: float = 0, split_factor: float = 1
) -> dict:
    return {
        "date": f"{day.isoformat()}T00:00:00.000Z",
        "close": float(close),
        "high": float(close + 1),
        "low": float(close - 1),
        "open": float(close),
        "adjClose": float(close),
        "adjHigh": float(close + 1),
        "adjLow": float(close - 1),
        "adjOpen": float(close),
        "divCash": float(div_cash),
        "splitFactor": float(split_factor),
        "volume": 1000,
        "adjVolume": 1000,
    }


def bars_from_tiingo(records: list[dict]) -> list[Bar]:
    return [
        Bar(
            date.fromisoformat(row["date"][:10]),
            row["close"],
            row["divCash"],
            row["splitFactor"],
        )
        for row in records
    ]


def small_bars() -> tuple[list[Bar], list[Bar], list[Bar]]:
    dates = [
        date(2022, 12, 27),
        date(2022, 12, 28),
        date(2022, 12, 29),
        date(2022, 12, 30),
        date(2023, 1, 3),
        date(2023, 1, 4),
    ]
    spy = bars_from_tiingo(
        [
            raw_bar(day, close)
            for day, close in zip(dates, (100, 90, 90, 120, 120, 121), strict=True)
        ]
    )
    spxl = bars_from_tiingo([raw_bar(day, 50) for day in dates])
    bil_rows = [raw_bar(day, 100) for day in dates]
    bil_rows[3]["divCash"] = 10.0
    bil_rows[4]["divCash"] = 1.0
    bil_rows[4]["splitFactor"] = 0.5
    bil = bars_from_tiingo(bil_rows)
    return spy, spxl, bil


def test_tax_gain_loss_carry_dividend_and_expiry():
    ledger = TaxLedger(2020)
    ledger.add(2020, 100)
    assert ledger.settle(2020) == pytest.approx(20.315)

    ledger = TaxLedger(2020)
    ledger.add(2020, -100)
    assert ledger.settle(2020) == 0
    ledger.add(2021, 40)
    assert ledger.settle(2021) == 0
    ledger.add(2022, 60)
    assert ledger.settle(2022) == 0

    ledger = TaxLedger(2020)
    ledger.add(2020, -100)
    assert ledger.settle(2020) == 0
    assert ledger.settle(2021) == 0
    assert ledger.settle(2022) == 0
    assert ledger.settle(2023) == 0
    ledger.add(2024, 100)
    assert ledger.settle(2024) == pytest.approx(20.315)

    ledger = TaxLedger(2024)
    ledger.add(2024, 500)
    assert ledger.settle(2024) == pytest.approx(101.575)


def test_split_dividend_switch_and_prior_tax_sale_order(monkeypatch):
    import h003

    spy, spxl, bil = small_bars()
    monkeypatch.setattr(h003, "SIGNAL_LOOKBACK", 2)
    days, result = simulate(spy, spxl, bil, date(2022, 12, 29), date(2023, 1, 4))
    jan3 = days[2]
    assert days[0]["holding"] == "BIL"
    assert jan3["signal"] is True
    assert jan3["holding"] == "SPXL"
    assert result["switch_count"] == 1
    initial_shares = 1 / 1.0002 / 100
    due = initial_shares * 10 * 0.20315
    assert result["tax_by_year"]["2022"] == pytest.approx(due)
    assert jan3["V"] > jan3["U"]
    bil_shares = (initial_shares + initial_shares * 10 / 100) * 0.5
    bil_shares += (initial_shares + initial_shares * 10 / 100) / 100
    switch_proceeds = bil_shares * 100 * (1 - 0.0002)
    spxl_shares = switch_proceeds / 1.0005 / 50
    expected_jan3_value = (spxl_shares - due / (1 - 0.0005) / 50) * 50 + due
    assert jan3["V"] == pytest.approx(expected_jan3_value, rel=1e-12)
    assert days[-1]["holding"] == "CASH"
    assert all(math.isfinite(row["A"]) for row in days)


def test_future_bars_do_not_change_prior_signal_or_wealth(monkeypatch):
    import h003

    spy, spxl, bil = small_bars()
    monkeypatch.setattr(h003, "SIGNAL_LOOKBACK", 2)
    original, _ = simulate(spy, spxl, bil, date(2022, 12, 29), date(2023, 1, 4))
    changed = [
        replace(bar, close=bar.close * 7) if bar.date == date(2023, 1, 4) else bar
        for bar in spy
    ]
    replay, _ = simulate(changed, spxl, bil, date(2022, 12, 29), date(2023, 1, 4))
    assert [row for row in original[:-1]] == [row for row in replay[:-1]]
    _, signals_before = signal_history(spy)
    _, signals_after = signal_history(changed)
    assert signals_before[:-1] == signals_after[:-1]


def test_comparison_uses_relative_tolerance_and_exact_fields():
    assert matches(0.5, 0.5 + 2e-11, 1e-10)
    assert not matches(0.5, 0.5 + 6e-11, 1e-10)
    assert not matches(True, 1, 1e-10)

    day = {
        "date": date(2022, 1, 3),
        "signal": True,
        "holding": "SPXL",
        "V": 0.5,
        "U": 0.5,
        "R": 0.0,
        "Q": 0.0,
        "d": 0.0,
        "A": 0.5,
    }
    summary = {"gate_a": True, "value": 0.5}
    assert compare([day], summary, [day | {"V": 0.5 + 2e-11}], summary) == []

    mismatches = compare(
        [day],
        summary,
        [day | {"signal": 1, "extra": 2}],
        {"gate_a": 1, "value": 0.5, "extra": 2},
    )
    assert "days[0]: fields differ" in mismatches
    assert "days[0].signal: mismatch" in mismatches
    assert "summary.gate_a: mismatch" in mismatches
    assert "summary: fields differ" in mismatches


@pytest.mark.parametrize(
    "bad",
    [
        Bar(date(2022, 1, 1), 0, 0, 1),
        Bar(date(2022, 1, 1), float("nan"), 0, 1),
        Bar(date(2022, 1, 1), 1, -1, 1),
        Bar(date(2022, 1, 1), 1, 0, 0),
    ],
)
def test_invalid_bar_values_fail_closed(bad):
    with pytest.raises(ValueError, match="invalid bar value"):
        validate_bars([bad])
