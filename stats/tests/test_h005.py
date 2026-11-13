import copy
import json
import math
from datetime import date, timedelta

import pyarrow as pa
import pyarrow.parquet as pq
import pytest

import h005 as h


def number(value):
    assert h.finite(value)
    return float(value)


def test_rate_vintage_signals_use_each_currency_preparation_history(rows):
    augmented = copy.deepcopy(rows)
    series_ids = dict(zip((*h.AREAS, "USA"), (*h.FIRST_RATES, h.US_FIRST), strict=True))
    for row in rows:
        if row["kind"] != "rate":
            continue
        area = row["series"]
        start = "2013-04-01" if area in {"USA", "CAN"} else "2013-05-01"
        if row["date"] >= start:
            value = (
                None if area == "USA" and row["date"] == "2020-04-01" else row["value"]
            )
            augmented.append(
                {
                    **row,
                    "kind": "first_rate",
                    "series": series_ids[area],
                    "value": value,
                }
            )
    augmented.sort(key=lambda row: (row["kind"], row["series"], row["date"]))
    prepared, days, summary = h.compute(augmented)
    for signal in summary["vintage"]["rate_signals"]:
        expected = "2014-05-30" if signal["currency"] == "CAD" else "2014-06-30"
        assert signal["available_start"] == expected
        values = signal["values"]
        first = next(i for i, row in enumerate(values) if row["date"] == expected)
        assert values[first - 1]["first_signal"] is None
        assert values[first]["first_signal"] == values[first]["current_signal"]
        assert signal["comparable_months"] == len(values) - first
    truncated = [
        row
        for row in augmented
        if row["kind"] != "first_rate" or row["date"] >= "2013-06-01"
    ]
    old_prepared, old_days, old_summary = h.compute(truncated)
    del summary["vintage"]["rate_signals"]
    del old_summary["vintage"]["rate_signals"]
    assert json.dumps(summary) == json.dumps(old_summary)
    assert json.dumps(days) == json.dumps(old_days)
    assert json.dumps(prepared) == json.dumps(old_prepared)


def test_zero_month_end_volatility_before_first_target_stops(rows):
    altered = copy.deepcopy(rows)
    for row in altered:
        if row["kind"] == "rate":
            row["value"] = 2.0
        elif (
            row["kind"] == "fx"
            and row["series"] == "DEXUSEU"
            and row["date"] <= "2009-05-31"
        ):
            row["value"] = 1.0
    with pytest.raises(ValueError, match="^volatility must be positive and finite$"):
        h.compute(altered)


def observation(
    kind, series, observed, value=None, close=None, dividend=None, split=None
):
    return {
        "kind": kind,
        "series": series,
        "date": observed.isoformat(),
        "value": value,
        "close": close,
        "div_cash": dividend,
        "split_factor": split,
    }


def synthetic(end=date(2026, 10, 30), vintage=False):
    rows = []
    observed = date(2008, 1, 1)
    count = 0
    while observed <= end:
        if observed.weekday() < 5:
            for j, series in enumerate(h.DEX):
                value = (
                    (1 + 0.00003 * count)
                    * (1 + 0.012 * math.sin(count / 13 + j))
                    * (j + 1)
                )
                rows.append(observation("fx", series, observed, value))
                if vintage:
                    rows.append(
                        observation(
                            "first_fx",
                            series,
                            observed,
                            value * (1 + 0.00001 * math.cos(count)),
                        )
                    )
            rows.append(
                observation(
                    "spy",
                    "SPY",
                    observed,
                    close=100 + count * 0.002,
                    dividend=0.01 if count % 60 == 0 else 0.0,
                    split=1.0,
                )
            )
            count += 1
        observed += timedelta(days=1)
    observed = date(2007, 12, 1)
    while observed < end.replace(day=1):
        for j, area in enumerate((*h.AREAS, "USA")):
            if area == "USA" and observed == date(2020, 4, 1):
                continue
            value = 1.0 + j * 0.1 + 0.1 * math.sin(observed.year * 12 + observed.month)
            rows.append(observation("rate", area, observed, value))
            if vintage and observed >= date(2013, 6, 1):
                rows.append(
                    observation(
                        "first_rate",
                        (*h.FIRST_RATES, h.US_FIRST)[j],
                        observed,
                        value + 0.01,
                    )
                )
        observed = date(
            observed.year + (observed.month == 12), observed.month % 12 + 1, 1
        )
    rows = [
        row
        for row in rows
        if not (row["kind"] == "spy" and row["date"] == "2010-01-01")
    ]
    return sorted(rows, key=lambda row: (row["kind"], row["series"], row["date"]))


