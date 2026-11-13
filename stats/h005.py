"""Independently recompute the H005 currency momentum preregistration."""

import argparse
import hashlib
import json
import math
import re
import sys
from datetime import date, timedelta
from pathlib import Path
from typing import TypeGuard

import pyarrow as pa
import pyarrow.parquet as pq

from h001 import checked_run_directory, psr, write_verdict

CURRENCIES = ("EUR", "JPY", "GBP", "CHF", "CAD", "AUD", "NZD", "NOK", "SEK")
DEX = (
    "DEXUSEU",
    "DEXJPUS",
    "DEXUSUK",
    "DEXSZUS",
    "DEXCAUS",
    "DEXUSAL",
    "DEXUSNZ",
    "DEXNOUS",
    "DEXSDUS",
)
AREAS = ("EA20", "JPN", "GBR", "CHE", "CAN", "AUS", "NZL", "NOR", "SWE")
FIRST_RATES = (
    "IR3TIB01EZM156N",
    "IR3TIB01JPM156N",
    "IR3TIB01GBM156N",
    "IR3TIB01CHM156N",
    "IR3TIB01CAM156N",
    "IR3TIB01AUM156N",
    "IR3TIB01NZM156N",
    "IR3TIB01NOM156N",
    "IR3TIB01SEM156N",
)
US_FIRST = "IR3TIB01USM156N"
INVERT = {"DEXJPUS", "DEXSZUS", "DEXCAUS", "DEXNOUS", "DEXSDUS"}
END = date(2024, 10, 31)
HOLDOUT_END = date(2026, 10, 30)
SCENARIOS = (
    ("verdict", 1.0, 0.0001, 0.0025),
    ("k1_5", 1.5, 0.0001, 0.0025),
    ("k1_8", 1.8, 0.0001, 0.0025),
    ("k2", 2.0, 0.0001, 0.0025),
    ("k3", 3.0, 0.0001, 0.0025),
    ("zero", 1.0, 0.0, 0.0),
    ("double", 1.0, 0.0002, 0.005),
    ("high", 1.0, 0.0001, 0.01),
)
OBS_FIELDS = {"kind", "series", "date", "value", "close", "div_cash", "split_factor"}
RESULT_FILES = (
    "days.parquet",
    "indicators.parquet",
    "observations.parquet",
    "summary.json",
)


def day(value: object) -> date:
    if type(value) is date:
        return value
    if not isinstance(value, str) or re.fullmatch(r"\d{4}-\d{2}-\d{2}", value) is None:
        raise ValueError("invalid ISO date")
    return date.fromisoformat(value)


def month(value: date) -> tuple[int, int]:
    return value.year, value.month


def previous_month(value: date) -> date:
    return (value.replace(day=1) - timedelta(days=1)).replace(day=1)


def business_days(first: date, last: date) -> int:
    return sum(
        (first + timedelta(days=i)).weekday() < 5
        for i in range((last - first).days + 1)
    )


def finite(value: object) -> TypeGuard[float | int]:
    return (
        isinstance(value, (float, int))
        and not isinstance(value, bool)
        and math.isfinite(value)
    )


def read_observations(path: Path, stage: str) -> list[dict]:
    schema = pq.read_schema(path)
    if set(schema.names) != OBS_FIELDS or len(schema.names) != len(OBS_FIELDS):
        raise ValueError("invalid observations columns")
    if any(schema.field(name).type != pa.string() for name in ("kind", "series")):
        raise ValueError("invalid observation identifier types")
    if schema.field("date").type not in (pa.string(), pa.date32()):
        raise ValueError("invalid observation date type")
    if any(
        schema.field(name).type != pa.float64()
        for name in OBS_FIELDS - {"kind", "series", "date"}
    ):
        raise ValueError("invalid observation numeric types")
    return validate_observations(pq.read_table(path).to_pylist(), stage)


