"""Independently recompute the preregistered H004 RSI(2) leverage overlay test."""

import argparse
import json
import math
import re
import sys
from datetime import date
from itertools import pairwise
from pathlib import Path
from statistics import fmean, pstdev

import pandas as pd
import pyarrow as pa
import pyarrow.parquet as pq

from h001 import Bar, checked_run_directory, psr, validate_bars, write_verdict

MAIN_START = date(2008, 1, 2)
RECENT_START = date(2019, 11, 1)
END = date(2024, 10, 31)
HOLDOUT_START = date(2024, 11, 1)
HOLDOUT_END = date(2026, 10, 30)
SIGNAL_LOOKBACK = 200
TAX_RATE = 0.20315
PSR_THRESHOLD = 1 - 0.05 / 4
STRATEGY_COST = {"SSO": 0.0005, "SPY": 0.0002}
SPY_COST = 0.0002
DAY_FIELDS = ("signal", "holding", "V", "U", "R", "Q", "d", "A")


def read_bars(path: Path, stage: str = "dev") -> dict[str, list[Bar]]:
    schema = pq.read_schema(path)
    required = {
        "ticker": pa.string(),
        "date": pa.date32(),
        "close": pa.float64(),
        "div_cash": pa.float64(),
        "split_factor": pa.float64(),
    }
    if len(schema) != len(required) or any(
        name not in schema.names or schema.field(name).type != kind
        for name, kind in required.items()
    ):
        raise ValueError("invalid bars schema")
    frame = pd.read_parquet(path)
    end = END if stage == "dev" else HOLDOUT_END
    frame = frame[frame["date"] <= end]
    if frame.isna().any().any():
        raise ValueError("null bar value")
    grouped: dict[str, list[Bar]] = {}
    for ticker, rows in frame.groupby("ticker", sort=False):
        if ticker not in {"SPY", "SSO"}:
            raise ValueError("unexpected ticker")
        bars = [
            Bar(**row) for row in rows.drop(columns="ticker").to_dict(orient="records")
        ]
        validate_bars(bars)
        grouped[str(ticker)] = bars
    if set(grouped) != {"SPY", "SSO"}:
        raise ValueError("expected SPY and SSO bars")
    return grouped


def above_mean(value: float, window: list[float]) -> bool:
    difference = 0.0
    for previous in window:
        difference += value - previous
    return difference > 0.0


def next_signal(holding: bool, above200: bool, above5: bool, rsi: float) -> bool:
    if holding:
        return not above5
    return above200 and rsi <= 5


def signal_history(spy: list[Bar]) -> list[dict]:
    validate_bars(spy)
    index = [1.0]
    for previous, current in pairwise(spy):
        daily_return = (
            current.close * current.split_factor + current.div_cash
        ) / previous.close - 1
        value = index[-1] * (1 + daily_return)
        if not math.isfinite(value) or value <= 0:
            raise ValueError("invalid SPY total-return index")
        index.append(value)
    au = ad = 0.0
    holding = False
    rows = []
    for i, value in enumerate(index):
        rsi = None
        if i:
            delta = value - index[i - 1]
            up, down = max(delta, 0.0), max(-delta, 0.0)
            if i <= 2:
                au += up / 2
                ad += down / 2
            else:
                au = (au + up) / 2
                ad = (ad + down) / 2
            if i >= 2:
                rsi = 50.0 if au + ad == 0 else 100 * au / (au + ad)
        m5 = fmean(index[i - 4 : i + 1]) if i >= 4 else None
        m200 = (
            fmean(index[i - SIGNAL_LOOKBACK + 1 : i + 1])
            if i >= SIGNAL_LOOKBACK - 1
            else None
        )
        if any(
            not math.isfinite(number)
            for number in (value, au, ad, m200, m5, rsi)
            if number is not None
        ):
            raise ValueError("non-finite signal indicator")
        signal = None
        if i >= SIGNAL_LOOKBACK - 1:
            if i > SIGNAL_LOOKBACK - 1:
                assert m200 is not None and m5 is not None and rsi is not None
                holding = next_signal(
                    holding,
                    above_mean(value, index[i - SIGNAL_LOOKBACK + 1 : i + 1]),
                    above_mean(value, index[i - 4 : i + 1]),
                    rsi,
                )
            signal = holding
        rows.append(
            {
                "date": spy[i].date,
                "index": value,
                "m200": m200,
                "m5": m5,
                "rsi": rsi,
                "signal": signal,
            }
        )
    return rows