@pytest.fixture(scope="module")
def rows():
    return synthetic()


@pytest.fixture(scope="module")
def dev(rows):
    return h.compute(rows)


def test_units_interest_month_boundary_and_short_sign(rows):
    history = h.History(h.validate_observations(rows, "dev"))
    observed = date(2010, 2, 1)
    assert history.spot("DEXJPUS", observed) == 1 / number(
        history.maps[("fx", "DEXJPUS")][observed]
    )
    assert (
        history.spot("DEXUSEU", observed) == history.maps[("fx", "DEXUSEU")][observed]
    )
    assert (
        history.rate("EA20", observed)
        == number(history.maps[("rate", "EA20")][date(2010, 1, 1)]) / 100
    )
    expected = sum(
        (
            number(history.rate("EA20", date(2010, 1, 29) + timedelta(days=i)))
            - number(history.rate("USA", date(2010, 1, 29) + timedelta(days=i)))
        )
        / 365
        for i in range(3)
    )
    assert history.interest("EA20", date(2010, 1, 29), observed) == expected
    assert -2 * 3 * expected == -(2 * 3 * expected)


def test_fx_sixth_business_day_stops(rows):
    history = h.History(h.validate_observations(rows, "dev"))
    source = history.maps[("fx", "DEXUSEU")]
    first = date(2010, 3, 1)
    for offset in range(8):
        source[first + timedelta(days=offset)] = None
    assert history.spot("DEXUSEU", date(2010, 3, 5)) is not None
    with pytest.raises(ValueError, match="five business"):
        history.spot("DEXUSEU", date(2010, 3, 8))


def test_rate_first_five_applying_days_and_us_exception(rows):
    history = h.History(h.validate_observations(rows, "holdout"))
    del history.maps[("rate", "GBR")][date(2021, 2, 1)]
    assert (
        history.rate("GBR", date(2021, 3, 5))
        == number(history.maps[("rate", "GBR")][date(2021, 1, 1)]) / 100
    )
    assert history.rate("GBR", date(2021, 3, 7)) is not None
    with pytest.raises(ValueError, match="rate unavailable"):
        history.rate("GBR", date(2021, 3, 8))
    assert (
        history.rate("USA", date(2020, 5, 31))
        == number(history.maps[("rate", "USA")][date(2020, 3, 1)]) / 100
    )
    del history.maps[("rate", "GBR")][date(2026, 3, 1)]
    with pytest.raises(ValueError, match="rate unavailable"):
        history.rate("GBR", date(2026, 4, 8))


def test_tax_profit_loss_fifo_expiry_and_partial_year():
    tax = h.TaxLedger()
    tax.add(2010, -4)
    assert tax.settle(2010) == 0
    tax.add(2011, -3)
    assert tax.settle(2011) == 0
    tax.add(2012, 5)
    assert tax.settle(2012) == 0
    assert tax.losses == [[2010, 0.0], [2011, 2]]
    tax.add(2015, 5)
    assert tax.settle(2015) == pytest.approx(5 * 0.20315)
    tax.add(2016, 3)
    assert tax.settle(2016) == pytest.approx(3 * 0.20315)


