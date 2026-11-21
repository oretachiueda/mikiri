"""Independent S002 computations."""

import bisect
import csv
import io
import json
import math
import statistics
import zipfile
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


def return_calendar(fred: dict[str, dict[date, float | None]]) -> list[date]:
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
        public = day + timedelta(days=6)
    index = bisect.bisect_left(calendar, public)
    if index == len(calendar):
        raise ValueError("calendar ends before public date")
    public = calendar[index]
    return public, calendar[index + 1] if index + 1 < len(calendar) else None


def z_values(
    reports: list[tuple[date, float]], window: int
) -> dict[date, float | None]:
    if window not in (52, 156):
        raise ValueError("unsupported window")
    slots = latest_slots(reports)
    result: dict[date, float | None] = {}
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