class TaxLedger:
    def __init__(self, first_year: int):
        self.income: dict[int, float] = {first_year: 0.0}
        self.losses: list[list[float | int]] = []
        self.taxes: dict[int, float] = {}
        self.finalized: set[int] = set()

    def add(self, year: int, amount: float) -> None:
        self.income[year] = self.income.get(year, 0.0) + amount

    def settle(self, year: int) -> float:
        if year in self.finalized:
            return self.taxes[year]
        income = self.income.get(year, 0.0)
        self.losses = [loss for loss in self.losses if year - int(loss[0]) <= 3]
        if income < 0:
            self.losses.append([year, -income])
            tax = 0.0
        else:
            remaining = income
            for loss in self.losses:
                used = min(remaining, float(loss[1]))
                remaining -= used
                loss[1] = float(loss[1]) - used
                if remaining == 0:
                    break
            self.losses = [loss for loss in self.losses if float(loss[1]) > 0]
            tax = TAX_RATE * remaining
        self.taxes[year] = tax
        self.finalized.add(year)
        return tax


def simulate(
    spy: list[Bar],
    sso: list[Bar],
    start: date,
    end: date,
    cost_multiplier: float = 1.0,
) -> tuple[list[dict], dict]:
    spy = [bar for bar in spy if bar.date <= end]
    signals = [row["signal"] for row in signal_history(spy)]
    by_date = {
        ticker: {bar.date: bar for bar in bars}
        for ticker, bars in (("SSO", sso), ("SPY", spy))
    }
    main_spy = [i for i, bar in enumerate(spy) if start <= bar.date <= end]
    if not main_spy or spy[main_spy[0]].date != start:
        raise ValueError("window start must be an SPY trading day")
    dates = [spy[i].date for i in main_spy]
    if dates[-1] != end:
        raise ValueError("window end must be an SPY trading day")
    if any(day not in by_date[ticker] for day in dates for ticker in ("SSO",)):
        raise ValueError("SSO missing an SPY window date")
    first_signal_index = main_spy[0] - 1
    if first_signal_index < SIGNAL_LOOKBACK - 1 or (
        start == MAIN_START and first_signal_index < SIGNAL_LOOKBACK
    ):
        raise ValueError("fewer than 200 SPY days before first order")
    if start == MAIN_START and spy[first_signal_index].date != date(2007, 12, 31):
        raise ValueError("first main signal must use 2007-12-31 SPY data")

    def target(index: int) -> str:
        if signals[index] is None:
            raise ValueError("signal unavailable")
        return "SSO" if signals[index] else "SPY"

    first_target = target(first_signal_index)
    holding = first_target
    rate = STRATEGY_COST[first_target] * cost_multiplier
    opening_bar = by_date[holding][dates[0]]
    shares = (1 / (1 + rate)) / opening_bar.close
    basis = shares * opening_bar.close * (1 + rate)
    ledger = TaxLedger(dates[0].year)
    previous_u = shares * opening_bar.close
    benchmark_shares = (1 / (1 + SPY_COST * cost_multiplier)) / spy[main_spy[0]].close
    benchmark_first = benchmark_shares * spy[main_spy[0]].close
    benchmark_previous = benchmark_first
    days = [
        {
            "date": dates[0],
            "signal": signals[first_signal_index],
            "holding": holding,
            "V": previous_u,
            "U": previous_u,
            "R": 0.0,
            "Q": 0.0,
            "d": 0.0,
            "A": previous_u,
        }
    ]
    benchmark_a = benchmark_previous
    benchmark_peak = benchmark_a
    benchmark_drawdown = 0.0
    wealth_peak = previous_u
    strategy_drawdown = 0.0
    switches = 0
    sso_days = 0
    entry_count = int(holding == "SSO")
    held_returns = []
    for offset, day in enumerate(dates[1:], start=1):
        previous_day = dates[offset - 1]
        year = day.year
        if year not in ledger.income:
            ledger.income[year] = 0.0
        if holding == "SSO":
            sso_days += 1
            current, previous = spy[main_spy[offset]], spy[main_spy[offset] - 1]
            held_returns.append(
                (current.close * current.split_factor + current.div_cash)
                / previous.close
                - 1
            )
        bar = by_date[holding][day]
        dividend = shares * bar.div_cash
        shares = shares * bar.split_factor + dividend / bar.close
        basis += dividend
        ledger.add(year, dividend)
        value_before_trade = shares * bar.close
        desired = target(main_spy[offset] - 1)
        if desired != holding and offset < len(dates) - 1:
            sale_proceeds = value_before_trade * (
                1 - STRATEGY_COST[holding] * cost_multiplier
            )
            ledger.add(year, sale_proceeds - basis)
            holding = desired
            new_bar = by_date[holding][day]
            shares = (
                sale_proceeds
                / (1 + STRATEGY_COST[holding] * cost_multiplier)
                / new_bar.close
            )
            basis = sale_proceeds
            switches += 1
            entry_count += int(holding == "SSO")
        value_after_switch = shares * by_date[holding][day].close
        paid_tax = 0.0
        value_before_tax = value_after_switch
        if day.year != previous_day.year:
            due = ledger.settle(previous_day.year)
            if due > 0:
                tax_gross = due / (1 - STRATEGY_COST[holding] * cost_multiplier)
                if tax_gross >= value_after_switch:
                    raise ValueError("tax sale exceeds portfolio value")
                sold_shares = tax_gross / by_date[holding][day].close
                ledger.add(year, due - basis * sold_shares / shares)
                basis *= 1 - sold_shares / shares
                shares -= sold_shares
                paid_tax = due
                value_before_tax = shares * by_date[holding][day].close + due
        after_tax = value_before_tax - paid_tax
        if (
            not all(math.isfinite(x) for x in (value_before_tax, after_tax))
            or value_before_tax <= 0
            or after_tax <= 0
        ):
            raise ValueError("invalid strategy wealth")
        daily_return = value_before_tax / previous_u - 1
        spy_bar = by_date["SPY"][day]
        benchmark_shares = (
            benchmark_shares * spy_bar.split_factor
            + benchmark_shares * spy_bar.div_cash / spy_bar.close
        )
        benchmark_value = benchmark_shares * spy_bar.close
        benchmark_return = benchmark_value / benchmark_previous - 1
        benchmark_previous_for_day = benchmark_previous
        benchmark_previous = benchmark_value
        benchmark_a *= 1 + benchmark_return
        if offset < len(dates) - 1:
            benchmark_peak = max(benchmark_peak, benchmark_a)
            benchmark_drawdown = max(
                benchmark_drawdown, 1 - benchmark_a / benchmark_peak
            )
        else:
            benchmark_previous_before_final = benchmark_previous_for_day
        previous_u = after_tax
        a_value = days[-1]["A"] * (1 + daily_return)
        if offset < len(dates) - 1:
            wealth_peak = max(wealth_peak, a_value)
            strategy_drawdown = max(strategy_drawdown, 1 - a_value / wealth_peak)
        days.append(
            {
                "date": day,
                "signal": signals[main_spy[offset] - 1],
                "holding": holding,
                "V": value_before_tax,
                "U": after_tax,
                "R": daily_return,
                "Q": benchmark_return,
                "d": daily_return - benchmark_return,
                "A": a_value,
            }
        )

    final_day = dates[-1]
    final_bar = by_date[holding][final_day]
    final_proceeds = (
        shares * final_bar.close * (1 - STRATEGY_COST[holding] * cost_multiplier)
    )
    ledger.add(final_day.year, final_proceeds - basis)
    final_tax = ledger.settle(final_day.year)
    if final_tax >= final_proceeds:
        raise ValueError("final tax exceeds liquidation proceeds")
    strategy_final = final_proceeds - final_tax
    benchmark_final = (
        benchmark_shares
        * by_date["SPY"][final_day].close
        * (1 - SPY_COST * cost_multiplier)
    )
    previous_day_u = days[-2]["U"]
    final_v = final_proceeds
    final_r = final_v / previous_day_u - 1
    final_q = benchmark_final / benchmark_previous_before_final - 1
    final_a = days[-2]["A"] * (1 + final_r)
    days[-1].update(
        holding="CASH",
        V=final_v,
        U=strategy_final,
        R=final_r,
        Q=final_q,
        d=final_r - final_q,
        A=final_a,
    )
    wealth_peak = max(wealth_peak, final_a)
    strategy_drawdown = max(strategy_drawdown, 1 - final_a / wealth_peak)
    benchmark_a = benchmark_final
    benchmark_peak = max(benchmark_peak, benchmark_a)
    benchmark_drawdown = max(benchmark_drawdown, 1 - benchmark_a / benchmark_peak)
    ledger.taxes[final_day.year] = final_tax
    for year in range(start.year, end.year + 1):
        ledger.taxes.setdefault(year, 0.0)
    cal_days = (end - start).days
    metrics = {
        "W": strategy_final,
        "B": benchmark_final,
        "strategy_cagr_pretax": (days[-1]["A"] / days[0]["A"]) ** (365.25 / cal_days)
        - 1,
        "strategy_cagr_aftertax": strategy_final ** (365.25 / cal_days) - 1,
        "benchmark_cagr_pretax": (benchmark_a / benchmark_first) ** (365.25 / cal_days)
        - 1,
        "benchmark_cagr_aftertax": benchmark_final ** (365.25 / cal_days) - 1,
        "strategy_drawdown": strategy_drawdown,
        "benchmark_drawdown": benchmark_drawdown,
        "switch_count": switches,
        "sso_share": sso_days / max(1, len(dates) - 1),
        "entry_count": entry_count,
        "mean_holding_days": sso_days / entry_count if entry_count else None,
        "sso_spy_mean": fmean(held_returns) if held_returns else None,
        "sso_spy_std": pstdev(held_returns) if held_returns else None,
        "tax_by_year": {
            str(year): ledger.taxes[year] for year in range(start.year, end.year + 1)
        },
    }
    return days, metrics