def test_ewma_normalized_finite_history_and_twelve_month(dev):
    indicators, _, _ = dev
    currency = [row for row in indicators if row["currency"] == "EUR"]
    index = 300
    past = [row["x"] for row in currency[1 : index + 1]]
    weights = [(1 - 60 / 61) * (60 / 61) ** i for i in range(len(past))]
    normalizer = sum(weights)
    mean = sum(x * w for x, w in zip(reversed(past), weights, strict=True)) / normalizer
    variance = (
        sum(w * (x - mean) ** 2 for x, w in zip(reversed(past), weights, strict=True))
        / normalizer
    )
    assert currency[index]["mean"] == pytest.approx(mean, abs=1e-14)
    assert currency[index]["sigma"] == pytest.approx(
        math.sqrt(261 * variance), abs=1e-12
    )
    current_index = next(
        i for i, row in enumerate(currency) if row["date"] == "2009-12-31"
    )
    begin = next(i for i, row in enumerate(currency) if row["date"] == "2008-12-31")
    product = (
        math.prod(1 + row["x"] for row in currency[begin + 1 : current_index + 1]) - 1
    )
    assert currency[current_index]["X"] == product


def test_execution_fixed_quantity_costs_tax_and_reports(dev):
    prepared, days, summary = dev
    main = [
        row for row in days if row["period"] == "main" and row["scenario"] == "verdict"
    ]
    assert main[0]["date"] == "2010-01-04"
    assert main[0]["P_EUR"] == main[0]["financing_EUR"] == 0
    assert main[0]["R"] is None
    opening_spots = {
        row["currency"]: row["S"] for row in prepared if row["date"] == main[0]["date"]
    }
    assert main[0]["gross_before"] == 0
    assert main[0]["gross_after"] == pytest.approx(
        sum(
            abs(main[0][f"q_after_{currency}"]) * opening_spots[currency]
            for currency in h.CURRENCIES
        )
    )
    assert main[0]["gross_after"] != pytest.approx(
        main[0]["gross_after"] / main[0]["E"]
    )
    assert main[1]["P_EUR"] != 0
    assert main[0]["q_after_EUR"] == main[1]["q_before_EUR"] == main[1]["q_after_EUR"]
    assert main[1]["financing_EUR"] == pytest.approx(0.0025 * main[1]["G_EUR"] / 365)
    assert main[-1]["q_after_EUR"] == main[-1]["N_EUR"] == 0
    assert main[-1]["transaction_EUR"] > 0
    assert main[-1]["tax"] >= 0
    assert any(row["tax"] > 0 and row["date"].endswith("12-31") for row in main)
    for period in ("main", "recent"):
        table = summary["report_tables"][period]
        assert len(table["rows"]) == 8
        assert table["rows"][0] == summary[period]
        for report in table["rows"]:
            assert report["reconciliation"] is True
            assert report["contribution_total"] == sum(report["contribution"].values())
            assert (
                report["benchmark"]["cagr_pretax"]
                == report["benchmark"]["cagr_aftertax"]
            )
        zero_days = [
            row for row in days if row["period"] == period and row["scenario"] == "zero"
        ]
        assert all(
            row[f"transaction_{currency}"] == row[f"financing_{currency}"] == 0
            for row in zero_days
            for currency in h.CURRENCIES
        )


def test_next_month_target_uses_prior_equity_and_flip_both_legs(rows):
    history = h.History(h.validate_observations(rows, "dev"))
    prepared = h.indicators(history)
    last_jan = history.month_ends[(2010, 1)]
    for j in range(9):
        prepared[last_jan * 9 + j]["signal"] = -prepared[last_jan * 9 + j]["signal"]
    days, _ = h.simulate(
        history, prepared, "sample", date(2010, 1, 4), date(2010, 2, 3), h.SCENARIOS[0]
    )
    by_date = {row["date"]: row for row in days}
    execution = by_date["2010-02-01"]
    signal_day = by_date[history.dates[last_jan].isoformat()]
    for j, currency in enumerate(h.CURRENCIES):
        assert execution[f"N_{currency}"] == pytest.approx(
            prepared[last_jan * 9 + j]["signal"]
            * signal_day["E"]
            * 0.4
            / prepared[last_jan * 9 + j]["sigma"]
            / 9
        )
        spot = prepared[(last_jan + 1) * 9 + j]["S"]
        expected = (
            0.0001
            * abs(execution[f"q_after_{currency}"] - execution[f"q_before_{currency}"])
            * spot
        )
        assert execution[f"transaction_{currency}"] == pytest.approx(expected)