def validate_observations(rows: list[dict], stage: str) -> list[dict]:
    if stage not in {"dev", "holdout"}:
        raise ValueError("invalid stage")
    filtered = []
    previous = None
    for source in rows:
        if source.keys() != OBS_FIELDS:
            raise ValueError("invalid observation fields")
        kind, series, observed = source["kind"], source["series"], day(source["date"])
        if stage == "dev" and (
            observed > END
            or (kind in {"rate", "first_rate"} and observed >= date(2024, 10, 1))
        ):
            continue
        if observed > HOLDOUT_END:
            continue
        key = kind, series, observed
        if previous is not None and key <= previous:
            raise ValueError("observations must be sorted and unique")
        previous = key
        allowed = {
            "fx": DEX,
            "first_fx": DEX,
            "rate": (*AREAS, "USA"),
            "first_rate": (*FIRST_RATES, US_FIRST),
            "spy": ("SPY",),
        }
        if kind not in allowed or series not in allowed[kind]:
            raise ValueError("unexpected observation series")
        row = {**source, "date": observed}
        if kind == "spy":
            if source["value"] is not None or any(
                not finite(source[name])
                for name in ("close", "div_cash", "split_factor")
            ):
                raise ValueError("invalid SPY value")
            if (
                source["close"] <= 0
                or source["div_cash"] < 0
                or source["split_factor"] <= 0
            ):
                raise ValueError("invalid SPY price or action")
        else:
            if any(
                source[name] is not None
                for name in ("close", "div_cash", "split_factor")
            ):
                raise ValueError("unexpected non-SPY fields")
            value = source["value"]
            if value is not None and (
                not finite(value) or (kind.endswith("fx") and value <= 0)
            ):
                raise ValueError("invalid observation value")
            if kind in {"rate", "first_rate"} and observed.day != 1:
                raise ValueError("rate month must begin on first calendar day")
        filtered.append(row)
    if not filtered:
        raise ValueError("empty observations")
    for kind, required in (
        ("fx", set(DEX)),
        ("rate", {*AREAS, "USA"}),
        ("spy", {"SPY"}),
    ):
        if {row["series"] for row in filtered if row["kind"] == kind} != required:
            raise ValueError("missing required observation series")
    return filtered


class History:
    def __init__(self, rows: list[dict]):
        self.maps: dict[tuple[str, str], dict[date, float | None]] = {}
        self.spy = []
        for row in rows:
            if row["kind"] == "spy":
                if row["date"] >= date(2008, 1, 1):
                    self.spy.append(row)
            else:
                self.maps.setdefault((row["kind"], row["series"]), {})[row["date"]] = (
                    row["value"]
                )
        if not self.spy or month(self.spy[0]["date"]) != (2008, 1):
            raise ValueError("preparation must start in January 2008")
        self.dates = [row["date"] for row in self.spy]
        self.month_ends = {month(value): i for i, value in enumerate(self.dates)}
        self._fx_cache: dict[tuple[str, str], dict[date, float | None]] = {}
        self._rate_cache: dict[tuple[str, str, date], float | None] = {}

    def spot(
        self, series: str, when: date, kind: str = "fx", optional: bool = False
    ) -> float | None:
        key = kind, series
        if key not in self._fx_cache:
            source = self.maps.get(key, {})
            result = {}
            if source:
                first, last = min(source), self.dates[-1]
                latest = None
                age = 0
                cursor = first
                while cursor <= last:
                    if cursor.weekday() < 5:
                        value = source.get(cursor)
                        if value is not None:
                            latest, age = value, 0
                        else:
                            age += 1
                        result[cursor] = latest if age <= 5 else None
                    cursor += timedelta(days=1)
            self._fx_cache[key] = result
        value = self._fx_cache[key].get(when)
        if value is None:
            if optional:
                return None
            raise ValueError(
                f"FX unavailable beyond five business days: {series} {when}"
            )
        return 1.0 / value if series in INVERT else value

    def rate(
        self, area: str, when: date, vintage: bool = False, optional: bool = False
    ) -> float | None:
        series = US_FIRST if area == "USA" else FIRST_RATES[AREAS.index(area)]
        kind = "first_rate" if vintage else "rate"
        key = kind, area, when
        if key not in self._rate_cache:
            source = self.maps.get((kind, series if vintage else area), {})
            needed = previous_month(when)
            if area == "USA" and needed == date(2020, 4, 1):
                value = self.maps.get(("rate", "USA"), {}).get(date(2020, 3, 1))
            else:
                value = source.get(needed)
                if value is None and business_days(when.replace(day=1), when) <= 5:
                    preceding = [
                        d for d, val in source.items() if d < needed and val is not None
                    ]
                    if preceding:
                        value = source[max(preceding)]
            self._rate_cache[key] = None if value is None else value / 100.0
        result = self._rate_cache[key]
        if result is None and not optional:
            raise ValueError(
                f"rate unavailable beyond five business days: {area} {when}"
            )
        return result

    def interest(
        self,
        area: str,
        first: date,
        last: date,
        vintage: bool = False,
        optional: bool = False,
    ) -> float | None:
        result = 0.0
        cursor = first
        while cursor < last:
            foreign = self.rate(area, cursor, vintage, optional)
            us = self.rate("USA", cursor, vintage, optional)
            if foreign is None or us is None:
                return None
            result += (foreign - us) / 365.0
            cursor += timedelta(days=1)
        return result