def require_dev_verdict(path: Path | None) -> None:
    if path is None:
        raise ValueError("holdout requires dev verdict")
    verdict = json.loads(path.read_text())
    if not isinstance(verdict, dict):
        raise TypeError("invalid dev verdict")
    if verdict.get("match") is not True:
        raise ValueError("dev verdict must match")
    verdict = verdict.get("python_summary")
    if (
        not isinstance(verdict, dict)
        or verdict.get("stage") != "dev"
        or any(verdict.get(key) is not True for key in ("gate_a", "gate_b", "gate_c"))
    ):
        raise ValueError("holdout requires passing A, B, C")


def compute(
    bars: dict[str, list[Bar]], stage: str = "dev", dev_verdict: Path | None = None
) -> tuple[list[dict], dict]:
    if stage not in {"dev", "holdout"}:
        raise ValueError("invalid stage")
    if stage == "holdout":
        require_dev_verdict(dev_verdict)
    end = END if stage == "dev" else HOLDOUT_END
    bars = {
        ticker: [bar for bar in bars[ticker] if bar.date <= end]
        for ticker in ("SPY", "SSO")
    }
    for rows in bars.values():
        validate_bars(rows)
    spy = bars["SPY"]
    first_signal = next(
        (i for i, bar in enumerate(spy) if bar.date == date(2007, 12, 31)), -1
    )
    if first_signal < 200:
        raise ValueError("need 200 SPY days before first signal day")
    if stage == "holdout":
        days, metrics = simulate(spy, bars["SSO"], HOLDOUT_START, HOLDOUT_END)
        return days, {
            "stage": stage,
            "n_cumulative": 4,
            "holdout_n": len(days),
            "gate_h": metrics["W"] > metrics["B"],
            "holdout": metrics,
        }
    main_days, main = simulate(spy, bars["SSO"], MAIN_START, END)
    _, recent = simulate(spy, bars["SSO"], RECENT_START, END)
    _, cost2 = simulate(spy, bars["SSO"], MAIN_START, END, 2.0)
    differences = [row["d"] for row in main_days[1:]]
    probability = psr(differences)
    summary = {
        "stage": stage,
        "n_cumulative": 4,
        "main_n": len(main_days),
        "recent_n": sum(RECENT_START <= row["date"] <= END for row in main_days),
        "psr_t": len(differences),
        "psr_main": probability,
        "psr_threshold": PSR_THRESHOLD,
        "gate_a": main["W"] > main["B"],
        "gate_b": probability >= PSR_THRESHOLD,
        "gate_c": recent["W"] > recent["B"],
        "gate_a_cost2x": cost2["W"] > cost2["B"],
        "main": main,
        "recent": recent,
    }
    return main_days, summary


