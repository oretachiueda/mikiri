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
        public = day + timedelta(days=6 if day.weekday() == 1 else 7)
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
        for i, (asof, public, signal_day, value) in enumerate(usable):
            events.setdefault(signal_day, []).append((asof, leg, value))
            expiry = public + timedelta(days=14)
            if i + 1 < len(usable) and usable[i + 1][1] <= expiry:
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
            result_direction[leg] = active[leg] if sizes[leg] else 0
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
) -> list[tuple[date, float, float]]:
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
    result = []
    for day in calendar:
        if day < start:
            continue
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
                            - (0.02 if leg == "MXN" else 0.0135)
                        )
                        / 365
                    )
            if leg in fills.get(day, {}):
                q, leverage = fills[day][leg]
                new_notional = q * leverage * equity
                pnl -= SPREAD[leg] / (2 * price) * abs(new_notional - notional)
                positions[leg] = new_notional
            previous_fix[leg] = (day, price)
        daily_return = pnl / equity
        equity += pnl
        if not math.isfinite(equity) or equity <= 0:
            raise ValueError(f"bust on {day}")
        result.append((day, daily_return, equity))
    return result