def indicators(history: History) -> list[dict]:
    observed = history.dates[0]
    while observed <= history.dates[-1]:
        if observed.weekday() < 5:
            for series in DEX:
                history.spot(series, observed)
        observed += timedelta(days=1)
    result = []
    means = [0.0] * 9
    seconds = [0.0] * 9
    weight = 0.0
    returns: list[list[float | None]] = [[] for _ in CURRENCIES]
    decay = 60.0 / 61.0
    for i, observed in enumerate(history.dates):
        if i:
            weight = decay * weight + (1.0 - decay)
        for j, currency in enumerate(CURRENCIES):
            spot = history.spot(DEX[j], observed)
            foreign, us = (
                history.rate(AREAS[j], observed),
                history.rate("USA", observed),
            )
            interest = x = mean_value = sigma = cumulative = signal = None
            if i:
                old = history.spot(DEX[j], history.dates[i - 1])
                interest = history.interest(AREAS[j], history.dates[i - 1], observed)
                assert spot is not None and old is not None and interest is not None
                x = spot / old - 1 + interest
                if not math.isfinite(x):
                    raise ValueError("non-finite FX excess return")
                returns[j].append(x)
                newest_weight = (1.0 - decay) / weight
                deviation = x - means[j]
                means[j] += newest_weight * deviation
                seconds[j] = (1.0 - newest_weight) * (
                    seconds[j] + newest_weight * deviation * deviation
                )
                mean_value = means[j]
                sigma = math.sqrt(261.0 * max(0.0, seconds[j]))
            is_end = history.month_ends[month(observed)] == i
            if (
                is_end
                and sigma is not None
                and (sigma <= 0 or not math.isfinite(sigma))
            ):
                raise ValueError("volatility must be positive and finite")
            lookback = history.month_ends.get((observed.year - 1, observed.month))
            if is_end and lookback is not None:
                cumulative = 1.0
                for value in returns[j][lookback:i]:
                    assert value is not None
                    cumulative *= 1.0 + value
                cumulative -= 1.0
                signal = int(cumulative > 0) - int(cumulative < 0)
            result.append(
                {
                    "date": observed.isoformat(),
                    "currency": currency,
                    "S": spot,
                    "r_foreign": foreign,
                    "r_us": us,
                    "interest": interest,
                    "x": x,
                    "mean": mean_value,
                    "sigma": sigma,
                    "X": cumulative,
                    "signal": signal,
                    "month_end": is_end,
                }
            )
    return result


class TaxLedger:
    def __init__(self):
        self.income: dict[int, float] = {}
        self.losses: list[list[float | int]] = []
        self.taxes: dict[int, float] = {}

    def add(self, year: int, amount: float) -> None:
        self.income[year] = self.income.get(year, 0.0) + amount

    def settle(self, year: int) -> float:
        self.losses = [loss for loss in self.losses if 1 <= year - int(loss[0]) <= 3]
        amount = self.income.get(year, 0.0)
        if amount < 0:
            self.losses.append([year, -amount])
            tax = 0.0
        else:
            for loss in self.losses:
                used = min(amount, float(loss[1]))
                amount -= used
                loss[1] = float(loss[1]) - used
            tax = amount * 0.20315
        self.taxes[year] = tax
        return tax


def benchmark(bars: list[dict]) -> tuple[list[float], dict]:
    shares = 1.0 / 1.0002 / bars[0]["close"]
    values = []
    peak, drawdown = 1.0, 0.0
    for i, bar in enumerate(bars):
        if i:
            shares = (
                shares * bar["split_factor"] + shares * bar["div_cash"] / bar["close"]
            )
        value = shares * bar["close"]
        if i == len(bars) - 1:
            value *= 0.9998
        peak = max(peak, value)
        drawdown = max(drawdown, 1.0 - value / peak)
        values.append(value)
    elapsed = (bars[-1]["date"] - bars[0]["date"]).days
    cagr = values[-1] ** (365.0 / elapsed) - 1.0
    return values, {"cagr_pretax": cagr, "cagr_aftertax": cagr, "drawdown": drawdown}


