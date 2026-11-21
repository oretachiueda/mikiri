import pytest

from s002 import d_offsets


def test_d_offsets_reference_and_bounds():
    offsets = d_offsets(7258)
    assert offsets[:10] == [1501, 3107, 5893, 5182, 6974, 6493, 1900, 5680, 1960, 931]
    assert len(set(offsets)) == 1000
    assert all(252 <= offset <= 7258 - 252 for offset in offsets)
    with pytest.raises(ValueError, match="calendar too short"):
        d_offsets(1502)


def write_cot(path, rows):
    import csv
    import io
    import zipfile

    out = io.StringIO(newline="")
    writer = csv.writer(out)
    writer.writerow(
        [
            " Market and Exchange Names ",
            "As of Date in Form YYYY-MM-DD",
            "CFTC Contract Market Code",
            "Commercial Positions-Long (All)",
            "Commercial Positions-Short (All)",
        ]
        + [f"unused{i}" for i in range(124)]
    )
    for row in rows:
        writer.writerow(list(row) + [""] * 124)
    with zipfile.ZipFile(path, "w") as archive:
        archive.writestr("cot.txt", out.getvalue().encode("latin-1"))


def test_cot_format_ecu_cutoff_and_duplicate(tmp_path):
    from datetime import date

    from s002 import read_cot

    write_cot(
        tmp_path / "a.zip",
        [
            ("EUR", "1998-12-29", "099741", 1, 3),
            ("EURO, FX", "1999-01-05", "099741", 1, 3),
            ("JPY", "2024-11-05", "097741", "bad", "bad"),
        ],
    )
    assert read_cot(tmp_path)["EUR"] == [(date(1999, 1, 5), 0.5)]
    write_cot(tmp_path / "b.zip", [("EURO", "1999-01-05", "099741", 1, 3)])
    with pytest.raises(ValueError, match="duplicate COT"):
        read_cot(tmp_path)


def test_fred_blank_calendar_and_future_invariance(tmp_path):
    from datetime import date

    from s002 import FX, read_fred, return_calendar

    path = tmp_path / "DEXJPUS.csv"
    path.write_text(
        "observation_date,DEXJPUS\n2024-10-29,150\n2024-10-30,\n2024-10-31,151\n2024-11-01,invalid\n"
    )
    data = read_fred(path)
    assert data[date(2024, 10, 30)] is None
    fred = {name: {} for name in FX}
    fred["DEXJPUS"] = data
    assert return_calendar(fred) == [date(2024, 10, 29), date(2024, 10, 31)]
    path.write_text(path.read_text().replace("invalid", "999"))
    assert read_fred(path) == data


def test_spy_json_future_invariance(tmp_path):
    from datetime import date

    from s002 import read_spy

    path = tmp_path / "spy.json"
    path.write_text(
        '[{"date":"2024-10-31T00:00:00Z","adjClose":100},{"date":"2024-11-01T00:00:00Z","adjClose":"bad"}]'
    )
    assert read_spy(path) == {date(2024, 10, 31): 100.0}
    path.write_text(path.read_text().replace('"bad"', "500"))
    assert read_spy(path) == {date(2024, 10, 31): 100.0}


def test_slot_duplicate_and_z_windows():
    from datetime import date, timedelta

    from s002 import latest_slots, slot, z_values

    start = date(1992, 10, 6)
    reports = [(start + timedelta(days=7 * i), float(i % 7)) for i in range(156)]
    assert slot(start) == 0
    later = (start + timedelta(days=3), 10.0)
    assert latest_slots([later, reports[0]])[0] == later
    assert z_values(reports, 52)[reports[50][0]] is None
    assert z_values(reports, 52)[reports[51][0]] is not None
    assert z_values(reports, 156)[reports[155][0]] is not None
    missing = reports[:20] + reports[21:]
    assert z_values(missing, 156)[reports[155][0]] is None


def test_public_dates_and_signal_calendar():
    from datetime import date

    from s002 import public_and_signal

    calendar = [
        date.fromisoformat(s)
        for s in (
            "2013-11-29",
            "2013-12-02",
            "2019-03-29",
            "2019-04-01",
            "2024-01-08",
            "2024-01-09",
        )
    ]
    assert public_and_signal(date(2013, 10, 1), calendar) == (calendar[0], calendar[1])
    assert public_and_signal(date(2019, 3, 12), calendar) == (calendar[2], calendar[3])
    assert public_and_signal(date(2024, 1, 1), calendar) == (calendar[4], calendar[5])


