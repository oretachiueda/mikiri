"""Independent S002 computations."""

import bisect
import csv
import io
import itertools
import json
import math
import statistics
import zipfile
from collections.abc import Mapping, Sequence
from datetime import date, timedelta
from pathlib import Path


def d_offsets(t: int) -> list[int]:
    """Generate the 1,000 distinct preregistered circular shifts."""
    if t - 503 < 1000:
        raise ValueError("calendar too short for 1,000 distinct offsets")
    mask = (1 << 64) - 1
    state = 2026112101
    seen: set[int] = set()
    offsets = []
    while len(offsets) < 1000:
        state = (state + 0x9E3779B97F4A7C15) & mask
        z = state
        z = ((z ^ (z >> 30)) * 0xBF58476D1CE4E5B9) & mask
        z = ((z ^ (z >> 27)) * 0x94D049BB133111EB) & mask
        offset = 252 + ((z ^ (z >> 31)) % (t - 503))
        if offset not in seen:
            seen.add(offset)
            offsets.append(offset)
    return offsets


CUTOFF = date(2024, 10, 31)
START = date(1992, 10, 6)
EPOCH = date(1992, 10, 3)
CODES = {
    "097741": "JPY",
    "099741": "EUR",
    "096742": "GBP",
    "092741": "CHF",
    "090741": "CAD",
    "232741": "AUD",
    "112741": "NZD",
    "095741": "MXN",
}
FX = (
    "DEXJPUS",
    "DEXUSEU",
    "DEXUSUK",
    "DEXSZUS",
    "DEXCAUS",
    "DEXUSAL",
    "DEXUSNZ",
    "DEXMXUS",
)


def read_cot(directory: Path) -> dict[str, list[tuple[date, float]]]:
    reports: dict[str, list[tuple[date, float]]] = {name: [] for name in CODES.values()}
    seen: set[tuple[str, date]] = set()
    for path in sorted(directory.glob("*.zip")):
        with zipfile.ZipFile(path) as archive:
            members = [
                name for name in archive.namelist() if name.lower().endswith(".txt")
            ]
            if len(members) != 1:
                raise ValueError(f"expected one txt in {path.name}")
            with archive.open(members[0]) as raw:
                reader = csv.DictReader(
                    io.TextIOWrapper(raw, encoding="latin-1", newline="")
                )
                if reader.fieldnames is None:
                    raise ValueError("missing COT header")
                reader.fieldnames = [name.strip() for name in reader.fieldnames]
                required = {
                    "Market and Exchange Names",
                    "As of Date in Form YYYY-MM-DD",
                    "CFTC Contract Market Code",
                    "Commercial Positions-Long (All)",
                    "Commercial Positions-Short (All)",
                }
                if not required.issubset(reader.fieldnames):
                    raise ValueError("missing COT columns")
                for row in reader:
                    code = row["CFTC Contract Market Code"].strip()
                    if code not in CODES:
                        continue
                    day = date.fromisoformat(
                        row["As of Date in Form YYYY-MM-DD"].strip()
                    )
                    if (
                        day > CUTOFF
                        or day < START
                        or (code == "099741" and day < date(1999, 1, 1))
                    ):
                        continue
                    key = (code, day)
                    if key in seen:
                        raise ValueError(f"duplicate COT report: {code} {day}")
                    seen.add(key)
                    long = float(row["Commercial Positions-Long (All)"])
                    short = float(row["Commercial Positions-Short (All)"])
                    if (
                        not math.isfinite(long)
                        or not math.isfinite(short)
                        or long < 0
                        or short < 0
                        or long + short <= 0
                    ):
                        raise ValueError("invalid COT positions")
                    reports[CODES[code]].append((day, (short - long) / (short + long)))
    for rows in reports.values():
        rows.sort()
    return reports