def test_future_rows_never_affect_dev_or_numeric_validation(rows, dev):
    changed = copy.deepcopy(rows)
    for row in changed:
        if (
            row["date"] > "2024-10-31"
            or row["kind"] in {"rate", "first_rate"}
            and row["date"] >= "2024-10-01"
        ):
            for key in ("value", "close", "div_cash", "split_factor"):
                row[key] = float("nan")
    assert h.compute(changed) == dev


@pytest.mark.parametrize(
    "field,value", [("value", 0.0), ("value", -1.0), ("value", float("nan"))]
)
def test_bad_spot_value_rejected(rows, field, value):
    changed = copy.deepcopy(rows)
    row = next(row for row in changed if row["kind"] == "fx")
    row[field] = value
    with pytest.raises(ValueError, match="observation value"):
        h.validate_observations(changed, "dev")


@pytest.mark.parametrize(
    "field,value",
    [("close", 0), ("split_factor", 0), ("div_cash", -1), ("close", float("inf"))],
)
def test_bad_spy_value_rejected(rows, field, value):
    changed = copy.deepcopy(rows)
    next(row for row in changed if row["kind"] == "spy")[field] = value
    with pytest.raises(ValueError, match="SPY"):
        h.validate_observations(changed, "dev")


def test_duplicate_reverse_missing_fields_and_short_preparation(rows):
    with pytest.raises(ValueError, match="sorted and unique"):
        h.validate_observations([rows[0], rows[0], *rows[1:]], "dev")
    with pytest.raises(ValueError, match="sorted and unique"):
        h.validate_observations(list(reversed(rows)), "dev")
    changed = copy.deepcopy(rows)
    del changed[0]["date"]
    with pytest.raises(ValueError, match="fields"):
        h.validate_observations(changed, "dev")
    with pytest.raises(ValueError, match="January 2008"):
        h.compute([row for row in rows if row["date"] >= "2009-01-01"])
    with pytest.raises(ValueError, match="PSR variance"):
        h.psr([0.0, 0.0])


def test_vintages_missing_and_independent_first_rates(rows, dev):
    vintage = dev[2]["vintage"]
    assert all(row["comparable_months"] == 0 for row in vintage["spot"])
    assert all(row["comparable_months"] == 0 for row in vintage["rates"])
    sample = synthetic(vintage=True)
    history = h.History(h.validate_observations(sample, "dev"))
    prepared = h.indicators(history)
    report = h.vintages(history, prepared, "dev")
    assert report["spot"][0]["comparable_months"] > 0
    assert report["rate_signals"][0]["values"][0]["first_signal"] is None
    assert report["rate_signals"][0]["comparable_months"] > 0
    usa = next(row for row in report["rates"] if row["area"] == "USA")
    exception = next(row for row in usa["values"] if row["date"] == "2020-04-01")
    assert exception == {
        "date": "2020-04-01",
        "current": None,
        "first": None,
        "difference": None,
    }
    assert history.rate("USA", date(2020, 5, 20), vintage=True) == history.rate(
        "USA", date(2020, 5, 20)
    )