def matches(expected: object, actual: object, tolerance: float) -> bool:
    if isinstance(expected, float):
        return (
            isinstance(actual, (float, int))
            and not isinstance(actual, bool)
            and math.isfinite(actual)
            and abs(expected - actual) <= tolerance * max(abs(expected), abs(actual))
        )
    if isinstance(expected, dict):
        return (
            isinstance(actual, dict)
            and expected.keys() == actual.keys()
            and all(
                matches(value, actual[key], tolerance)
                for key, value in expected.items()
            )
        )
    if isinstance(expected, list):
        return (
            isinstance(actual, list)
            and len(expected) == len(actual)
            and all(
                matches(a, b, tolerance) for a, b in zip(expected, actual, strict=True)
            )
        )
    return type(expected) is type(actual) and expected == actual


def matches_return(expected: float, actual: object) -> bool:
    return (
        isinstance(actual, (float, int))
        and not isinstance(actual, bool)
        and math.isfinite(actual)
        and abs(expected - actual) <= 1e-10 * max(1.0, abs(expected), abs(actual))
    )


def read_days(path: Path) -> list[dict]:
    schema = pq.read_schema(path)
    required = {
        "date": pa.date32(),
        "signal": pa.bool_(),
        "holding": pa.string(),
        **dict.fromkeys(("V", "U", "R", "Q", "d", "A"), pa.float64()),
    }
    if len(schema) != len(required) or any(
        name not in schema.names or schema.field(name).type != kind
        for name, kind in required.items()
    ):
        raise ValueError("invalid days schema")
    frame = pd.read_parquet(path)
    if set(frame.columns) != set(required):
        raise ValueError("invalid days columns")
    if frame.isna().any().any():
        raise ValueError("null day value")
    rows = frame.to_dict(orient="records")
    for row in rows:
        if type(row["date"]) is not date or type(row["signal"]) is not bool:
            raise ValueError("invalid day value")
        if row["holding"] not in {"SSO", "SPY", "CASH"} or not all(
            math.isfinite(row[name]) for name in ("V", "U", "R", "Q", "d", "A")
        ):
            raise ValueError("invalid day value")
    return rows