def read_fred(path: Path) -> dict[date, float | None]:
    series = path.stem
    values: dict[date, float | None] = {}
    with path.open(newline="") as stream:
        reader = csv.reader(stream)
        if next(reader, None) != ["observation_date", series]:
            raise ValueError(f"invalid FRED header: {path.name}")
        previous = None
        for row in reader:
            if len(row) != 2:
                raise ValueError("invalid FRED row")
            day = date.fromisoformat(row[0])
            if day > CUTOFF:
                continue
            if previous is not None and day <= previous:
                raise ValueError("FRED dates not strictly increasing")
            previous = day
            value = float(row[1]) if row[1] else None
            if value is not None and (
                not math.isfinite(value) or (series in FX and value <= 0)
            ):
                raise ValueError("invalid FRED value")
            values[day] = value
    return values


def read_spy(path: Path) -> dict[date, float]:
    values: dict[date, float] = {}
    previous = None
    for row in json.loads(path.read_text()):
        day = date.fromisoformat(row["date"][:10])
        if day > CUTOFF:
            continue
        if previous is not None and day <= previous:
            raise ValueError("SPY dates not strictly increasing")
        previous = day
        value = float(row["adjClose"])
        if not math.isfinite(value) or value <= 0:
            raise ValueError("invalid SPY price")
        values[day] = value
    return values


def return_calendar(fred: Mapping[str, Mapping[date, float | None]]) -> list[date]:
    return sorted(
        {day for name in FX for day, value in fred[name].items() if value is not None}
    )


def slot(day: date) -> int:
    return (day - EPOCH).days // 7


def latest_slots(reports: list[tuple[date, float]]) -> dict[int, tuple[date, float]]:
    result: dict[int, tuple[date, float]] = {}
    for day, value in reports:
        index = slot(day)
        if index not in result or day > result[index][0]:
            result[index] = (day, value)
    return result


def public_and_signal(day: date, calendar: list[date]) -> tuple[date, date | None]:
    if date(2013, 10, 1) <= day <= date(2013, 11, 19):
        public = date(2013, 11, 29)
    elif date(2018, 12, 24) <= day <= date(2019, 3, 12):
        public = date(2019, 3, 29)
    else:
        public = day + timedelta(days=6 if day.weekday() == 1 else 14)
    index = bisect.bisect_left(calendar, public)
    if index == len(calendar):
        raise ValueError("calendar ends before public date")
    public = calendar[index]
    return public, calendar[index + 1] if index + 1 < len(calendar) else None


def z_values(
    reports: list[tuple[date, float]], window: int
) -> Mapping[date, float | None]:
    if window not in (52, 156):
        raise ValueError("unsupported window")
    slots = latest_slots(reports)
    result: Mapping[date, float | None] = {}
    for index, (day, value) in sorted(slots.items()):
        indices = range(index - window + 1, index + 1)
        if not all(i in slots for i in indices):
            result[day] = None
            continue
        values = [slots[i][1] for i in indices]
        deviation = statistics.stdev(values)
        if not math.isfinite(deviation) or deviation <= 0:
            raise ValueError("invalid z window standard deviation")
        result[day] = (value - statistics.mean(values)) / deviation
    return result


LEGS = tuple(CODES.values())
USD_BASE = {"JPY", "CHF", "CAD", "MXN"}
RATE_CODES = {
    "JPY": "JP",
    "EUR": "EZ",
    "GBP": "GB",
    "CHF": "CH",
    "CAD": "CA",
    "AUD": "AU",
    "NZD": "NZ",
    "MXN": "MX",
}


def direction(value: float | None, signal: str, band: str, leg: str) -> int:
    if value is None or abs(value) <= (
        0 if band == "b0" else 0.1 if signal == "lvl" else 1.0
    ):
        return 0
    foreign = 1 if value > 0 else -1
    return -foreign if leg in USD_BASE else foreign


