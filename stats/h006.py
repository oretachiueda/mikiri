"""Independently recompute the H006 EUR/USD two-leg session preregistration.

Unit 1: HistData zip input, zoneinfo clock handling, outlier cleaning, the
R1 clock check, and computation days with boundary minutes and prices.
Unit 2: the S3 stop walk per leg and the per-day table (sigma, u, sigma_hat,
leverage, R) with the days.parquet column layout.
"""

import hashlib
import io
import math
import zipfile
from dataclasses import dataclass
from datetime import date, datetime, timedelta
from pathlib import Path
from zoneinfo import ZoneInfo

import numpy as np
import pandas as pd

NY = ZoneInfo("America/New_York")
DUB = ZoneInfo("Europe/Dublin")
JST = ZoneInfo("Asia/Tokyo")
FIRST_DAY = date(2007, 1, 2)
ZIP_PREFIX = "HISTDATA_COM_ASCII_EURUSD_M1_"
YEAR_ZIPS = tuple(f"{ZIP_PREFIX}{year}.zip" for year in range(2007, 2026))
MONTH_ZIPS = tuple(f"{ZIP_PREFIX}2026{month:02d}.zip" for month in range(1, 11))
ALL_ZIPS = YEAR_ZIPS + MONTH_ZIPS
DEV_ZIPS = tuple(f"{ZIP_PREFIX}{year}.zip" for year in range(2007, 2025))
CUTOFFS = {"dev": b"20241031", "holdout": b"20261030"}
STAGE_ZIPS = {"dev": DEV_ZIPS, "holdout": ALL_ZIPS}
CSV_COLUMNS = ("stamp", "open", "high", "low", "close", "volume")
STAMP_PATTERN = r"\d{8} \d{6}"


@dataclass(frozen=True)
class Bars:
    stamp: np.ndarray
    open: np.ndarray
    high: np.ndarray
    low: np.ndarray
    close: np.ndarray
    sha256: dict[str, str]
    parsed: int


def splitmix64(values):
    """Vectorized splitmix64 with the seed XOR applied once inside."""
    v = np.asarray(values, dtype=np.uint64) ^ np.uint64(20261115)
    v = v + np.uint64(0x9E3779B97F4A7C15)
    v = (v ^ (v >> np.uint64(30))) * np.uint64(0xBF58476D1CE4E5B9)
    v = (v ^ (v >> np.uint64(27))) * np.uint64(0x94D049BB133111EB)
    return v ^ (v >> np.uint64(31))


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def _zip_text(path: Path, member: str) -> bytes:
    with zipfile.ZipFile(path) as archive:
        if member not in archive.namelist():
            raise ValueError(f"missing CSV member in {path.name}")
        return archive.read(member)


def _parse_zip(name: str, text: bytes, cutoff: bytes) -> dict[str, np.ndarray]:
    lines = [line for line in text.split(b"\n") if line and line[:8] <= cutoff]
    if not lines:
        return {
            "stamp": np.array([], dtype=np.int64),
            **{key: np.array([], dtype=np.float64) for key in CSV_COLUMNS[1:]},
        }
    if any(line.count(b";") != len(CSV_COLUMNS) - 1 for line in lines):
        raise ValueError(f"malformed HistData line in {name}")
    try:
        frame = pd.read_csv(
            io.BytesIO(b"\n".join(lines)),
            sep=";",
            header=None,
            names=list(CSV_COLUMNS),
            dtype=str,
            on_bad_lines="error",
        )
    except ValueError as error:
        raise ValueError(f"malformed HistData line in {name}") from error
    if not frame["stamp"].str.fullmatch(STAMP_PATTERN).all():
        raise ValueError(f"malformed HistData timestamp in {name}")
    stamp = frame["stamp"].str[:8].astype("int64") * 1_000_000 + frame["stamp"].str[
        9:
    ].astype("int64")
    if (stamp % 100 != 0).any():
        raise ValueError(f"bar seconds must be zero in {name}")
    columns = {}
    for column in CSV_COLUMNS[1:]:
        values = pd.to_numeric(frame[column], errors="coerce").to_numpy(np.float64)
        if not np.isfinite(values).all():
            raise ValueError(f"non-finite or malformed value in {name}")
        columns[column] = values
    for column in ("open", "high", "low", "close"):
        if (columns[column] <= 0).any():
            raise ValueError(f"non-positive price in {name}")
    return {"stamp": stamp.to_numpy(), **columns}