def simulate(
    history: History,
    prepared: list[dict],
    period: str,
    start: date,
    end: date,
    scenario: tuple,
) -> tuple[list[dict], dict]:
    name, k, c, f = scenario
    indices = [i for i, value in enumerate(history.dates) if start <= value <= end]
    if (
        not indices
        or history.dates[indices[0]] != start
        or history.dates[indices[-1]] != end
    ):
        raise ValueError("period endpoints must be SPY common days")
    if indices[0] == 0 or month(history.dates[indices[0] - 1]) == month(start):
        raise ValueError("period must begin after a preparation month end")
    bars = [history.spy[i] for i in indices]
    baseline, benchmark_report = benchmark(bars)
    q = [0.0] * 9
    equity = pretax = 1.0
    ledger = TaxLedger()
    targets = None
    rows = []
    contributions = [0.0] * 9
    turnover = total_tax = 0.0
    max_gross, min_inverse = 0.0, None
    peak_pre = peak_post = 1.0
    dd_pre = dd_post = 0.0
    month_base = year_pre = year_post = 1.0
    worst_month = None
    years = []
    ruined = False

    def target(signal_index: int, wealth: float) -> list[float]:
        if signal_index < 261:
            raise ValueError("fewer than 261 preparation returns")
        result = []
        for j in range(9):
            row = prepared[signal_index * 9 + j]
            if row["signal"] is None or row["sigma"] is None or row["sigma"] <= 0:
                raise ValueError(
                    "insufficient signal preparation or invalid volatility"
                )
            result.append(row["signal"] * wealth * k * 0.40 / row["sigma"] / 9.0)
        return result

    targets = target(indices[0] - 1, 1.0)
    for offset, index in enumerate(indices):
        observed = history.dates[index]
        previous_equity = equity
        spots = [prepared[index * 9 + j]["S"] for j in range(9)]
        incoming = q.copy()
        gross = [0.0] * 9
        pnl = [0.0] * 9
        financing = [0.0] * 9
        transaction = [0.0] * 9
        executed = [None] * 9
        before_gross_amount = sum(abs(incoming[j]) * spots[j] for j in range(9))
        before_gross = before_gross_amount / previous_equity
        max_gross = max(max_gross, before_gross)
        if before_gross > 0:
            inverse = 1.0 / before_gross
            min_inverse = inverse if min_inverse is None else min(min_inverse, inverse)
        if offset:
            calendar_days = (observed - history.dates[index - 1]).days
            for j in range(9):
                previous_spot = prepared[(index - 1) * 9 + j]["S"]
                gross[j] = abs(q[j]) * previous_spot
                pnl[j] = q[j] * previous_spot * prepared[index * 9 + j]["x"]
            equity += sum(pnl)
            ruined = equity <= 0 or not math.isfinite(equity)
            if not ruined:
                financing = [f * value * calendar_days / 365.0 for value in gross]
                equity -= sum(financing)
                ruined = equity <= 0 or not math.isfinite(equity)
        is_last = offset == len(indices) - 1
        if not ruined and (is_last or targets is not None):
            if not is_last:
                assert targets is not None
            executable = [0.0] * 9 if targets is None else targets
            desired = (
                [0.0] * 9 if is_last else [executable[j] / spots[j] for j in range(9)]
            )
            executed = [0.0] * 9 if is_last else executable.copy()
            traded = [abs(desired[j] - q[j]) * spots[j] for j in range(9)]
            transaction = [c * value for value in traded]
            turnover += sum(traded) / equity
            equity -= sum(transaction)
            q = desired
            ruined = equity <= 0 or not math.isfinite(equity)
        targets = None
        income = sum(pnl) - sum(financing) - sum(transaction)
        ledger.add(observed.year, income)
        for j in range(9):
            contributions[j] += pnl[j] - financing[j] - transaction[j]
        value_before_tax = equity
        daily_return = value_before_tax / previous_equity - 1.0 if offset else None
        pretax = (
            pretax * (1.0 + daily_return)
            if daily_return is not None
            else value_before_tax
        )
        ruined = (
            ruined
            or not math.isfinite(pretax)
            or (daily_return is not None and not math.isfinite(daily_return))
        )
        is_year_end = (
            is_last or history.dates[indices[offset + 1]].year != observed.year
        )
        tax = 0.0
        if not ruined and is_year_end:
            tax = ledger.settle(observed.year)
            equity -= tax
            total_tax += tax
            ruined = equity <= 0 or not math.isfinite(equity)
        after_gross_amount = sum(abs(q[j]) * spots[j] for j in range(9))
        after_gross = after_gross_amount / equity if equity > 0 else None
        if after_gross is not None:
            max_gross = max(max_gross, after_gross)
            if after_gross > 0:
                inverse = 1.0 / after_gross
                min_inverse = (
                    inverse if min_inverse is None else min(min_inverse, inverse)
                )
        if math.isfinite(pretax) and math.isfinite(equity):
            peak_pre, peak_post = max(peak_pre, pretax), max(peak_post, equity)
            dd_pre, dd_post = (
                max(dd_pre, 1.0 - pretax / peak_pre),
                max(dd_post, 1.0 - equity / peak_post),
            )
        if (
            ruined
            or is_last
            or history.dates[indices[offset + 1]].month != observed.month
        ):
            monthly_return = equity / month_base - 1.0
            worst_month = (
                monthly_return
                if worst_month is None
                else min(worst_month, monthly_return)
            )
            month_base = equity
        if is_year_end:
            years.append(
                {
                    "year": observed.year,
                    "pretax_return": pretax / year_pre - 1.0,
                    "aftertax_return": equity / year_post - 1.0,
                    "income": ledger.income[observed.year],
                    "tax": tax,
                    "losses": [
                        {"year": int(y), "amount": amount}
                        for y, amount in ledger.losses
                    ],
                }
            )
            year_pre, year_post = pretax, equity
        row = {
            "period": period,
            "scenario": name,
            "date": observed.isoformat(),
            "V": value_before_tax,
            "E": equity,
            "R": daily_return,
            "A": pretax,
            "tax": tax,
            "B": baseline[offset],
            "year_income": ledger.income[observed.year],
            "turnover": turnover,
            "gross_before": before_gross_amount,
            "gross_after": after_gross_amount,
        }
        for j, currency in enumerate(CURRENCIES):
            row.update(
                {
                    f"q_before_{currency}": incoming[j],
                    f"q_after_{currency}": q[j],
                    f"N_{currency}": executed[j],
                    f"P_{currency}": pnl[j],
                    f"G_{currency}": gross[j],
                    f"transaction_{currency}": transaction[j],
                    f"financing_{currency}": financing[j],
                }
            )
        rows.append(clean_finite(row))
        if ruined:
            break
        if history.month_ends[month(observed)] == index and not is_last:
            targets = target(index, equity)
    actual_end = day(rows[-1]["date"])
    elapsed = (actual_end - start).days
    report = {
        "scenario": name,
        "k": k,
        "c": c,
        "f": f,
        "status": "ruined" if ruined else "ok",
        "ruin_date": actual_end.isoformat() if ruined else None,
        "start": start.isoformat(),
        "end": actual_end.isoformat(),
        "W": equity,
        "B": baseline[-1],
        "A": pretax,
        "cagr_pretax": None if ruined else pretax ** (365.0 / elapsed) - 1.0,
        "cagr_aftertax": None if ruined else equity ** (365.0 / elapsed) - 1.0,
        "drawdown_pretax": dd_pre,
        "drawdown_aftertax": dd_post,
        "worst_month": worst_month,
        "max_gross_to_equity": max_gross,
        "min_equity_to_gross": min_inverse,
        "turnover": turnover,
        "total_tax": total_tax,
        "contribution": dict(zip(CURRENCIES, contributions, strict=True)),
        "contribution_total": sum(contributions),
        "reconciliation": math.isfinite(equity)
        and math.isfinite(sum(contributions))
        and abs(sum(contributions) - total_tax - (equity - 1.0))
        <= 1e-10 * max(1.0, abs(sum(contributions) - total_tax), abs(equity - 1.0)),
        "yearly": years,
        "benchmark": benchmark_report,
    }
    return rows, clean_finite(report)