def fix_returns(prices: Mapping[date, float | None], day: date) -> dict[date, float]:
    fixes = [(d, p) for d, p in sorted(prices.items()) if d < day and p is not None]
    return {
        d: math.log(p / previous) for (_, previous), (d, p) in itertools.pairwise(fixes)
    }


def volatility(values: list[float]) -> float | None:
    if len(values) < 60:
        return None
    sigma = statistics.stdev(values[-60:]) * math.sqrt(252)
    if not math.isfinite(sigma) or sigma <= 0:
        raise ValueError("invalid volatility")
    return sigma


def size_positions(
    day: date,
    directions: dict[str, int],
    prices: Mapping[str, Mapping[date, float | None]],
    universe: str,
) -> dict[str, float]:
    returns = {leg: fix_returns(prices[leg], day) for leg, q in directions.items() if q}
    sigmas = {leg: volatility(list(series.values())) for leg, series in returns.items()}
    active = {
        leg: directions[leg] / sigma
        for leg, sigma in sigmas.items()
        if sigma is not None
    }
    sizes = {leg: 0.0 for leg in directions}
    if not active:
        return sizes
    if universe == "jpy":
        sizes["JPY"] = min(10.0, 0.30 * abs(active["JPY"]))
        return sizes
    common = sorted(set.intersection(*(set(returns[leg]) for leg in active)))
    portfolio = [
        sum(weight * returns[leg][d] for leg, weight in active.items()) for d in common
    ]
    sigma_p = volatility(portfolio)
    if sigma_p is None:
        return sizes
    raw = {leg: 0.30 / sigma_p * abs(weight) for leg, weight in active.items()}
    scale = min(1.0, 10.0 / sum(raw.values()))
    sizes.update({leg: value * scale for leg, value in raw.items()})
    return sizes


def has_rate(rates: Mapping[date, float | None], day: date) -> bool:
    month = day.year * 12 + day.month - 1 - 2
    return any(
        value is not None and d.year * 12 + d.month - 1 <= month
        for d, value in rates.items()
    )


def signals(
    config: str,
    reports: dict[str, list[tuple[date, float]]],
    calendar: list[date],
    prices: Mapping[str, Mapping[date, float | None]],
    rates: Mapping[str, Mapping[date, float | None]],
    start: date = date(1996, 1, 2),
    raw: bool = False,
) -> list[tuple[date, dict[str, date | None], dict[str, int], dict[str, float]]]:
    signal, band, holding, universe = config.split("-")
    if (
        signal not in ("lvl", "z52", "z156")
        or band not in ("b0", "b1")
        or holding not in ("h1", "h2")
        or universe not in ("jpy", "fx8")
    ):
        raise ValueError("invalid config")
    legs = ("JPY",) if universe == "jpy" else LEGS
    events: dict[date, list[tuple[date, str, float | None]]] = {}
    for leg in legs:
        rows = sorted(latest_slots(reports[leg]).values())
        zs = {} if signal == "lvl" else z_values(rows, int(signal[1:]))
        usable = []
        for asof, hp in rows:
            if holding == "h2" and slot(asof) % 2:
                continue
            try:
                public, signal_day = public_and_signal(asof, calendar)
            except ValueError:
                continue
            if signal_day is not None:
                usable.append(
                    (asof, public, signal_day, hp if signal == "lvl" else zs[asof])
                )
        for asof, public, signal_day, value in usable:
            events.setdefault(signal_day, []).append((asof, leg, value))
            expiry = public + timedelta(days=14)
            if any(
                public <= other_public <= expiry
                and (other_public > public or other_asof > asof)
                for other_asof, other_public, _, _ in usable
            ):
                continue
            at = bisect.bisect_left(calendar, expiry)
            if at < len(calendar):
                events.setdefault(calendar[at], []).append((asof, leg, None))
    current = dict.fromkeys(legs, 0)
    output = []
    for day, changes in sorted(events.items()):
        if day < start:
            continue
        acted = set()
        for _, leg, value in sorted(changes):
            current[leg] = direction(value, signal, band, leg)
            acted.add(leg)
        active = {
            leg: q if has_rate(rates[leg], day) and has_rate(rates["USD"], day) else 0
            for leg, q in current.items()
        }
        sizes = size_positions(day, active, prices, universe)
        fills = {}
        result_direction = {}
        result_size = {}
        for leg in sorted(acted):
            fixes = sorted(
                d for d, p in prices[leg].items() if d > day and p is not None
            )
            fills[leg] = fixes[0] if fixes else None
            result_size[leg] = sizes[leg]
            result_direction[leg] = (
                current[leg] if raw else active[leg] if sizes[leg] else 0
            )
        output.append((day, fills, result_direction, result_size))
    return output