def compare(
    calculated_days: list[dict],
    calculated_summary: dict,
    rust_days: list[dict],
    rust_summary: dict,
) -> list[str]:
    mismatches = []
    if len(calculated_days) != len(rust_days):
        mismatches.append("days: row count differs")
    for i, (expected, actual) in enumerate(
        zip(calculated_days, rust_days, strict=False)
    ):
        if expected.keys() != actual.keys():
            mismatches.append(f"days[{i}]: fields differ")
        for key, value in expected.items():
            tolerance = 1e-10 if key in DAY_FIELDS[2:] else 0.0
            observed = actual.get(key)
            matched = (
                matches_return(value, observed)
                if key in {"R", "Q", "d"}
                else matches(value, observed, tolerance)
            )
            if not matched:
                mismatches.append(f"days[{i}].{key}: mismatch")
    for key, value in calculated_summary.items():
        tolerance = 1e-9 if key == "psr_main" else 1e-10
        if not matches(value, rust_summary.get(key), tolerance):
            mismatches.append(f"summary.{key}: mismatch")
    if calculated_summary.keys() != rust_summary.keys():
        mismatches.append("summary: fields differ")
    return mismatches


def run(
    directory: Path, stage: str = "dev", dev_verdict: Path | None = None
) -> tuple[Path, str, bool]:
    if stage == "holdout":
        require_dev_verdict(dev_verdict)
    directory = checked_run_directory(directory)
    bars = read_bars(directory / "bars.parquet", stage)
    calculated_days, calculated_summary = compute(bars, stage, dev_verdict)
    rust_summary = json.loads((directory / "summary.json").read_text())
    if not isinstance(rust_summary, dict):
        raise TypeError("invalid summary")
    digests = rust_summary.get("input_sha256")
    if (
        not isinstance(digests, dict)
        or digests.keys() != {"SPY", "SSO"}
        or any(
            not isinstance(value, str) or re.fullmatch(r"[0-9a-f]{64}", value) is None
            for value in digests.values()
        )
    ):
        digests = None
    calculated_summary["input_sha256"] = digests
    rust_days = read_days(directory / "days.parquet")
    mismatches = compare(calculated_days, calculated_summary, rust_days, rust_summary)
    expected_indicators = signal_history(bars["SPY"])
    schema = pq.read_schema(directory / "indicators.parquet")
    required = {
        "date": pa.date32(),
        "signal": pa.bool_(),
        **dict.fromkeys(("index", "m200", "m5", "rsi"), pa.float64()),
    }
    if len(schema) != len(required) or any(
        name not in schema.names or schema.field(name).type != kind
        for name, kind in required.items()
    ):
        raise ValueError("invalid indicators schema")
    rust_indicators = pq.read_table(directory / "indicators.parquet").to_pylist()
    if len(expected_indicators) != len(rust_indicators):
        mismatches.append("indicators: row count differs")
    for i, (expected, actual) in enumerate(
        zip(expected_indicators, rust_indicators, strict=False)
    ):
        for key, value in expected.items():
            if not matches(value, actual.get(key), 1e-10):
                mismatches.append(f"indicators[{i}].{key}: mismatch")
    if digests is None:
        mismatches.append("summary.input_sha256: invalid")
    verdict = {
        "python_summary": calculated_summary,
        "rust_summary": rust_summary,
        "match": not mismatches,
        "mismatches": mismatches,
    }
    path, digest = write_verdict(directory, verdict)
    return path, digest, not mismatches


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--run", required=True, type=Path)
    parser.add_argument("--stage", required=True, choices=("dev", "holdout"))
    parser.add_argument("--dev-verdict", type=Path)
    args = parser.parse_args()
    try:
        path, digest, matched = run(args.run, args.stage, args.dev_verdict)
    except (OSError, ValueError, TypeError, KeyError, ArithmeticError):
        print("H004 input or computation failed", file=sys.stderr)
        return 1
    print(f"{path} sha256={digest} match={str(matched).lower()}")
    return 0 if matched else 1


if __name__ == "__main__":
    sys.exit(main())