def clean_finite(value: object):
    if isinstance(value, float):
        return value if math.isfinite(value) else None
    if isinstance(value, dict):
        return {key: clean_finite(item) for key, item in value.items()}
    if isinstance(value, list):
        return [clean_finite(item) for item in value]
    return value


def vintage_signal(
    history: History,
    j: int,
    end_index: int,
    spot_vintage: bool = False,
    rate_vintage: bool = False,
) -> int | None:
    observed = history.dates[end_index]
    begin = history.month_ends.get((observed.year - 1, observed.month))
    if begin is None:
        return None
    cumulative = 1.0
    for i in range(begin + 1, end_index + 1):
        previous, current = history.dates[i - 1], history.dates[i]
        old = history.spot(DEX[j], previous, "first_fx" if spot_vintage else "fx", True)
        new = history.spot(DEX[j], current, "first_fx" if spot_vintage else "fx", True)
        interest = history.interest(AREAS[j], previous, current, rate_vintage, True)
        if old is None or new is None or interest is None:
            return None
        cumulative *= 1.0 + (new / old - 1.0 + interest)
    if not math.isfinite(cumulative):
        return None
    cumulative -= 1.0
    return int(cumulative > 0) - int(cumulative < 0)


def availability(values: list[dict], key: str) -> tuple[str | None, str | None]:
    dates = [row["date"] for row in values if row[key] is not None]
    return (dates[0], dates[-1]) if dates else (None, None)