SPREAD = {
    "JPY": 0.002,
    "EUR": 0.00004,
    "GBP": 0.0003,
    "CHF": 0.0003,
    "CAD": 0.0003,
    "AUD": 0.00006,
    "NZD": 0.0003,
    "MXN": 0.003,
}


def rate_for_day(rates: Mapping[date, float | None], day: date) -> float:
    month = day.year * 12 + day.month - 1 - 2
    available = [
        (d, value)
        for d, value in rates.items()
        if d.year * 12 + d.month - 1 <= month and value is not None
    ]
    if not available:
        raise ValueError(f"missing rate for {day}")
    return max(available, key=lambda row: row[0])[1] / 100


def simulate_returns(
    calendar: list[date],
    actions: Sequence[
        tuple[date, Mapping[str, date | None], Mapping[str, int], Mapping[str, float]]
    ],
    prices: Mapping[str, Mapping[date, float | None]],
    rates: Mapping[str, Mapping[date, float | None]],
    start: date = date(1996, 1, 2),
    tax: bool = False,
    costs: bool = True,
    details: dict[str, float] | None = None,
) -> list[tuple[date, float, float]]:
    from h005 import TaxLedger

    ledger = TaxLedger()
    year_start = 1.0
    fills: dict[date, dict[str, tuple[int, float]]] = {}
    for signal_day, dates, directions, sizes in sorted(
        actions, key=lambda action: action[0]
    ):
        for leg, fill_day in dates.items():
            if fill_day is not None:
                if fill_day <= signal_day:
                    raise ValueError("fill must follow signal")
                fills.setdefault(fill_day, {})[leg] = (directions[leg], sizes[leg])
    positions = {leg: 0.0 for leg in prices}
    previous_fix: dict[str, tuple[date, float]] = {}
    equity = 1.0
    peak = 1.0
    trades = max_gross = max_dd = 0.0
    result = []
    active_calendar = [day for day in calendar if day >= start]
    for i, day in enumerate(active_calendar):
        pnl = 0.0
        for leg, series in prices.items():
            price = series.get(day)
            if price is None:
                continue
            prior = previous_fix.get(leg)
            notional = positions[leg]
            if prior is not None and notional:
                previous_day, previous_price = prior
                pnl += notional * (price / previous_price - 1)
                held = "USD" if (notional > 0) == (leg in USD_BASE) else leg
                sold = leg if held == "USD" else "USD"
                for offset in range(1, (day - previous_day).days + 1):
                    accrual_day = previous_day + timedelta(days=offset)
                    pnl += (
                        abs(notional)
                        * (
                            rate_for_day(rates[held], accrual_day)
                            - rate_for_day(rates[sold], accrual_day)
                            - ((0.02 if leg == "MXN" else 0.0135) if costs else 0.0)
                        )
                        / 365
                    )
            if leg in fills.get(day, {}):
                q, leverage = fills[day][leg]
                new_notional = q * leverage * equity
                if new_notional != notional:
                    trades += 1
                if costs:
                    pnl -= SPREAD[leg] / (2 * price) * abs(new_notional - notional)
                positions[leg] = new_notional
            previous_fix[leg] = (day, price)
        max_gross = max(max_gross, sum(abs(v) for v in positions.values()) / equity)
        daily_return = pnl / equity
        equity += pnl
        if not math.isfinite(equity) or equity <= 0:
            raise ValueError(f"bust on {day}")
        if tax and (
            i + 1 == len(active_calendar) or active_calendar[i + 1].year != day.year
        ):
            ledger.add(day.year, equity - year_start)
            equity -= ledger.settle(day.year)
            if equity <= 0:
                raise ValueError(f"bust on {day}")
            year_start = equity
        peak = max(peak, equity)
        max_dd = max(max_dd, 1 - equity / peak)
        result.append((day, daily_return, equity))
    if details is not None:
        details.update(trades=trades, max_gross=max_gross, max_dd=max_dd)
    return result