def read_histdata(directory: Path, stage: str) -> Bars:
    """Read the stage's HistData zips, dropping post-cutoff lines before parsing."""
    if stage not in STAGE_ZIPS:
        raise ValueError("invalid stage")
    required = STAGE_ZIPS[stage]
    entries = {entry.name for entry in directory.iterdir() if entry.is_file()}
    unknown = sorted(entries - set(ALL_ZIPS))
    if unknown:
        raise ValueError(f"unexpected file in HistData directory: {unknown[0]}")
    missing = sorted(set(required) - entries)
    if missing:
        raise ValueError(f"missing HistData zip: {missing[0]}")
    stamps, opened = [], {}
    pieces = {key: [] for key in CSV_COLUMNS[1:]}
    for name in sorted(required):
        tag = name[len(ZIP_PREFIX) : -len(".zip")]
        text = _zip_text(directory / name, f"DAT_ASCII_EURUSD_M1_{tag}.csv")
        opened[name] = digest(directory / name)
        piece = _parse_zip(name, text, CUTOFFS[stage])
        stamps.append(piece["stamp"])
        for key, values in pieces.items():
            values.append(piece[key])
    stamp = np.concatenate(stamps)
    columns = {key: np.concatenate(values) for key, values in pieces.items()}
    steps = np.diff(stamp)
    if (steps == 0).any():
        raise ValueError("duplicate bar timestamp")
    if (steps < 0).any():
        raise ValueError("bar timestamps out of order")
    return Bars(
        stamp=stamp,
        open=columns["open"],
        high=columns["high"],
        low=columns["low"],
        close=columns["close"],
        sha256=opened,
        parsed=int(stamp.size),
    )