def test_readers_preserve_earlier_values_when_future_changes(tmp_path):
    from datetime import date

    from s002 import read_cot, read_fred, read_spy

    cot = tmp_path / "a.zip"
    write_cot(
        cot,
        [("JPY", "2024-01-02", "097741", 1, 3), ("JPY", "2024-01-09", "097741", 2, 3)],
    )
    fred = tmp_path / "DEXJPUS.csv"
    fred.write_text("observation_date,DEXJPUS\n2024-01-02,150\n2024-01-09,151\n")
    spy = tmp_path / "spy.json"
    spy.write_text(
        '[{"date":"2024-01-02T00:00:00Z","adjClose":100},{"date":"2024-01-09T00:00:00Z","adjClose":101}]'
    )
    day = date(2024, 1, 2)
    before = (read_cot(tmp_path)["JPY"][0], read_fred(fred)[day], read_spy(spy)[day])
    write_cot(
        cot,
        [
            ("JPY", "2024-01-02", "097741", 1, 3),
            ("JPY", "2024-01-09", "097741", 100, 1),
        ],
    )
    fred.write_text(fred.read_text().replace(",151", ",999"))
    spy.write_text(spy.read_text().replace(":101", ":999"))
    assert (
        read_cot(tmp_path)["JPY"][0],
        read_fred(fred)[day],
        read_spy(spy)[day],
    ) == before


def test_direction_bands_and_quote_convention():
    from s002 import direction

    assert direction(0, "lvl", "b0", "JPY") == 0
    assert direction(0.1, "lvl", "b1", "EUR") == 0
    assert direction(-1.0, "z52", "b1", "EUR") == 0
    for leg in ("JPY", "CHF", "CAD", "MXN"):
        assert direction(1, "lvl", "b0", leg) == -1
    for leg in ("EUR", "GBP", "AUD", "NZD"):
        assert direction(1, "lvl", "b0", leg) == 1


def test_sizing_strict_history_joint_days_and_cap():
    import math
    from datetime import date, timedelta

    from s002 import fix_returns, size_positions

    start = date(2020, 1, 1)
    prices = {
        leg: {
            start + timedelta(days=i): 100 * math.exp(0.0001 * (i % 3))
            for i in range(63)
        }
        for leg in ("JPY", "EUR")
    }
    day = start + timedelta(days=61)
    assert len(fix_returns(prices["JPY"], day)) == 60
    assert size_positions(day, {"JPY": 1}, prices, "jpy")["JPY"] == 10
    before = size_positions(day, {"JPY": 1, "EUR": 1}, prices, "fx8")
    assert sum(before.values()) == pytest.approx(10)
    prices["JPY"][day] = 1e8
    assert size_positions(day, {"JPY": 1, "EUR": 1}, prices, "fx8") == before
    prices["EUR"].pop(start + timedelta(days=20))
    assert size_positions(day, {"JPY": 1, "EUR": 1}, prices, "fx8") == {
        "JPY": 10,
        "EUR": 0,
    }
    with pytest.raises(ValueError, match="volatility"):
        size_positions(
            day, {"JPY": 1}, {"JPY": dict.fromkeys(prices["JPY"], 100.0)}, "jpy"
        )


def test_signal_batch_stale_fill_and_h2():
    import math
    from datetime import date, timedelta

    from s002 import signals

    start = date(2013, 8, 1)
    calendar = [start + timedelta(days=i) for i in range(180)]
    prices = {
        "JPY": {d: 100 * math.exp(0.001 * (i % 3)) for i, d in enumerate(calendar)}
    }
    reports = {"JPY": [(date(2013, 10, 1), 0.5), (date(2013, 10, 8), -0.5)]}
    rates = {"JPY": {date(2013, 1, 1): 0.01}, "USD": {date(2013, 1, 1): 0.02}}
    result = signals("lvl-b0-h1-jpy", reports, calendar, prices, rates, start)
    shutdown = [row for row in result if row[0] == date(2013, 11, 30)]
    assert len(shutdown) == 1
    assert shutdown[0][1]["JPY"] == date(2013, 12, 1)
    assert shutdown[0][2]["JPY"] == 1
    assert result[-1][0] == date(2013, 12, 13)
    assert result[-1][2]["JPY"] == 0
    h2 = signals("lvl-b0-h2-jpy", reports, calendar, prices, rates, start)
    assert len([row for row in h2 if row[0] == date(2013, 11, 30)]) == 1
    assert h2[0][2]["JPY"] == 1
    rates["USD"] = {}
    assert (
        signals("lvl-b0-h1-jpy", reports, calendar, prices, rates, start)[0][2]["JPY"]
        == 0
    )


def test_fx8_incomplete_joint_window_and_zero_portfolio():
    import math
    from datetime import date, timedelta

    from s002 import size_positions

    start = date(2020, 1, 1)
    day = start + timedelta(days=130)
    prices = {
        "JPY": {
            start + timedelta(days=i): 100 * math.exp(0.01 * (i % 3))
            for i in range(0, 122, 2)
        },
        "EUR": {
            start + timedelta(days=i): 100 * math.exp(0.01 * (i % 3))
            for i in range(1, 123, 2)
        },
    }
    assert size_positions(day, {"JPY": 1, "EUR": 1}, prices, "fx8") == {
        "JPY": 0,
        "EUR": 0,
    }
    prices["EUR"] = prices["JPY"].copy()
    with pytest.raises(ValueError, match="volatility"):
        size_positions(day, {"JPY": 1, "EUR": -1}, prices, "fx8")