def tax_wealth(days: list[date], returns: list[float]) -> float:
    if len(days) != len(returns):
        raise ValueError("calendar and returns differ in length")
    equity = year_start = 1.0
    losses: list[tuple[int, float]] = []
    for i, (day, daily_return) in enumerate(zip(days, returns, strict=True)):
        equity *= 1 + daily_return
        if not math.isfinite(equity) or equity <= 0:
            raise ValueError("bust")
        if i + 1 == len(days) or days[i + 1].year != day.year:
            income = equity - year_start
            losses = [(year, amount) for year, amount in losses if day.year - year <= 3]
            if income < 0:
                losses.append((day.year, -income))
            else:
                remaining = []
                for year, amount in losses:
                    used = min(income, amount)
                    income -= used
                    if amount > used:
                        remaining.append((year, amount - used))
                losses = remaining
                equity -= income * 0.20315
            year_start = equity
    return equity


def benchmark_wealth(
    days: list[date], spy: Mapping[date, float], jpy: Mapping[date, float | None]
) -> float:
    spy_rows = sorted(spy.items())
    jpy_rows = sorted((day, value) for day, value in jpy.items() if value is not None)
    levels = []
    for day in (days[0], days[-1]):
        si = bisect.bisect_right(spy_rows, (day, float("inf"))) - 1
        ji = bisect.bisect_right(jpy_rows, (day, float("inf"))) - 1
        if si < 0 or ji < 0:
            raise ValueError("benchmark unavailable")
        levels.append(spy_rows[si][1] * jpy_rows[ji][1])
    return levels[1] / levels[0]


def sharpe(returns: list[float]) -> float:
    sd = statistics.stdev(returns)
    if not math.isfinite(sd) or sd <= 0:
        raise ValueError("invalid return standard deviation")
    return statistics.mean(returns) / sd * math.sqrt(252)


def probability_sharpe(returns: list[float], reference_daily: float = 0.0) -> float:
    from statistics import NormalDist

    n = len(returns)
    daily = sharpe(returns) / math.sqrt(252)
    mean = statistics.mean(returns)
    m2 = sum((x - mean) ** 2 for x in returns) / n
    skew = sum((x - mean) ** 3 for x in returns) / n / m2**1.5
    kurtosis = sum((x - mean) ** 4 for x in returns) / n / m2**2
    denominator = 1 - skew * daily + (kurtosis - 1) * daily**2 / 4
    if denominator <= 0 or not math.isfinite(denominator):
        raise ValueError("invalid PSR denominator")
    return NormalDist().cdf(
        (daily - reference_daily) * math.sqrt(n - 1) / math.sqrt(denominator)
    )


def deflated_sharpe(
    winner: list[float], all_returns: list[list[float]]
) -> tuple[float, float, float]:
    from statistics import NormalDist

    if len(all_returns) != 24:
        raise ValueError("expected 24 configurations")
    variance = statistics.variance(
        sharpe(values) / math.sqrt(252) for values in all_returns
    )
    normal = NormalDist()
    gamma = 0.5772156649
    sr0 = math.sqrt(variance) * (
        (1 - gamma) * normal.inv_cdf(1 - 1 / 24)
        + gamma * normal.inv_cdf(1 - 1 / (24 * math.e))
    )
    return probability_sharpe(winner, sr0), sr0, variance