def gate_fixture(tmp_path):
    directory = tmp_path / "dev"
    directory.mkdir()
    summary = {"stage": "dev", "gate_a": True, "gate_b": True, "gate_c": True}
    for name in h.RESULT_FILES:
        (directory / name).write_bytes(
            json.dumps(summary).encode() if name == "summary.json" else b"synthetic"
        )
    (directory / "SHA256SUMS").write_text(
        "".join(f"{h.digest(directory / name)}  {name}\n" for name in h.RESULT_FILES)
    )
    result = h.digest(directory / "SHA256SUMS")
    envelope = {
        "match": True,
        "results_sha256": result,
        "python_summary": summary,
        "rust_summary": summary,
    }
    match = directory / "verdict.json"
    match.write_text(json.dumps(envelope))
    fx = tmp_path / "fx"
    fx.mkdir()
    for series in h.DEX:
        (fx / f"{series}.csv").write_text(f"observation_date,{series}\n")
    rates, spy = tmp_path / "OECD.csv", tmp_path / "SPY.json"
    rates.write_text("synthetic rates")
    spy.write_text("[]")
    records = [
        {
            "hypothesis": "H005",
            "kind": "run",
            "stage": "dev",
            "results_sha256": result,
            "data_sha256": h.raw_hashes(fx, rates, spy),
        },
        {
            "hypothesis": "H005",
            "kind": "verdict",
            "stage": "dev",
            "result": "pass",
            "gates": {"A": True, "B": True, "C": True},
            "results_sha256": result,
        },
        {"hypothesis": "H005", "kind": "holdout_open", "stage": "holdout"},
    ]
    ledger = tmp_path / "ledger.jsonl"
    ledger.write_text("".join(json.dumps(row) + "\n" for row in records))
    return match, ledger, fx, rates, spy


def test_holdout_authorization_complete(tmp_path):
    h.require_holdout(*gate_fixture(tmp_path))


@pytest.mark.parametrize(
    "failure",
    [
        "envelope",
        "match",
        "verdict",
        "gate",
        "hash",
        "open",
        "consumed",
        "manifest",
        "summary",
        "result_digest",
        "run",
    ],
)
def test_holdout_authorization_refusals(tmp_path, failure):
    match, ledger, fx, rates, spy = gate_fixture(tmp_path)
    envelope = json.loads(match.read_text())
    records = [json.loads(line) for line in ledger.read_text().splitlines()]
    if failure == "envelope":
        match = None
    elif failure == "match":
        envelope["match"] = False
    elif failure in {"verdict", "open", "run"}:
        kind = {"verdict": "verdict", "open": "holdout_open", "run": "run"}[failure]
        records = [row for row in records if row["kind"] != kind]
    elif failure == "gate":
        records[1]["gates"]["C"] = False
    elif failure == "hash":
        rates.write_text("modified")
    elif failure == "consumed":
        records.append({"hypothesis": "H005", "kind": "run", "stage": "holdout"})
    elif failure == "manifest":
        (match.parent / "observations.parquet").write_bytes(b"modified")
    elif failure == "summary":
        envelope["python_summary"]["extra"] = 3
    elif failure == "result_digest":
        envelope["results_sha256"] = "0" * 64
    if match is not None:
        match.write_text(json.dumps(envelope))
    ledger.write_text("".join(json.dumps(row) + "\n" for row in records))
    with pytest.raises((ValueError, OSError)):
        h.require_holdout(match, ledger, fx, rates, spy)


def test_numeric_comparison_boundary_and_signal_exact():
    assert not h.compare_tree({"x": 0.1}, {"x": 0.1 + 9e-11})
    assert h.compare_tree({"x": 0.1}, {"x": 0.1 + 2e-10})
    assert not h.compare_tree({"psr_main": 0.2}, {"psr_main": 0.2 + 9e-10})
    assert h.compare_tree({"signal": 0}, {"signal": 1e-12})
    assert h.compare_tree({"x": None}, {"x": float("nan")})