def vintages(history: History, prepared: list[dict], stage: str) -> dict:
    first = date(2010, 1, 1) if stage == "dev" else date(2024, 11, 1)
    signal_first = date(2013, 6, 1) if stage == "dev" else first
    indices = [
        index for index in history.month_ends.values() if history.dates[index] >= first
    ]
    spots = []
    rate_signals = []
    for j, currency in enumerate(CURRENCIES):
        values = []
        signals = []
        for index in indices:
            observed = history.dates[index]
            current = prepared[index * 9 + j]
            initial = history.spot(DEX[j], observed, "first_fx", True)
            initial_signal = vintage_signal(history, j, index, spot_vintage=True)
            values.append(
                {
                    "date": observed.isoformat(),
                    "current": current["S"],
                    "first": initial,
                    "difference": None if initial is None else current["S"] - initial,
                    "current_signal": current["signal"],
                    "first_signal": initial_signal,
                }
            )
            if observed >= signal_first:
                signals.append(
                    {
                        "date": observed.isoformat(),
                        "current_signal": current["signal"],
                        "first_signal": vintage_signal(
                            history, j, index, rate_vintage=True
                        ),
                    }
                )
        available_start, available_end = availability(values, "first")
        comparable = [
            row
            for row in values
            if row["first_signal"] is not None and row["current_signal"] is not None
        ]
        spots.append(
            {
                "currency": currency,
                "series": DEX[j],
                "available_start": available_start,
                "available_end": available_end,
                "comparable_months": len(comparable),
                "signal_flips": sum(
                    row["current_signal"] != row["first_signal"] for row in comparable
                ),
                "values": values,
            }
        )
        available_start, available_end = availability(signals, "first_signal")
        comparable = [
            row
            for row in signals
            if row["first_signal"] is not None and row["current_signal"] is not None
        ]
        rate_signals.append(
            {
                "currency": currency,
                "available_start": available_start,
                "available_end": available_end,
                "comparable_months": len(comparable),
                "signal_flips": sum(
                    row["current_signal"] != row["first_signal"] for row in comparable
                ),
                "values": signals,
            }
        )
    rates = []
    stop = date(2024, 9, 1) if stage == "dev" else date(2026, 9, 1)
    for area, series in zip((*AREAS, "USA"), (*FIRST_RATES, US_FIRST), strict=True):
        values = []
        observed = date(2013, 6, 1)
        while observed <= stop:
            current = history.maps.get(("rate", area), {}).get(observed)
            initial = history.maps.get(("first_rate", series), {}).get(observed)
            current = None if current is None else current / 100.0
            initial = None if initial is None else initial / 100.0
            if area == "USA" and observed == date(2020, 4, 1):
                current = initial = None
            values.append(
                {
                    "date": observed.isoformat(),
                    "current": current,
                    "first": initial,
                    "difference": None
                    if current is None or initial is None
                    else current - initial,
                }
            )
            observed = date(
                observed.year + (observed.month == 12), observed.month % 12 + 1, 1
            )
        available_start, available_end = availability(values, "difference")
        rates.append(
            {
                "area": area,
                "series": series,
                "available_start": available_start,
                "available_end": available_end,
                "comparable_months": sum(
                    row["difference"] is not None for row in values
                ),
                "values": values,
            }
        )
    return {
        "spot": spots,
        "rates": rates,
        "rate_signals": rate_signals,
        "eur_note": "OECD EA20 versus ALFRED EA19: differences include aggregation definitions",
    }