def winning_config(paths: Mapping[str, list[float]]) -> str:
    return min(paths, key=lambda name: (-sharpe(paths[name]), name))


def shift_signal_days(
    calendar: list[date],
    actions: Sequence[
        tuple[date, Mapping[str, date | None], Mapping[str, int], Mapping[str, float]]
    ],
    offset: int,
) -> list[tuple[date, dict[str, int]]]:
    shifted: dict[date, dict[str, int]] = {}
    for signal_day, _, directions, _ in sorted(actions, key=lambda row: row[0]):
        index = bisect.bisect_left(calendar, signal_day)
        if index == len(calendar):
            raise ValueError("signal outside calendar")
        day = calendar[(index + offset) % len(calendar)]
        shifted.setdefault(day, {}).update(directions)
    return sorted(shifted.items())


def shifted_actions(
    config: str,
    raw_actions: Sequence[
        tuple[date, Mapping[str, date | None], Mapping[str, int], Mapping[str, float]]
    ],
    offset: int,
    calendar: list[date],
    reports: dict[str, list[tuple[date, float]]],
    prices: Mapping[str, Mapping[date, float | None]],
    rates: Mapping[str, Mapping[date, float | None]],
    market_calendar: list[date] | None = None,
) -> list[tuple[date, dict[str, date | None], dict[str, int], dict[str, float]]]:
    signal, _, _, universe = config.split("-")
    legs = ("JPY",) if universe == "jpy" else LEGS
    availability: dict[str, list[tuple[date, date, bool]]] = {}
    for leg in legs:
        rows = sorted(latest_slots(reports[leg]).values())
        zs = {} if signal == "lvl" else z_values(rows, int(signal[1:]))
        history = []
        for asof, _ in rows:
            try:
                public, usable = public_and_signal(
                    asof, market_calendar if market_calendar is not None else calendar
                )
            except ValueError:
                continue
            if usable is not None:
                history.append(
                    (usable, public, signal == "lvl" or zs[asof] is not None)
                )
        availability[leg] = sorted(history)
    current = dict.fromkeys(legs, 0)
    result = []
    for day, updates in shift_signal_days(calendar, raw_actions, offset):
        for leg, q in updates.items():
            history = [entry for entry in availability[leg] if entry[0] <= day]
            active = bool(
                history
                and history[-1][2]
                and day < history[-1][1] + timedelta(days=14)
                and has_rate(rates[leg], day)
                and has_rate(rates["USD"], day)
            )
            current[leg] = q if active else 0
        sizes = size_positions(day, current, prices, universe)
        fills = {}
        directions = {}
        leverage = {}
        for leg in updates:
            fixes = sorted(
                d for d, value in prices[leg].items() if d > day and value is not None
            )
            fills[leg] = fixes[0] if fixes else None
            directions[leg] = current[leg] if sizes[leg] else 0
            leverage[leg] = sizes[leg]
        result.append((day, fills, directions, leverage))
    return result


def d_control(
    config: str,
    raw_actions: Sequence[
        tuple[date, Mapping[str, date | None], Mapping[str, int], Mapping[str, float]]
    ],
    calendar: list[date],
    reports: dict[str, list[tuple[date, float]]],
    prices: Mapping[str, Mapping[date, float | None]],
    rates: Mapping[str, Mapping[date, float | None]],
    actual_sr: float,
    market_calendar: list[date] | None = None,
) -> tuple[int, list[int]]:
    offsets = d_offsets(len(calendar))
    count = 0
    for offset in offsets:
        actions = shifted_actions(
            config,
            raw_actions,
            offset,
            calendar,
            reports,
            prices,
            rates,
            market_calendar,
        )
        path = simulate_returns(calendar, actions, prices, rates, calendar[0])
        count += sharpe([daily for _, daily, _ in path]) < actual_sr
    return count, offsets[:10]