def test_public_non_tuesday_waits_seven_days():
    from datetime import date

    from s002 import public_and_signal

    calendar = [date(2024, 1, 8), date(2024, 1, 9), date(2024, 1, 10)]
    assert public_and_signal(date(2024, 1, 1), calendar) == (calendar[0], calendar[1])
    assert public_and_signal(date(2024, 1, 2), calendar) == (calendar[0], calendar[1])
    assert public_and_signal(date(2024, 1, 3), calendar) == (calendar[2], None)


def test_one_trade_spot_carry_markup_spread_and_exit():
    from datetime import date

    from s002 import simulate_returns

    days = [date(2024, 1, 30), date(2024, 1, 31), date(2024, 2, 2), date(2024, 2, 5)]
    prices = {"EUR": {days[0]: 1.0, days[1]: 1.0, days[2]: 1.1, days[3]: 1.2}}
    rates = {
        "EUR": {date(2023, 11, 1): 5.0, date(2023, 12, 1): 6.0},
        "USD": {date(2023, 11, 1): 2.0, date(2023, 12, 1): None},
    }
    actions = [
        (days[0], {"EUR": days[1]}, {"EUR": 1}, {"EUR": 2.0}),
        (days[2], {"EUR": days[3]}, {"EUR": 0}, {"EUR": 0.0}),
    ]
    result = simulate_returns(days, actions, prices, rates, days[0])
    entry_cost = 0.00004 / 2 * 2
    assert result[1][1] == pytest.approx(-entry_cost)
    carry = 2 * 2 * (0.06 - 0.02 - 0.0135) / 365
    pnl = 2 * 0.1 + carry
    assert result[2][1] == pytest.approx(pnl / (1 - entry_cost))
    exit_carry = 2 * 3 * (0.06 - 0.02 - 0.0135) / 365
    exit_pnl = 2 * (1.2 / 1.1 - 1) + exit_carry - (0.00004 / 2 / 1.2 * 2)
    assert result[3][1] == pytest.approx(exit_pnl / result[2][2])


def test_rate_rollover_short_markup_and_mxn_spread():
    from datetime import date

    from s002 import rate_for_day, simulate_returns

    days = [date(2024, 1, 29), date(2024, 1, 30), date(2024, 2, 2)]
    rates = {
        "USD": {date(2023, 11, 1): 4, date(2023, 12, 1): 5},
        "MXN": {date(2023, 11, 1): 8, date(2023, 12, 1): None},
    }
    assert rate_for_day(rates["MXN"], days[-1]) == 0.08
    prices = {"MXN": {days[0]: 20.0, days[1]: 20.0, days[2]: 20.0}}
    actions = [(days[0], {"MXN": days[1]}, {"MXN": -1}, {"MXN": 1.0})]
    result = simulate_returns(days, actions, prices, rates, days[0])
    assert result[1][1] == pytest.approx(-0.003 / 2 / 20)
    expected = ((0.08 - 0.04 - 0.02) + 2 * (0.08 - 0.05 - 0.02)) / 365
    assert result[2][1] == pytest.approx(expected / result[1][2])


def test_notional_uses_previous_equity_and_bust():
    from datetime import date, timedelta

    from s002 import simulate_returns

    days = [date(2024, 1, 1) + timedelta(days=i) for i in range(5)]
    prices = {"JPY": dict(zip(days, [100.0, 100.0, 110.0, 110.0, 120.0], strict=True))}
    rates = {"JPY": {date(2023, 1, 1): 0.0}, "USD": {date(2023, 1, 1): 1.35}}
    actions = [
        (days[0], {"JPY": days[1]}, {"JPY": 1}, {"JPY": 1.0}),
        (days[2], {"JPY": days[3]}, {"JPY": 1}, {"JPY": 2.0}),
    ]
    result = simulate_returns(days, actions, prices, rates, days[0])
    expected_notional = 2 * result[2][2]
    assert result[4][1] == pytest.approx(
        expected_notional * (120 / 110 - 1) / result[3][2]
    )
    prices["JPY"][days[2]] = 1.0
    actions[0][3]["JPY"] = 10.0
    with pytest.raises(ValueError, match="bust"):
        simulate_returns(days, actions, prices, rates, days[0])


def test_spreads_and_flat_or_missing_fix_days():
    from datetime import date, timedelta

    from s002 import SPREAD, simulate_returns

    assert SPREAD == {
        "JPY": 0.002,
        "EUR": 0.00004,
        "GBP": 0.0003,
        "CHF": 0.0003,
        "CAD": 0.0003,
        "AUD": 0.00006,
        "NZD": 0.0003,
        "MXN": 0.003,
    }
    start = date(2024, 1, 1)
    days = [start + timedelta(days=i) for i in range(4)]
    prices = {"AUD": {days[0]: 1.0, days[1]: 1.0, days[3]: 1.0}}
    rates = {"AUD": {date(2023, 1, 1): 0}, "USD": {date(2023, 1, 1): 0}}
    action = [(days[0], {"AUD": days[1]}, {"AUD": 1}, {"AUD": 1.0})]
    result = simulate_returns(days, action, prices, rates, start)
    assert result[0][1] == 0
    assert result[1][1] == pytest.approx(-0.00006 / 2)
    assert result[2][1] == 0
    assert result[3][1] == pytest.approx(-2 * 0.0135 / 365 / result[2][2])