def compute(
    rows: list[dict], stage: str = "dev"
) -> tuple[list[dict], list[dict], dict]:
    history = History(validate_observations(rows, stage))
    end = END if stage == "dev" else HOLDOUT_END
    if history.dates[-1] != end:
        raise ValueError("insufficient common-day coverage")
    prepared = indicators(history)
    summary = {
        "stage": stage,
        "n_cumulative": 5,
        "actual_scenarios": [
            {"scenario": name, "k": k, "c": c, "f": f} for name, k, c, f in SCENARIOS
        ],
        "report_tables": {},
    }
    periods = (
        (("main", date(2010, 1, 4)), ("recent", date(2019, 11, 1)))
        if stage == "dev"
        else (("holdout", date(2024, 11, 1)),)
    )
    all_days = []
    for period, start in periods:
        reports = []
        for scenario in SCENARIOS:
            days, report = simulate(history, prepared, period, start, end, scenario)
            all_days.extend(days)
            reports.append(report)
            if scenario[0] == "verdict":
                summary[period] = report
                if period == "main":
                    returns = [row["R"] for row in days[1:]]
                    probability = None if report["status"] == "ruined" else psr(returns)
                    summary.update(
                        psr_main=probability, psr_t=len(returns), psr_threshold=0.99
                    )
        summary["report_tables"][period] = {
            "multipliers": [row[0] for row in SCENARIOS[:5]],
            "costs": ["zero", "verdict", "double", "high"],
            "rows": reports,
        }
    summary["vintage"] = vintages(history, prepared, stage)
    if stage == "dev":
        summary.update(
            gate_a=summary["main"]["status"] == "ok"
            and summary["main"]["W"] > summary["main"]["B"],
            gate_b=summary["psr_main"] is not None and summary["psr_main"] >= 0.99,
            gate_c=summary["recent"]["status"] == "ok"
            and summary["recent"]["W"] > summary["recent"]["B"],
        )
    else:
        summary["gate_h"] = (
            summary["holdout"]["status"] == "ok"
            and summary["holdout"]["W"] > summary["holdout"]["B"]
        )
    return prepared, all_days, summary


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def verify_manifest(directory: Path) -> str:
    path = directory / "SHA256SUMS"
    lines = path.read_text().splitlines()
    expected = []
    for name in RESULT_FILES:
        expected.append(f"{digest(directory / name)}  {name}")
    if lines != expected or path.read_text() != "".join(
        line + "\n" for line in expected
    ):
        raise ValueError("invalid or mismatching dev results manifest")
    return digest(path)


def raw_hashes(fx_dir: Path | None, rates: Path | None, spy: Path | None) -> dict:
    if fx_dir is None or rates is None or spy is None:
        raise ValueError("holdout requires original input paths")
    return {
        **{f"{series}.csv": digest(fx_dir / f"{series}.csv") for series in DEX},
        "OECD.csv": digest(rates),
        "SPY.json": digest(spy),
    }


def require_holdout(
    dev_match: Path | None,
    ledger: Path | None,
    fx_dir: Path | None,
    rates: Path | None,
    spy: Path | None,
) -> None:
    if dev_match is None or ledger is None:
        raise ValueError("holdout requires dev match envelope and ledger")
    envelope = json.loads(dev_match.read_text())
    if not isinstance(envelope, dict) or envelope.get("match") is not True:
        raise ValueError("dev independent recomputation must match")
    results_digest = verify_manifest(dev_match.parent)
    if envelope.get("results_sha256") != results_digest:
        raise ValueError("dev envelope results hash mismatch")
    actual_summary = json.loads((dev_match.parent / "summary.json").read_text())
    for key in ("rust_summary", "python_summary"):
        summary = envelope.get(key)
        if (
            not isinstance(summary, dict)
            or summary.get("stage") != "dev"
            or any(
                summary.get(gate) is not True for gate in ("gate_a", "gate_b", "gate_c")
            )
        ):
            raise ValueError("holdout requires passing dev gates")
        if (key == "rust_summary" and actual_summary != summary) or compare_tree(
            actual_summary, summary
        ):
            raise ValueError("dev envelope summary mismatch")
    records = [
        json.loads(line) for line in ledger.read_text().splitlines() if line.strip()
    ]
    if any(not isinstance(row, dict) for row in records):
        raise ValueError("invalid ledger record")
    records = [row for row in records if row.get("hypothesis") == "H005"]
    if any(
        row.get("kind") == "run" and row.get("stage") == "holdout" for row in records
    ):
        raise ValueError("holdout already consumed")
    runs = [
        (index, row)
        for index, row in enumerate(records)
        if row.get("kind") == "run" and row.get("stage") == "dev"
    ]
    if not runs:
        raise ValueError("holdout requires matching ledger dev run")
    run_index, run = runs[-1]
    if run.get("results_sha256") != results_digest or run.get(
        "data_sha256"
    ) != raw_hashes(fx_dir, rates, spy):
        raise ValueError("holdout raw input or dev results hash mismatch")
    verdicts = [
        (index, row)
        for index, row in enumerate(records)
        if index > run_index
        and row.get("kind") == "verdict"
        and row.get("stage") == "dev"
    ]
    if not verdicts:
        raise ValueError("holdout requires matching ledger passing verdict")
    verdict_index, verdict = verdicts[-1]
    if (
        verdict.get("results_sha256") != results_digest
        or verdict.get("result") != "pass"
        or not isinstance(verdict.get("gates"), dict)
        or any(verdict["gates"].get(gate) is not True for gate in ("A", "B", "C"))
    ):
        raise ValueError("holdout requires passing ledger gates and results")
    if not any(
        index > verdict_index
        and row.get("kind") == "holdout_open"
        and row.get("stage") == "holdout"
        for index, row in enumerate(records)
    ):
        raise ValueError("holdout must be explicitly opened after dev verdict")