def dev_judgment(
    paths: Mapping[str, list[float]],
    wealth: Mapping[str, tuple[float, float]],
    b_dev: float,
    b_c: float,
) -> tuple[str, dict[str, bool], dict[str, float]]:
    if len(paths) != 24 or paths.keys() != wealth.keys():
        raise ValueError("expected 24 matching configurations")
    if len({len(path) for path in paths.values()}) != 1:
        raise ValueError("dev paths must share a calendar")
    winner = winning_config(paths)
    psr = probability_sharpe(paths[winner])
    dsr, sr0, variance = deflated_sharpe(paths[winner], list(paths.values()))
    w_dev, w_c = wealth[winner]
    gates = {
        "A": w_dev > b_dev,
        "B1": psr >= 1 - 0.05 / 276,
        "B2": dsr >= 0.95,
        "C": w_c > b_c,
    }
    values = {
        "W_dev": w_dev,
        "B_dev": b_dev,
        "W_C": w_c,
        "B_C": b_c,
        "psr": psr,
        "psr_bar": 1 - 0.05 / 276,
        "dsr": dsr,
        "sr0_daily": sr0,
        "V": variance,
    }
    return winner, gates, values


def judgment_gates(
    w_dev: float,
    b_dev: float,
    w_c: float,
    b_c: float,
    psr: float,
    dsr: float,
    shifted_srs: Sequence[float],
    actual_sr: float,
) -> dict[str, bool]:
    if len(shifted_srs) != 1000 or any(
        not math.isfinite(value) for value in shifted_srs
    ):
        raise ValueError("D requires 1,000 finite shifted Sharpe ratios")
    return {
        "A": w_dev > b_dev,
        "B1": psr >= 1 - 0.05 / 276,
        "B2": dsr >= 0.95,
        "C": w_c > b_c,
        "D": sum(value < actual_sr for value in shifted_srs) >= 950,
    }


def load_inputs(
    cot: Path, fred: Path, spy: Path
) -> tuple[
    dict[str, list[tuple[date, float]]],
    dict[str, dict[date, float | None]],
    dict[str, dict[date, float | None]],
    dict[date, float],
    dict[str, str],
]:
    import hashlib

    reports = read_cot(cot)
    files = sorted(cot.glob("*.zip"))
    if not files:
        raise ValueError("no COT zip files")
    prices = {}
    for leg, series in zip(LEGS, FX, strict=True):
        path = fred / f"{series}.csv"
        files.append(path)
        prices[leg] = read_fred(path)
    rates = {}
    for leg, code in {"USD": "US", **RATE_CODES}.items():
        path = fred / f"IRSTCI01{code}M156N.csv"
        files.append(path)
        rates[leg] = read_fred(path)
    files.append(spy)
    spy_values = read_spy(spy)
    hashes = {
        str(path): hashlib.sha256(path.read_bytes()).hexdigest() for path in files
    }
    return reports, prices, rates, spy_values, hashes