def test_observation_parquet_schema(tmp_path, rows):
    schema = pa.schema(
        [
            (key, pa.string() if key in {"kind", "series", "date"} else pa.float64())
            for key in rows[0]
        ]
    )
    path = tmp_path / "observations.parquet"
    pq.write_table(pa.Table.from_pylist(rows, schema=schema), path)
    assert h.read_observations(path, "dev") == h.validate_observations(rows, "dev")
    pq.write_table(pa.table({"date": ["2008-01-01"]}), path)
    with pytest.raises(ValueError, match="columns"):
        h.read_observations(path, "dev")


def test_sixth_missing_fx_day_on_spy_holiday_still_stops(rows):
    changed = copy.deepcopy(rows)
    dates = {
        "2010-03-01",
        "2010-03-02",
        "2010-03-03",
        "2010-03-04",
        "2010-03-05",
        "2010-03-08",
    }
    for row in changed:
        if row["kind"] == "fx" and row["series"] == "DEXUSEU" and row["date"] in dates:
            row["value"] = None
    changed = [
        row
        for row in changed
        if not (row["kind"] == "spy" and row["date"] == "2010-03-08")
    ]
    with pytest.raises(ValueError, match="FX unavailable"):
        h.indicators(h.History(h.validate_observations(changed, "dev")))


def test_ruin_stops_stages_and_never_reports_cagr(rows):
    history = h.History(h.validate_observations(rows, "dev"))
    prepared = h.indicators(history)
    before = history.month_ends[(2009, 12)]
    first = history.dates.index(date(2010, 1, 4))
    for j in range(9):
        prepared[before * 9 + j].update(signal=1, sigma=0.01)
        prepared[(first + 1) * 9 + j]["x"] = -1.0
    days, report = h.simulate(
        history, prepared, "test", date(2010, 1, 4), date(2010, 1, 6), h.SCENARIOS[0]
    )
    assert report["status"] == "ruined"
    assert report["ruin_date"] == "2010-01-05"
    assert report["cagr_pretax"] is report["cagr_aftertax"] is None
    assert report["reconciliation"] is True
    assert len(days) == 2
    assert all(
        days[-1][f"financing_{currency}"] == days[-1][f"transaction_{currency}"] == 0
        for currency in h.CURRENCIES
    )


def test_zero_signal_does_not_redistribute_and_spy_actions(rows):
    history = h.History(h.validate_observations(rows, "dev"))
    prepared = h.indicators(history)
    before = history.month_ends[(2009, 12)]
    prepared[before * 9]["signal"] = 0
    days, _ = h.simulate(
        history, prepared, "test", date(2010, 1, 4), date(2010, 1, 6), h.SCENARIOS[0]
    )
    assert days[0]["q_after_EUR"] == 0
    assert days[0]["N_JPY"] == pytest.approx(
        prepared[before * 9 + 1]["signal"] * 0.4 / prepared[before * 9 + 1]["sigma"] / 9
    )
    bars = [
        {
            "date": date(2010, 1, 4),
            "close": 100.0,
            "div_cash": 0.0,
            "split_factor": 1.0,
        },
        {"date": date(2010, 1, 5), "close": 51.0, "div_cash": 1.0, "split_factor": 2.0},
    ]
    values, _ = h.benchmark(bars)
    assert values[-1] == pytest.approx((1 / 1.0002 / 100) * (2 * 51 + 1) * 0.9998)


def test_holdout_open_before_verdict_and_stale_run_refused(tmp_path):
    args = gate_fixture(tmp_path)
    match, ledger, *_ = args
    records = [json.loads(line) for line in ledger.read_text().splitlines()]
    records[1], records[2] = records[2], records[1]
    ledger.write_text("".join(json.dumps(row) + "\n" for row in records))
    with pytest.raises(ValueError, match="opened after"):
        h.require_holdout(*args)
    records.append(
        {
            "hypothesis": "H005",
            "kind": "run",
            "stage": "dev",
            "results_sha256": "0" * 64,
        }
    )
    ledger.write_text("".join(json.dumps(row) + "\n" for row in records))
    with pytest.raises(ValueError, match="hash mismatch"):
        h.require_holdout(*args)
    assert match.is_file()