def compare_tree(expected: object, actual: object, label: str = "summary") -> list[str]:
    differences = []
    if isinstance(expected, dict):
        if not isinstance(actual, dict) or expected.keys() != actual.keys():
            return [f"{label}: fields differ"]
        for key, value in expected.items():
            differences.extend(compare_tree(value, actual[key], f"{label}.{key}"))
    elif isinstance(expected, list):
        if not isinstance(actual, list) or len(expected) != len(actual):
            return [f"{label}: row count differs"]
        for index, (left, right) in enumerate(zip(expected, actual, strict=True)):
            differences.extend(compare_tree(left, right, f"{label}[{index}]"))
    elif isinstance(expected, (float, int)) and not isinstance(expected, bool):
        exact = label.endswith("signal")
        tolerance = 0.0 if exact else 1e-9 if label.endswith("psr_main") else 1e-10
        scale = (
            1.0
            if label.endswith("psr_main")
            else max(1.0, abs(expected), abs(actual))
            if finite(actual)
            else 1.0
        )
        if not finite(actual) or abs(expected - actual) > tolerance * scale:
            differences.append(f"{label}: mismatch")
    elif type(expected) is not type(actual) or expected != actual:
        differences.append(f"{label}: mismatch")
    return differences


def run(
    directory: Path,
    stage: str = "dev",
    dev_match: Path | None = None,
    ledger: Path | None = None,
    fx_dir: Path | None = None,
    rates: Path | None = None,
    spy: Path | None = None,
    first_release_dir: Path | None = None,
) -> tuple[Path, str, bool]:
    if stage == "holdout":
        require_holdout(dev_match, ledger, fx_dir, rates, spy)
    directory = checked_run_directory(directory)
    result_digest = verify_manifest(directory)
    prepared, days, summary = compute(
        read_observations(directory / "observations.parquet", stage), stage
    )
    rust_summary = json.loads((directory / "summary.json").read_text())
    differences = compare_tree(summary, rust_summary)
    for name, expected in (("indicators", prepared), ("days", days)):
        observed = pq.read_table(directory / f"{name}.parquet").to_pylist()
        for row in observed:
            if type(row.get("date")) is date:
                row["date"] = row["date"].isoformat()
        differences.extend(compare_tree(expected, observed, name))
    verdict = {
        "python_summary": summary,
        "rust_summary": rust_summary,
        "match": not differences,
        "mismatches": differences,
        "results_sha256": result_digest,
    }
    path, sha = write_verdict(directory, verdict)
    return path, sha, not differences


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--run", required=True, type=Path)
    parser.add_argument("--stage", required=True, choices=("dev", "holdout"))
    for name in ("dev-match", "ledger", "fx-dir", "rates", "spy", "first-release-dir"):
        parser.add_argument(f"--{name}", type=Path)
    args = parser.parse_args()
    try:
        path, sha, matched = run(
            args.run,
            args.stage,
            args.dev_match,
            args.ledger,
            args.fx_dir,
            args.rates,
            args.spy,
            args.first_release_dir,
        )
    except (OSError, ValueError, TypeError, KeyError, ArithmeticError) as error:
        print(f"H005 input or computation failed: {error}", file=sys.stderr)
        return 1
    print(f"{path} sha256={sha} match={str(matched).lower()}")
    return 0 if matched else 1


if __name__ == "__main__":
    sys.exit(main())