def run_dev(cot: Path, fred: Path, spy: Path, out: Path) -> None:
    import itertools
    import subprocess

    reports, prices, rates, spy_values, hashes = load_inputs(cot, fred, spy)
    market_calendar = sorted(
        {
            day
            for series in prices.values()
            for day, value in series.items()
            if value is not None
        }
    )
    calendar = [day for day in market_calendar if date(1996, 1, 2) <= day <= CUTOFF]
    if not calendar or calendar[0] != date(1996, 1, 2):
        raise ValueError("dev calendar must start 1996-01-02")
    c_index = bisect.bisect_left(calendar, date(2019, 11, 1))
    if c_index >= len(calendar):
        raise ValueError("missing C calendar")
    configs = [
        "-".join(parts)
        for parts in itertools.product(
            ("lvl", "z52", "z156"), ("b0", "b1"), ("h1", "h2"), ("jpy", "fx8")
        )
    ]
    paths: dict[str, list[float]] = {}
    wealth: dict[str, tuple[float, float]] = {}
    rows = []
    for config in configs:
        actions = signals(config, reports, market_calendar, prices, rates)
        stats: dict[str, float] = {}
        dev = simulate_returns(calendar, actions, prices, rates, details=stats)
        pre = simulate_returns(calendar, actions, prices, rates, costs=False)
        post_tax = simulate_returns(calendar, actions, prices, rates, tax=True)
        returns = [r for _, r, _ in dev]
        paths[config] = returns
        wealth[config] = (
            post_tax[-1][2],
            tax_wealth(calendar[c_index:], returns[c_index:]),
        )
        years = (calendar[-1] - calendar[0]).days / 365.25
        rows.append(
            {
                "config": config,
                "sr_after": sharpe(returns),
                "sr_before": sharpe([r for _, r, _ in pre]),
                "trades_per_year": stats["trades"] / years,
                "max_gross_L": stats["max_gross"],
                "max_dd": stats["max_dd"],
                "psr": probability_sharpe(returns),
            }
        )
    b_dev = benchmark_wealth(calendar, spy_values, prices["JPY"])
    b_c = benchmark_wealth(calendar[c_index:], spy_values, prices["JPY"])
    winner, gates, values = dev_judgment(paths, wealth, b_dev, b_c)
    raw = signals(winner, reports, market_calendar, prices, rates, raw=True)
    actual_sr = sharpe(paths[winner])
    count, first = d_control(
        winner, raw, calendar, reports, prices, rates, actual_sr, market_calendar
    )
    gates["D"] = count >= 950
    output_values: dict[str, object] = dict(values)
    output_values.update(d_count_below=count, d_offsets_first10=first)
    try:
        root = Path(__file__).resolve().parent.parent
        dirty = subprocess.run(
            ["git", "status", "--porcelain"],
            cwd=root,
            text=True,
            capture_output=True,
            check=True,
        ).stdout
        commit = (
            None
            if dirty
            else subprocess.run(
                ["git", "rev-parse", "HEAD"],
                cwd=root,
                text=True,
                capture_output=True,
                check=True,
            ).stdout.strip()
        )
    except (OSError, subprocess.CalledProcessError):
        commit = None
    scores = [sharpe(path) for path in paths.values()]
    summary = {
        "T": len(calendar),
        "dev_start": calendar[0].isoformat(),
        "dev_end": calendar[-1].isoformat(),
        "winner": winner,
        "winner_sr_after_annual": actual_sr,
        "gates": gates,
        "values": output_values,
        "single_trial_psr": values["psr"],
        "single_trial_bar_pass": values["psr"] >= 0.95,
        "median_sr_after": statistics.median(scores),
        "max_sr_after": max(scores),
        "input_sha256": hashes,
        "code": commit,
    }
    out.mkdir(parents=True, exist_ok=True)
    with (out / "results.csv").open("w", newline="") as stream:
        writer = csv.DictWriter(stream, fieldnames=list(rows[0]))
        writer.writeheader()
        writer.writerows(rows)
    (out / "summary.json").write_text(
        json.dumps(summary, indent=2, allow_nan=False) + "\n"
    )


def main(argv: list[str] | None = None) -> int:
    import argparse

    parser = argparse.ArgumentParser(description="S002 dev computation")
    parser.add_argument("--cot", required=True, type=Path)
    parser.add_argument("--fred", required=True, type=Path)
    parser.add_argument("--spy", required=True, type=Path)
    parser.add_argument("--out", required=True, type=Path)
    parser.add_argument("--holdout", action="store_true")
    args = parser.parse_args(argv)
    if args.holdout:
        parser.error("holdout is not implemented and cannot be run")
    run_dev(args.cot, args.fred, args.spy, args.out)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