def _calendar_date(value: int) -> date:
    return date(value // 10000, value // 100 % 100, value % 100)


def bar_minutes(stamp: np.ndarray) -> tuple[np.ndarray, np.ndarray, np.ndarray]:
    """Return calendar dates, minutes of day, and absolute wall-clock minutes."""
    day = stamp // 1_000_000
    clock = stamp % 1_000_000
    minute = clock // 10000 * 60 + clock // 100 % 100
    unique = np.unique(day)
    ordinals = np.array(
        [_calendar_date(int(value)).toordinal() for value in unique], dtype=np.int64
    )
    index = np.searchsorted(unique, day)
    return day, minute, ordinals[index] * 1440 + minute


def validate_clock(stamp: np.ndarray) -> None:
    """Every bar must be a real, fold=0 New York wall time that survives NY->JST->NY."""
    day, minute, _ = bar_minutes(stamp)
    for value in np.unique(day):
        observed = _calendar_date(int(value))
        offsets = {
            datetime(
                observed.year, observed.month, observed.day, hour, tzinfo=NY
            ).utcoffset()
            for hour in (0, 6, 12, 18)
        }
        if len(offsets) == 1:
            continue
        for one, minutes in zip(stamp[day == value], minute[day == value], strict=True):
            naive = datetime(
                observed.year,
                observed.month,
                observed.day,
                int(minutes) // 60,
                int(minutes) % 60,
            )
            back = naive.replace(tzinfo=NY).astimezone(JST).astimezone(NY)
            if back.replace(tzinfo=None) != naive:
                raise ValueError(f"bar timestamp is not a New York wall time: {one}")


def eu_open_instant(day: date) -> datetime:
    return datetime(day.year, day.month, day.day, 7, tzinfo=DUB)


def jst_close_instant(day: date) -> datetime:
    following = day + timedelta(days=1)
    return datetime(following.year, following.month, following.day, 3, 45, tzinfo=JST)


def ny_minutes(day: date) -> tuple[int, int]:
    """Dublin 07:00 and next-day JST 03:45 as New York minutes of this date."""
    eu = eu_open_instant(day).astimezone(NY)
    close = jst_close_instant(day).astimezone(NY)
    if eu.date() != day or close.date() != day:
        raise ValueError(f"boundary instants leave the New York date: {day}")
    return eu.hour * 60 + eu.minute, close.hour * 60 + close.minute


def us_dst(day: date) -> bool:
    noon = datetime(day.year, day.month, day.day, 12, tzinfo=NY)
    return bool(noon.dst())


def clean_outliers(bars: Bars) -> tuple[Bars, list[str]]:
    """Drop bars whose OHLC leaves the neighbor-60 close median by more than 3%."""
    count = bars.stamp.size
    if count <= 60:
        return bars, []
    offsets = np.concatenate((np.arange(-30, 0), np.arange(1, 31)))
    remove = np.zeros(count, dtype=bool)
    chunk = 65536
    for start in range(30, count - 30, chunk):
        stop = min(start + chunk, count - 30)
        index = np.arange(start, stop)[:, None] + offsets[None, :]
        window = bars.close[index]
        partition = np.partition(window, (29, 30), axis=1)
        median = (partition[:, 29] + partition[:, 30]) / 2.0
        span = np.stack((bars.open, bars.high, bars.low, bars.close), axis=1)[
            start:stop
        ]
        worst = np.abs(span - median[:, None]).max(axis=1)
        remove[start:stop] = (median > 0) & (worst > 0.03 * median)
    kept = ~remove
    removed = [
        f"{value // 1000000:08d} {value % 1000000:06d}" for value in bars.stamp[remove]
    ]
    return (
        Bars(
            stamp=bars.stamp[kept],
            open=bars.open[kept],
            high=bars.high[kept],
            low=bars.low[kept],
            close=bars.close[kept],
            sha256=bars.sha256,
            parsed=bars.parsed,
        ),
        removed,
    )


def time_checks(bars: Bars) -> dict:
    """R1: the 08:30 volatility peak and the Sunday 17:00 open, per season."""
    _, minute, absolute = bar_minutes(bars.stamp)
    month = bars.stamp // 1_000_000 // 100 % 100
    step = np.diff(absolute)
    pair = np.nonzero(step == 1)[0]
    jump = np.abs(np.log(bars.open[pair + 1] / bars.open[pair]))
    left_minute = minute[pair]
    left_month = month[pair]
    season = np.where(
        np.isin(left_month, (1, 2)), 0, np.where(np.isin(left_month, (7, 8)), 1, -1)
    )
    window = (left_minute >= 420) & (left_minute < 600) & (season >= 0)
    report: dict = {
        "peak_minute": {},
        "top_minutes": {},
        "sunday_median": {},
        "sundays": {},
    }
    for index, name in enumerate(("winter", "summer")):
        chosen = window & (season == index)
        total = np.bincount(left_minute[chosen], weights=jump[chosen], minlength=600)
        counts = np.bincount(left_minute[chosen], minlength=600)
        means = np.divide(total, counts, out=np.zeros(600), where=counts > 0)
        best = int(means.argmax())
        if means[best] <= 0 or int((means == means[best]).sum()) != 1 or best != 510:
            raise ValueError(f"R1 minute check failed for {name}: peak {best}")
        order = np.argsort(-means, kind="stable")
        top = [(int(m), float(means[m])) for m in order[:5] if counts[m] > 0]
        report["peak_minute"][name] = best
        report["top_minutes"][name] = top
    day, minute, _ = bar_minutes(bars.stamp)
    month = bars.stamp // 1_000_000 // 100 % 100
    unique = np.unique(day)
    starts = np.searchsorted(day, unique)
    weekday = np.array([_calendar_date(int(value)).weekday() for value in unique])
    months = np.array([_calendar_date(int(value)).month for value in unique])
    for index, name in enumerate(("winter", "summer")):
        sundays = unique[
            (weekday == 6) & np.isin(months, (1, 2) if index == 0 else (7, 8))
        ]
        first = minute[starts[np.searchsorted(unique, sundays)]]
        if first.size == 0:
            raise ValueError(f"R1 Sunday check failed for {name}: no Sundays")
        median = float(np.median(first))
        if not 1020 <= median <= 1030:
            raise ValueError(f"R1 Sunday check failed for {name}: median {median}")
        report["sunday_median"][name] = median
        report["sundays"][name] = int(first.size)
    return report


@dataclass
class Day:
    date: date
    eu_entry_min: int
    us_entry_min: int
    us_exit_min: int
    us_dst: bool
    mins: np.ndarray
    open: np.ndarray
    high: np.ndarray
    low: np.ndarray
    close: np.ndarray
    eu_entry_price: float
    eu_entry_fallback: bool
    us_entry_price: float
    us_entry_fallback: bool
    us_exit_price: float
    us_exit_fallback: bool


def boundary_price(mins: np.ndarray, opens: np.ndarray, minute):
    """Boundary minute open with the [minute-5, minute-1] same-date fallback."""
    target = np.asarray(minute, dtype=np.int64)
    exact_at = np.searchsorted(mins, target, side="left")
    inside = exact_at < mins.size
    exact = inside & (mins[np.minimum(exact_at, mins.size - 1)] == target)
    below = np.searchsorted(mins, target - 5, side="left")
    fallback = exact_at > below
    price = np.where(
        exact,
        opens[np.minimum(exact_at, mins.size - 1)],
        np.where(fallback, opens[np.maximum(exact_at - 1, 0)], np.nan),
    )
    return price, exact


def computation_days(bars: Bars) -> tuple[list[Day], dict]:
    """Weekdays whose three boundary prices exist, with their bar slices."""
    day, minute, _ = bar_minutes(bars.stamp)
    unique = np.unique(day)
    starts = np.searchsorted(day, unique)
    stops = np.append(starts[1:], bars.stamp.size)
    days: list[Day] = []
    weekdays = 0
    for position, value in enumerate(unique):
        observed = _calendar_date(int(value))
        if observed.weekday() > 4 or observed < FIRST_DAY:
            continue
        weekdays += 1
        eu_entry, us_exit = ny_minutes(observed)
        lo, hi = starts[position], stops[position]
        mins = minute[lo:hi]
        entry, entry_exact = boundary_price(mins, bars.open[lo:hi], np.array(eu_entry))
        middle, middle_exact = boundary_price(mins, bars.open[lo:hi], np.array(480))
        close, close_exact = boundary_price(mins, bars.open[lo:hi], np.array(us_exit))
        if not all(np.isfinite(price) for price in (entry, middle, close)):
            continue
        days.append(
            Day(
                date=observed,
                eu_entry_min=eu_entry,
                us_entry_min=480,
                us_exit_min=us_exit,
                us_dst=us_dst(observed),
                mins=mins,
                open=bars.open[lo:hi],
                high=bars.high[lo:hi],
                low=bars.low[lo:hi],
                close=bars.close[lo:hi],
                eu_entry_price=float(entry),
                eu_entry_fallback=bool(not entry_exact),
                us_entry_price=float(middle),
                us_entry_fallback=bool(not middle_exact),
                us_exit_price=float(close),
                us_exit_fallback=bool(not close_exact),
            )
        )
    stats = {
        "weekday_dates": weekdays,
        "computation_days": len(days),
        "excluded_weekdays": weekdays - len(days),
    }
    return days, stats


W_EU = 0.5777354984728738
W_US = 0.42226450152712625
LEVERAGE_CAP = 43.27239725805739
DIRECTION = {"eu": -1, "us": 1}
PREPARATION_DAYS = 60


@dataclass
class Walk:
    stopped: bool
    exit_min: int
    exit_price: float | None
    flag_min: int | None
    double_touches: int
    optimistic_matches: bool


def _stop_touch(low: float, high: float, level: float, direction: int) -> bool:
    return low <= level if direction > 0 else high >= level


def walk_window(
    mins: np.ndarray,
    high: np.ndarray,
    low: np.ndarray,
    open_: np.ndarray,
    start_min: int,
    length: int,
    entry: float,
    sigma: float,
    direction: int,
    use_stop: bool = True,
) -> Walk:
    """S3 stop walk over the bars with minute in [start_min, start_min + length).

    The stop starts one sigma against the direction and moves to the entry
    price from the bar after a favorable one-sigma touch. A time exit is
    reported as exit_price None at the window's boundary minute; the caller
    supplies the boundary price.
    """
    end_min = start_min + length
    if not use_stop:
        return Walk(False, end_min, None, None, 0, True)
    if not (sigma > 0) or not math.isfinite(sigma):
        raise ValueError("walk sigma must be positive and finite")
    lo = int(np.searchsorted(mins, start_min, side="left"))
    hi = int(np.searchsorted(mins, end_min, side="left"))
    minute = mins[lo:hi].tolist()
    opens = open_[lo:hi].tolist()
    highs = high[lo:hi].tolist()
    lows = low[lo:hi].tolist()
    stop0 = entry * math.exp(-direction * sigma)
    favorable = entry * math.exp(direction * sigma)

    def favorable_touch(high_: float, low_: float) -> bool:
        return high_ >= favorable if direction > 0 else low_ <= favorable

    def run(optimistic: bool) -> Walk:
        level = stop0
        flag_min = None
        move_next = False
        for index in range(len(minute)):
            if move_next:
                level = entry
                move_next = False
            touched = favorable_touch(highs[index], lows[index])
            if optimistic and touched and flag_min is None:
                flag_min = minute[index]
                move_next = True
            if _stop_touch(lows[index], highs[index], level, direction):
                gap = opens[index] < level if direction > 0 else opens[index] > level
                price = opens[index] if gap else level
                return Walk(True, minute[index], price, flag_min, 0, True)
            if not optimistic and touched and flag_min is None:
                flag_min = minute[index]
                move_next = True
        return Walk(False, end_min, None, flag_min, 0, True)

    conservative = run(False)
    if direction > 0:
        dual = (low[lo:hi] <= stop0) & (high[lo:hi] >= favorable)
    else:
        dual = (high[lo:hi] >= stop0) & (low[lo:hi] <= favorable)
    double_touches = int(dual.sum())
    optimistic = run(True)
    matches = (
        conservative.stopped,
        conservative.exit_min,
        conservative.exit_price,
    ) == (optimistic.stopped, optimistic.exit_min, optimistic.exit_price)
    return Walk(
        stopped=conservative.stopped,
        exit_min=conservative.exit_min,
        exit_price=conservative.exit_price,
        flag_min=conservative.flag_min,
        double_touches=double_touches,
        optimistic_matches=matches,
    )


LEG_FIELDS = (
    "entry_price",
    "entry_fallback",
    "boundary_exit_price",
    "sigma",
    "stop_initial",
    "flag_min",
    "stopped",
    "exit_min",
    "exit_price",
    "r_nostop",
    "r",
    "a",
    "c",
)
DAY_COLUMNS = (
    "date",
    "us_dst",
    "eu_entry_min",
    "us_entry_min",
    "us_exit_min_boundary",
    *(f"{leg}_{field}" for leg in ("eu", "us") for field in LEG_FIELDS),
    "u",
    "sigma_hat",
    "leverage",
    "R",
)


def daily_legs(days: list[Day], pip: float = 0.4, target: float = 0.30) -> pd.DataFrame:
    """Per-calculation-day leg table with the days.parquet columns.

    The first PREPARATION_DAYS calculation days carry no sigma and no walk:
    their r is the no-stop return. Volatility estimates always exclude the
    current day and use the previous 60 values.
    """
    half_cost = pip / 2 * 1e-4
    nostop_history: dict[str, list[float]] = {"eu": [], "us": []}
    u_history: list[float] = []
    rows: list[dict] = []
    for index, day in enumerate(days):
        row: dict = {
            "date": day.date,
            "us_dst": day.us_dst,
            "eu_entry_min": day.eu_entry_min,
            "us_entry_min": day.us_entry_min,
            "us_exit_min_boundary": day.us_exit_min,
        }
        walked = index >= PREPARATION_DAYS
        legs: dict[str, dict] = {}
        for leg in ("eu", "us"):
            direction = DIRECTION[leg]
            if leg == "eu":
                entry_min, exit_min = day.eu_entry_min, 480
                entry, fallback = day.eu_entry_price, day.eu_entry_fallback
                boundary_exit = day.us_entry_price
            else:
                entry_min, exit_min = 480, day.us_exit_min
                entry, fallback = day.us_entry_price, day.us_entry_fallback
                boundary_exit = day.us_exit_price
            r_nostop = direction * math.log(boundary_exit / entry)
            sigma = stop_initial = flag_min = None
            if walked:
                sigma = float(np.std(nostop_history[leg][-60:], ddof=1))
                if not math.isfinite(sigma) or sigma <= 0:
                    raise ValueError(f"sigma_leg not positive for {leg} on {day.date}")
                stop_initial = entry * math.exp(-direction * sigma)
                walk = walk_window(
                    day.mins,
                    day.high,
                    day.low,
                    day.open,
                    entry_min,
                    exit_min - entry_min,
                    entry,
                    sigma,
                    direction,
                )
                stopped = walk.stopped
                exit_min = walk.exit_min
                fill = walk.exit_price
                exit_price = boundary_exit if fill is None else fill
                flag_min = walk.flag_min
            else:
                stopped, exit_price = False, boundary_exit
            r = direction * math.log(exit_price / entry)
            a = direction * (exit_price - entry) / entry
            c = half_cost / entry + half_cost / exit_price
            if not all(map(math.isfinite, (r, a, c))):
                raise ValueError(f"non-finite leg return for {leg} on {day.date}")
            legs[leg] = {
                "entry_price": entry,
                "entry_fallback": fallback,
                "boundary_exit_price": boundary_exit,
                "sigma": sigma,
                "stop_initial": stop_initial,
                "flag_min": flag_min,
                "stopped": stopped,
                "exit_min": exit_min,
                "exit_price": exit_price,
                "r_nostop": r_nostop,
                "r": r,
                "a": a,
                "c": c,
            }
        u = W_EU * legs["eu"]["r"] + W_US * legs["us"]["r"]
        sigma_hat = leverage = ret = None
        if walked:
            sigma_hat = math.sqrt(252.0) * float(np.std(u_history[-60:], ddof=1))
            if not math.isfinite(sigma_hat) or sigma_hat <= 0:
                raise ValueError(f"sigma_hat not positive on {day.date}")
            leverage = min(target / sigma_hat, LEVERAGE_CAP)
            ret = leverage * (
                W_EU * (legs["eu"]["a"] - legs["eu"]["c"])
                + W_US * (legs["us"]["a"] - legs["us"]["c"])
            )
        for leg in ("eu", "us"):
            row.update({f"{leg}_{field}": legs[leg][field] for field in LEG_FIELDS})
        row.update({"u": u, "sigma_hat": sigma_hat, "leverage": leverage, "R": ret})
        rows.append(row)
        nostop_history["eu"].append(legs["eu"]["r_nostop"])
        nostop_history["us"].append(legs["us"]["r_nostop"])
        u_history.append(u)
    return pd.DataFrame(rows, columns=DAY_COLUMNS)
