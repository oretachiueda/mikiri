"""Independently recompute the H006 EUR/USD two-leg session preregistration.

Unit 1: HistData zip input, zoneinfo clock handling, outlier cleaning, the
R1 clock check, and computation days with boundary minutes and prices.
Unit 2: the S3 stop walk per leg and the per-day table (sigma, u, sigma_hat,
leverage, R) with the days.parquet column layout.
Unit 3: segments with the tax ledger and pretax path, the SPY baseline, PSR,
and the A/B/C/H gates with the accounts.parquet column layout.
Unit 4: gate D, the random-entry control vectorized per (day, leg) over all
start minutes at once, with the random.parquet column layout.
Unit 5: the prereg section 8 report tables per segment (tax, cost x target,
break-even, tails, bootstrap drawdown, ablations, same bar, flip, yearly),
shaped like the Rust summary.json reports structure.
Unit 6: the CLI pipeline that computes everything from the zips and SPY and
compares days/accounts/random/removed_bars parquet and every summary.json key
against the Rust run, writing a verdict.json envelope (match, capped
differences, summaries, results hash); holdout refuses without a matching dev
envelope, passing dev ledger verdict, equal input hashes, holdout_open and a
fresh holdout.
"""

import argparse
import hashlib
import io
import itertools
import json
import math
import statistics
import sys
import warnings
import zipfile
from dataclasses import dataclass
from datetime import date, datetime, timedelta
from pathlib import Path
from zoneinfo import ZoneInfo

import numpy as np
import pandas as pd
import pyarrow as pa
import pyarrow.parquet as pq

from h001 import checked_run_directory, psr, write_verdict
from h005 import benchmark

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


INPUT_START = date(2007, 1, 1)


def excluded_days(raw: np.ndarray, days: list[Day]) -> list[dict]:
    """Weekday dates from the input start to the last bar that are not days.

    A weekday with no raw bars at all is "no_bars"; one whose cleaned bars
    still fail a boundary price is "missing_boundary".
    """
    raw_dates = {int(value) for value in np.unique(raw // 1_000_000)}
    last = _calendar_date(max(raw_dates))
    calculation = {day.date for day in days}
    result: list[dict] = []
    observed = INPUT_START
    one = timedelta(days=1)
    while observed <= last:
        if observed.weekday() < 5 and observed not in calculation:
            ymd = observed.year * 10000 + observed.month * 100 + observed.day
            result.append(
                {
                    "date": observed.isoformat(),
                    "reason": "no_bars" if ymd not in raw_dates else "missing_boundary",
                }
            )
        observed += one
    return result


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
    optimistic: bool = False,
) -> Walk:
    """S3 stop walk over the bars with minute in [start_min, start_min + length).

    The stop starts one sigma against the direction and moves to the entry
    price from the bar after a favorable one-sigma touch. A time exit is
    reported as exit_price None at the window's boundary minute; the caller
    supplies the boundary price. With optimistic the returned values come
    from the flag-first ordering instead of the conservative one.
    double_touches counts bars from the entry through the conservative exit
    bar inclusive that touch both the bar's current stop and the favorable
    one-sigma level; bars after the exit are not walked (R15 supplement).
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

    def run(flag_first: bool) -> Walk:
        level = stop0
        flag_min = None
        move_next = False
        for index in range(len(minute)):
            if move_next:
                level = entry
                move_next = False
            touched = favorable_touch(highs[index], lows[index])
            if flag_first and touched and flag_min is None:
                flag_min = minute[index]
                move_next = True
            if _stop_touch(lows[index], highs[index], level, direction):
                gap = opens[index] < level if direction > 0 else opens[index] > level
                price = opens[index] if gap else level
                return Walk(True, minute[index], price, flag_min, 0, True)
            if not flag_first and touched and flag_min is None:
                flag_min = minute[index]
                move_next = True
        return Walk(False, end_min, None, flag_min, 0, True)

    conservative = run(False)
    minutes_slice = mins[lo:hi]
    # R15 supplement: count bars walked while the position is open, from the
    # entry boundary minute through the conservative exit bar inclusive. The
    # stop test uses the bar's current stop (stop0 through the flag bar, entry
    # from the bar after the flag); the favorable test always uses E*exp(±sigma).
    flag_pos = (
        int(np.searchsorted(minutes_slice, conservative.flag_min, side="left"))
        if conservative.flag_min is not None
        else len(minutes_slice)
    )
    stop_levels = np.where(np.arange(len(minutes_slice)) <= flag_pos, stop0, entry)
    if direction > 0:
        dual = (low[lo:hi] <= stop_levels) & (high[lo:hi] >= favorable)
    else:
        dual = (high[lo:hi] >= stop_levels) & (low[lo:hi] <= favorable)
    if conservative.stopped:
        exit_pos = int(
            np.searchsorted(minutes_slice, conservative.exit_min, side="left")
        )
        double_touches = int(dual[: exit_pos + 1].sum())
    else:
        double_touches = int(dual.sum())
    optimistic_run = run(True)
    matches = (
        conservative.stopped,
        conservative.exit_min,
        conservative.exit_price,
    ) == (optimistic_run.stopped, optimistic_run.exit_min, optimistic_run.exit_price)
    return (
        Walk(
            stopped=optimistic_run.stopped,
            exit_min=optimistic_run.exit_min,
            exit_price=optimistic_run.exit_price,
            flag_min=optimistic_run.flag_min,
            double_touches=double_touches,
            optimistic_matches=matches,
        )
        if optimistic
        else Walk(
            stopped=conservative.stopped,
            exit_min=conservative.exit_min,
            exit_price=conservative.exit_price,
            flag_min=conservative.flag_min,
            double_touches=double_touches,
            optimistic_matches=matches,
        )
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


def daily_legs(
    days: list[Day],
    pip: float = 0.4,
    target: float = 0.30,
    *,
    stops: bool = True,
    flip: bool = False,
    optimistic: bool = False,
) -> pd.DataFrame:
    """Per-calculation-day leg table with the days.parquet columns.

    The first PREPARATION_DAYS calculation days carry no sigma and no walk:
    their r is the no-stop return. Volatility estimates always exclude the
    current day and use the previous 60 values. Report modes: stops=False
    drops the stop walk (R6 S0), flip reverses both leg directions (R7),
    optimistic takes the flag-first walk ordering (R15).
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
            direction = -DIRECTION[leg] if flip else DIRECTION[leg]
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
                if stops:
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
                        optimistic=optimistic,
                    )
                    stopped = walk.stopped
                    exit_min = walk.exit_min
                    fill = walk.exit_price
                    exit_price = boundary_exit if fill is None else fill
                    flag_min = walk.flag_min
                else:
                    stopped, exit_price = False, boundary_exit
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


TAX_RATE = 0.20315
PSR_THRESHOLD = 1 - 0.05 / 6
SEGMENTS = {
    "dev": (date(2007, 7, 2), date(2024, 10, 31)),
    "recent": (date(2019, 11, 1), date(2024, 10, 31)),
    "holdout": (date(2024, 11, 1), date(2026, 10, 30)),
}
STAGE_SEGMENTS = {"dev": ("dev", "recent"), "holdout": ("dev", "recent", "holdout")}
SPY_CUTOFF = {"dev": date(2024, 10, 31), "holdout": date(2026, 10, 30)}
ACCOUNT_COLUMNS = (
    "segment",
    "date",
    "equity_start",
    "R",
    "tax_paid",
    "equity_end",
    "equity_pretax",
)


def read_spy(path: Path, stage: str) -> list[dict]:
    """Tiingo daily SPY JSON rows, dropped past the stage cutoff before parsing."""
    if stage not in SPY_CUTOFF:
        raise ValueError("invalid stage")
    rows = json.loads(path.read_text())
    if not isinstance(rows, list) or not rows:
        raise ValueError("SPY file must be a non-empty JSON array")
    cutoff = SPY_CUTOFF[stage]
    parsed: list[dict] = []
    dates: list[date] = []
    for row in rows:
        observed = date.fromisoformat(str(row["date"])[:10])
        if observed > cutoff:
            continue
        dates.append(observed)
        entry = {
            "date": observed,
            "close": float(row["close"]),
            "div_cash": float(row["divCash"]),
            "split_factor": float(row["splitFactor"]),
        }
        numbers = (entry["close"], entry["div_cash"], entry["split_factor"])
        if not all(math.isfinite(value) for value in numbers):
            raise ValueError(f"non-finite SPY value on {observed}")
        if entry["close"] <= 0:
            raise ValueError(f"non-positive SPY close on {observed}")
        parsed.append(entry)
    if not parsed:
        raise ValueError("no SPY rows within the stage cutoff")
    gaps = [later - earlier for earlier, later in itertools.pairwise(dates)]
    if any(gap <= timedelta(0) for gap in gaps):
        raise ValueError("SPY dates must be strictly increasing")
    return parsed


class TaxLedger:
    """Calendar-year income tax with the 3-year oldest-first loss carryforward."""

    def __init__(self, rate: float = TAX_RATE):
        self.rate = rate
        self.losses: list[list[float]] = []

    def settle(self, year: int, income: float) -> float:
        self.losses = [loss for loss in self.losses if 1 <= year - loss[0] <= 3]
        if income < 0:
            self.losses.append([year, -income])
            return 0.0
        for loss in self.losses:
            used = min(income, loss[1])
            income -= used
            loss[1] -= used
        return income * self.rate


def _drawdown(path: list[float]) -> float:
    peak = 1.0
    worst = 0.0
    for value in path:
        peak = max(peak, value)
        worst = max(worst, 1.0 - value / peak)
    return worst


def _equity_path(
    rets: list[float],
    days: list[date],
    name: str,
    tax_rate: float,
    strict: bool = True,
) -> tuple[list[dict], list[float], date | None]:
    """Walk equity from 1 through rets with calendar-year tax settling.

    Returns (accounts rows, pretax path, ruin date). Strict raises on ruin
    (R20, the main path); non-strict stops at the ruin day so report rows can
    mark it and continue.
    """
    ledger = TaxLedger(tax_rate)
    accounts: list[dict] = []
    pretax_path: list[float] = []
    equity = 1.0
    pretax = 1.0
    year_income = 0.0
    for position, (day, ret) in enumerate(zip(days, rets, strict=True)):
        year_income += equity * ret
        year_end = position + 1 == len(days) or days[position + 1].year != day.year
        tax = ledger.settle(day.year, year_income) if year_end else 0.0
        if year_end:
            year_income = 0.0
        start = equity
        equity = equity * (1.0 + ret) - tax
        pretax *= 1.0 + ret
        pretax_path.append(pretax)
        accounts.append(
            {
                "segment": name,
                "date": day,
                "equity_start": start,
                "R": ret,
                "tax_paid": tax,
                "equity_end": equity,
                "equity_pretax": pretax,
            }
        )
        if not math.isfinite(equity) or equity <= 0:
            if strict:
                raise ValueError(f"segment {name} equity ruined on {day}")
            return accounts, pretax_path, day
    return accounts, pretax_path, None


def account_segment(
    frame: pd.DataFrame,
    spy_rows: list[dict],
    name: str,
    tax_rate: float = TAX_RATE,
) -> tuple[pd.DataFrame, dict]:
    """One segment's accounts and judgment statistics; equity restarts at 1."""
    if name not in SEGMENTS:
        raise ValueError("invalid segment")
    first, last = SEGMENTS[name]
    rows = frame[(frame["date"] >= first) & (frame["date"] <= last)]
    days = list(rows["date"])
    if not days:
        raise ValueError(f"segment {name} has no calculation days")
    if pd.isna(rows.iloc[0]["leverage"]):
        raise ValueError(f"segment {name} starts before leverage exists")
    rets = [float(value) for value in rows["R"]]
    accounts, pretax_path, _ = _equity_path(rets, days, name, tax_rate)
    equity = accounts[-1]["equity_end"]
    spy_slice = [bar for bar in spy_rows if first <= bar["date"] <= last]
    if not spy_slice:
        raise ValueError(f"segment {name} has no SPY rows")
    values, _ = benchmark(spy_slice)
    elapsed = (days[-1] - days[0]).days
    leverage = rows["leverage"].astype(float)
    max_leverage = float(leverage.max())

    def cagr(final: float) -> float | None:
        return final ** (365.0 / elapsed) - 1.0 if elapsed > 0 else None

    psr_value = psr(rets)
    stats = {
        "segment": name,
        "w": equity,
        "b": values[-1],
        "sr": math.sqrt(252.0) * statistics.fmean(rets) / statistics.stdev(rets),
        "cagr_pretax": cagr(pretax_path[-1]),
        "cagr_aftertax": cagr(equity),
        "drawdown_pretax": _drawdown(pretax_path),
        "drawdown_aftertax": _drawdown([row["equity_end"] for row in accounts]),
        "max_leverage": max_leverage,
        "max_leg_notional_to_equity": max_leverage * W_EU,
        "psr": psr_value,
        "psr_threshold": PSR_THRESHOLD,
    }
    return pd.DataFrame(accounts, columns=ACCOUNT_COLUMNS), stats


def evaluate_gates(segment_stats: dict[str, dict]) -> dict[str, bool | None]:
    """A: dev after-tax W > B. B: dev PSR. C: recent W > B. H: holdout W > B."""
    dev = segment_stats["dev"]
    gates: dict[str, bool | None] = {
        "A": bool(dev["w"] > dev["b"]),
        "B": bool(dev["psr"] >= PSR_THRESHOLD),
        "C": bool(segment_stats["recent"]["w"] > segment_stats["recent"]["b"]),
        "H": None,
    }
    if "holdout" in segment_stats:
        holdout = segment_stats["holdout"]
        gates["H"] = bool(holdout["w"] > holdout["b"])
    return gates


def run_segments(
    frame: pd.DataFrame,
    spy_rows: list[dict],
    stage: str,
    tax_rate: float = TAX_RATE,
) -> tuple[pd.DataFrame, dict[str, dict], dict[str, bool | None]]:
    """All of the stage's segments with accounts, statistics, and gates."""
    if stage not in STAGE_SEGMENTS:
        raise ValueError("invalid stage")
    tables: list[pd.DataFrame] = []
    stats: dict[str, dict] = {}
    for name in STAGE_SEGMENTS[stage]:
        accounts, segment = account_segment(frame, spy_rows, name, tax_rate)
        tables.append(accounts)
        stats[name] = segment
    return pd.concat(tables, ignore_index=True), stats, evaluate_gates(stats)


RANDOM_COLUMNS = (
    "round",
    "day_index",
    "leg",
    "K",
    "start_min",
    "exit_min",
    "stopped",
    "a",
    "c",
    "valid",
)
ROUND_COUNT = 1000
GATE_D_LEVEL = 0.95


def _fallback_prices(values: np.ndarray, targets: np.ndarray) -> np.ndarray:
    """Exact bar at each target minute, else the last bar up to 5 minutes earlier."""
    price = values[targets].copy()
    for shift in range(1, 6):
        need = ~np.isfinite(price)
        if not need.any():
            break
        source = targets - shift
        inside = source >= 0
        candidate = np.where(inside, values[np.maximum(source, 0)], np.nan)
        price = np.where(need & np.isfinite(candidate), candidate, price)
    return price


def _window_walk(
    low_d: np.ndarray,
    high_d: np.ndarray,
    open_d: np.ndarray,
    length: int,
    sigma: float,
    direction: int,
    end_max: int,
) -> tuple[int, np.ndarray, np.ndarray, np.ndarray, np.ndarray]:
    """S3 walk for every start minute m with m + length <= end_max.

    Returns (K, entry, stopped, exit_min, exit_price) indexed by m. Time exits
    carry the boundary price of minute m + length and stopped fills respect
    gap opens. Missing bars never touch a level because NaN compares False.
    """
    K = end_max - length + 1
    if K <= 0:
        empty = np.array([])
        return 0, empty, empty.astype(bool), empty.astype(np.int64), empty
    starts = np.arange(K)
    entry = _fallback_prices(open_d, starts)
    if length == 0:
        return K, entry, np.zeros(K, dtype=bool), starts.copy(), entry.copy()
    end_price = _fallback_prices(open_d, starts + length)
    stop0 = entry * math.exp(-direction * sigma)
    favorable = entry * math.exp(direction * sigma)
    low_w = np.lib.stride_tricks.sliding_window_view(low_d, length)[:K]
    high_w = np.lib.stride_tricks.sliding_window_view(high_d, length)[:K]
    open_w = np.lib.stride_tricks.sliding_window_view(open_d, length)[:K]
    if direction > 0:
        hit0 = low_w <= stop0[:, None]
        flag_hit = high_w >= favorable[:, None]
        hit1 = low_w <= entry[:, None]
    else:
        hit0 = high_w >= stop0[:, None]
        flag_hit = low_w <= favorable[:, None]
        hit1 = high_w >= entry[:, None]
    none = length
    j0 = np.where(hit0.any(axis=1), hit0.argmax(axis=1), none)
    jf = np.where(flag_hit.any(axis=1), flag_hit.argmax(axis=1), none)
    after = np.arange(length)[None, :] > jf[:, None]
    hit1_after = hit1 & after
    j1 = np.where(hit1_after.any(axis=1), hit1_after.argmax(axis=1), none)
    first = j0 <= jf
    j_exit = np.where(first, j0, j1)
    stopped = j_exit < none
    exit_min = np.where(stopped, starts + j_exit, starts + length)
    open_at = open_w[np.arange(K), np.minimum(j_exit, length - 1)]
    level = np.where(first, stop0, entry)
    gap = open_at < level if direction > 0 else open_at > level
    fill = np.where(gap, open_at, level)
    exit_price = np.where(stopped, fill, end_price)
    return K, entry, stopped, exit_min, exit_price


def random_control(
    frame: pd.DataFrame,
    days: list[Day],
    rounds: int = ROUND_COUNT,
    pip: float = 0.4,
    table: bool = True,
) -> tuple[pd.DataFrame | None, dict]:
    """Gate D: matched random-entry control over the dev calculation days."""
    first_day, last_day = SEGMENTS["dev"]
    rows = frame[(frame["date"] >= first_day) & (frame["date"] <= last_day)]
    rows = rows.reset_index(drop=True)
    dev_days = [day for day in days if first_day <= day.date <= last_day]
    if len(dev_days) != len(rows):
        raise ValueError("dev frame rows and day objects disagree")
    half_cost = pip / 2 * 1e-4
    count = len(dev_days)
    seeds = np.arange(rounds, dtype=np.uint64)
    day_returns = np.full((rounds, count), np.nan)
    valid = np.zeros((rounds, count), dtype=bool)
    start_store = np.full((rounds, count, 2), -1, dtype=np.int64)
    exit_store = np.zeros((rounds, count, 2), dtype=np.int64)
    stopped_store = np.zeros((rounds, count, 2), dtype=bool)
    a_store = np.full((rounds, count, 2), np.nan)
    c_store = np.full((rounds, count, 2), np.nan)
    ks = np.zeros((count, 2), dtype=np.int64)
    for index, (day, row) in enumerate(zip(dev_days, rows.itertuples(), strict=True)):
        open_d = np.full(1440, np.nan)
        high_d = np.full(1440, np.nan)
        low_d = np.full(1440, np.nan)
        open_d[day.mins] = day.open
        high_d[day.mins] = day.high
        low_d[day.mins] = day.low
        end_max = 1018 if day.date.weekday() == 4 else 1438
        windows = []
        lengths = []
        for leg_id, leg in enumerate(("eu", "us")):
            entry_min = day.eu_entry_min if leg_id == 0 else day.us_entry_min
            length = int(getattr(row, f"{leg}_exit_min")) - entry_min
            lengths.append(length)
            K = end_max - length + 1
            ks[index, leg_id] = K
            if K <= 0:
                windows.append(None)
                continue
            sigma = float(getattr(row, f"{leg}_sigma"))
            _, entry, stopped, exit_min, exit_price = _window_walk(
                low_d, high_d, open_d, length, sigma, DIRECTION[leg], end_max
            )
            seed_n = seeds * np.uint64(2**32) + np.uint64(2 * index + leg_id)
            m = (splitmix64(seed_n) % np.uint64(K)).astype(np.int64)
            start_store[:, index, leg_id] = m
            walked = np.isfinite(entry[m]) & np.isfinite(exit_price[m])
            entry_r, exit_r = entry[m], exit_price[m]
            a_r = DIRECTION[leg] * (exit_r - entry_r) / entry_r
            c_r = half_cost / entry_r + half_cost / exit_r
            stopped_store[:, index, leg_id] = stopped[m] & walked
            exit_store[:, index, leg_id] = np.where(walked, exit_min[m], 0)
            a_store[:, index, leg_id] = np.where(walked, a_r, np.nan)
            c_store[:, index, leg_id] = np.where(walked, c_r, np.nan)
            windows.append((m, walked))
        if windows[0] is None or windows[1] is None:
            continue
        m_eu, ok_eu = windows[0]
        m_us, ok_us = windows[1]
        overlap = np.maximum(m_eu, m_us) < np.minimum(
            m_eu + lengths[0], m_us + lengths[1]
        )
        day_valid = ok_eu & ok_us & ~overlap
        valid[:, index] = day_valid
        if day_valid.any():
            leverage = float(row.leverage)
            net = W_EU * (a_store[:, index, 0] - c_store[:, index, 0]) + W_US * (
                a_store[:, index, 1] - c_store[:, index, 1]
            )
            day_returns[:, index] = np.where(day_valid, leverage * net, np.nan)
    counts = valid.sum(axis=1)
    with warnings.catch_warnings():
        warnings.simplefilter("ignore", RuntimeWarning)
        mean = np.nanmean(day_returns, axis=1)
        spread = np.nanstd(day_returns, axis=1, ddof=1)
    computable = (counts >= 2) & (spread > 0) & np.isfinite(spread) & np.isfinite(mean)
    round_sr = np.where(computable, math.sqrt(252.0) * mean / spread, np.nan)
    actual = [float(value) for value in rows["R"]]
    actual_sr = math.sqrt(252.0) * statistics.fmean(actual) / statistics.stdev(actual)
    if bool(computable.all()):
        percentile = float(np.count_nonzero(round_sr <= actual_sr) / rounds)
        gate_d = percentile >= GATE_D_LEVEL
    else:
        percentile = None
        gate_d = False
    control = {
        "actual_sr": actual_sr,
        "round_sr": [
            float(value) if np.isfinite(value) else None for value in round_sr
        ],
        "excluded_days": [int(value) for value in count - counts],
        "percentile": percentile,
        "gate_d": bool(gate_d),
    }
    if not table:
        return None, control
    starts_flat = start_store.reshape(-1)
    walked_flat = starts_flat >= 0
    valid_flat = np.repeat(valid, 2, axis=1).reshape(-1)
    result = pd.DataFrame(
        {
            "round": np.repeat(np.arange(rounds), count * 2),
            "day_index": np.tile(np.repeat(np.arange(count), 2), rounds),
            "leg": np.tile(np.array(["eu", "us"], dtype=object), rounds * count),
            "K": np.tile(ks.reshape(-1), rounds),
            "start_min": pd.array(
                np.where(walked_flat, starts_flat, np.nan), dtype="Int64"
            ),
            "exit_min": pd.array(
                np.where(walked_flat, exit_store.reshape(-1), np.nan), dtype="Int64"
            ),
            "stopped": pd.array(
                np.where(walked_flat, stopped_store.reshape(-1), np.nan),
                dtype="boolean",
            ),
            "a": a_store.reshape(-1),
            "c": c_store.reshape(-1),
            "valid": valid_flat,
        }
    )
    return result[list(RANDOM_COLUMNS)], control


COMMON_FIELDS = (
    "ruined",
    "ruin_date",
    "w",
    "b",
    "cagr_pretax",
    "cagr_aftertax",
    "drawdown_pretax",
    "drawdown_aftertax",
    "max_leverage",
    "max_leg_notional_to_equity",
)
TAX_RATES = (0.0, 0.05)
COST_PIPS = (0.0, 0.4, 0.8, 1.5)
COST_TARGETS = (0.30, 0.40)
BOOTSTRAP_ROUNDS = 1000


def _segment_rows(frame: pd.DataFrame, name: str) -> pd.DataFrame:
    """A segment's calculation days; preparation rows carry no trade."""
    first, last = SEGMENTS[name]
    rows = frame[(frame["date"] >= first) & (frame["date"] <= last)]
    rows = rows[rows["R"].notna()]
    if len(rows) == 0:
        raise ValueError(f"segment {name} has no calculation days")
    return rows


def _sr(values: list[float]) -> float | None:
    """Annualized Sharpe of daily returns; None when not computable (R9)."""
    if len(values) < 2:
        return None
    spread = statistics.stdev(values)
    if not math.isfinite(spread) or spread <= 0:
        return None
    return math.sqrt(252.0) * statistics.fmean(values) / spread


def _median(values: list[float | None]) -> float | None:
    kept = sorted(value for value in values if value is not None)
    return statistics.median(kept) if kept else None


def _segment_common(
    frame: pd.DataFrame,
    spy_rows: list[dict],
    name: str,
    tax_rate: float = TAX_RATE,
) -> dict[str, float | str | bool | None]:
    """The ten common report fields for one frame over one segment (R20)."""
    rows = _segment_rows(frame, name)
    days = list(rows["date"])
    rets = [float(value) for value in rows["R"]]
    accounts, pretax_path, ruin = _equity_path(rets, days, name, tax_rate, strict=False)
    first, last = SEGMENTS[name]
    spy_slice = [bar for bar in spy_rows if first <= bar["date"] <= last]
    if not spy_slice:
        raise ValueError(f"segment {name} has no SPY rows")
    b = benchmark(spy_slice)[0][-1]
    max_leverage = float(rows["leverage"].astype(float).max())
    if ruin is not None:
        values: dict[str, float | str | bool | None] = {
            "ruined": True,
            "ruin_date": ruin.isoformat(),
            "w": None,
            "b": b,
            "cagr_pretax": None,
            "cagr_aftertax": None,
            "drawdown_pretax": None,
            "drawdown_aftertax": None,
            "max_leverage": max_leverage,
            "max_leg_notional_to_equity": max_leverage * W_EU,
        }
    else:
        elapsed = (days[-1] - days[0]).days

        def cagr(final: float) -> float | None:
            return final ** (365.0 / elapsed) - 1.0 if elapsed > 0 else None

        values = {
            "ruined": False,
            "ruin_date": None,
            "w": accounts[-1]["equity_end"],
            "b": b,
            "cagr_pretax": cagr(pretax_path[-1]),
            "cagr_aftertax": cagr(accounts[-1]["equity_end"]),
            "drawdown_pretax": _drawdown(pretax_path),
            "drawdown_aftertax": _drawdown([row["equity_end"] for row in accounts]),
            "max_leverage": max_leverage,
            "max_leg_notional_to_equity": max_leverage * W_EU,
        }
    return {field: values[field] for field in COMMON_FIELDS}


def _tail_values(values: list[float]) -> dict[str, float | None]:
    """Loss-tail quantiles by integer nearest rank (R8); zero wins nothing."""
    values = [float(value) for value in values]
    ordered = sorted(values)
    count = len(ordered)

    def rank(percent: int) -> float:
        return ordered[(count * percent + 99) // 100 - 1]

    gains = [value for value in values if value > 0]
    losses = [value for value in values if value < 0]
    return {
        "p01": rank(1),
        "p05": rank(5),
        "worst": ordered[0],
        "win_loss_ratio": statistics.fmean(gains) / abs(statistics.fmean(losses))
        if gains and losses
        else None,
        "win_rate": len(gains) / count,
    }


def _tails(frame: pd.DataFrame, name: str) -> dict[str, float | None]:
    rows = _segment_rows(frame, name)
    xs: list[float] = []
    for eu_a, eu_c, us_a, us_c in zip(
        rows["eu_a"], rows["eu_c"], rows["us_a"], rows["us_c"], strict=True
    ):
        xs.append(float(eu_a) - float(eu_c))
        xs.append(float(us_a) - float(us_c))
    return _tail_values(xs)


def _bootstrap_drawdowns(
    rets: list[float], rounds: int = BOOTSTRAP_ROUNDS
) -> np.ndarray:
    """Sorted per-round maximum drawdowns (R10); a ruined path scores 1."""
    count = len(rets)
    base = np.asarray(rets, dtype=float)
    d = np.arange(rounds, dtype=np.uint64)[:, None]
    i = np.arange(count, dtype=np.uint64)[None, :]
    idx = (splitmix64(d * np.uint64(2**32) + i) % np.uint64(count)).astype(np.int64)
    equity = np.cumprod(base[idx] + 1.0, axis=1)
    peak = np.maximum(1.0, np.maximum.accumulate(equity, axis=1))
    draws = 1.0 - equity / peak
    draws[~np.isfinite(draws)] = 1.0
    draws[equity <= 0] = 1.0
    worst = draws.max(axis=1)
    worst[~np.isfinite(worst)] = 1.0
    return np.sort(worst)


def _bootstrap_values(rets: list[float]) -> dict[str, float]:
    draws = _bootstrap_drawdowns(rets)
    return {
        "p05": float(draws[49]),
        "p50": float(draws[499]),
        "p95": float(draws[949]),
    }


def _breakeven(frame: pd.DataFrame, name: str) -> dict[str, float | None]:
    """Cost multiple k* with mean R = 0 and its pip equivalent (R12)."""
    rows = _segment_rows(frame, name)
    gains = [
        float(leverage) * (W_EU * float(eu_a) + W_US * float(us_a))
        for leverage, eu_a, us_a in zip(
            rows["leverage"], rows["eu_a"], rows["us_a"], strict=True
        )
    ]
    costs = [
        float(leverage) * (W_EU * float(eu_c) + W_US * float(us_c))
        for leverage, eu_c, us_c in zip(
            rows["leverage"], rows["eu_c"], rows["us_c"], strict=True
        )
    ]
    denominator = statistics.fmean(costs)
    if denominator == 0:
        return {"multiple": None, "pip": None}
    multiple = statistics.fmean(gains) / denominator
    return {"multiple": multiple, "pip": 0.4 * multiple}


def _yearly(frame: pd.DataFrame, name: str) -> dict:
    rows = _segment_rows(frame, name)
    by_year: dict[int, list[float]] = {}
    winter: list[float] = []
    summer: list[float] = []
    for day, ret, dst in zip(rows["date"], rows["R"], rows["us_dst"], strict=True):
        value = float(ret)
        by_year.setdefault(day.year, []).append(value)
        (summer if dst else winter).append(value)
    years = [
        {"year": year, "days": len(values), "sr": _sr(values)}
        for year, values in sorted(by_year.items())
    ]
    return {
        "years": years,
        "median_sr": _median([entry["sr"] for entry in years]),
        "winter": {"days": len(winter), "sr": _sr(winter)},
        "summer": {"days": len(summer), "sr": _sr(summer)},
    }


def ablation_frame(frame: pd.DataFrame, leg: str) -> pd.DataFrame:
    """R6 leg ablation: the missing leg contributes zero w*(a - c)."""
    if leg not in ("eu", "us"):
        raise ValueError("invalid leg")
    weight = W_EU if leg == "eu" else W_US
    result = frame.copy()
    result["R"] = (
        result["leverage"] * weight * (result[f"{leg}_a"] - result[f"{leg}_c"])
    )
    return result


def _same_bar(frame: pd.DataFrame, days: list[Day], name: str) -> tuple[int, bool]:
    """Re-walk a segment counting double touches and optimistic agreement (R15)."""
    rows = _segment_rows(frame, name)
    by_date = {day.date: day for day in days}
    touches = 0
    matches = True
    for (
        day_date,
        eu_entry,
        us_exit,
        eu_entry_price,
        eu_sigma,
        us_entry_price,
        us_sigma,
    ) in zip(
        rows["date"],
        rows["eu_entry_min"],
        rows["us_exit_min_boundary"],
        rows["eu_entry_price"],
        rows["eu_sigma"],
        rows["us_entry_price"],
        rows["us_sigma"],
        strict=True,
    ):
        day = by_date[day_date]
        legs = (
            ("eu", int(eu_entry), 480, float(eu_entry_price), float(eu_sigma)),
            ("us", 480, int(us_exit), float(us_entry_price), float(us_sigma)),
        )
        for leg, entry_min, exit_min, entry, sigma in legs:
            walk = walk_window(
                day.mins,
                day.high,
                day.low,
                day.open,
                entry_min,
                exit_min - entry_min,
                entry,
                sigma,
                DIRECTION[leg],
            )
            touches += walk.double_touches
            matches = matches and walk.optimistic_matches
    return touches, matches


def flip_warning(original_sr: float | None, reversed_sr: float | None) -> bool:
    """R7: warn when the original and the reversed strategy both profit."""
    return bool(
        original_sr is not None
        and reversed_sr is not None
        and original_sr > 0
        and reversed_sr > 0
    )


def segment_report(
    frame: pd.DataFrame,
    days: list[Day],
    spy_rows: list[dict],
    name: str,
    tax_rate: float = TAX_RATE,
) -> dict:
    """One segment's prereg section 8 tables, summary.json-shaped."""
    common = _segment_common(frame, spy_rows, name, tax_rate)
    rows = _segment_rows(frame, name)
    rets = [float(value) for value in rows["R"]]
    cost_cells: list[dict] = []
    for pip in COST_PIPS:
        for target in COST_TARGETS:
            cell = _segment_common(
                daily_legs(days, pip=pip, target=target), spy_rows, name, tax_rate
            )
            if (pip, target) == (0.4, 0.30) and cell != common:
                raise ValueError(f"cost_target self-check failed for segment {name}")
            cost_cells.append({"pip": pip, "target": target, "common": cell})
    touches, matches = _same_bar(frame, days, name)
    optimistic_common = _segment_common(
        daily_legs(days, optimistic=True), spy_rows, name, tax_rate
    )
    if matches and optimistic_common != common:
        raise ValueError(f"same_bar self-check failed for segment {name}")
    flipped = daily_legs(days, flip=True)
    flip_rets = [float(value) for value in _segment_rows(flipped, name)["R"]]
    original_sr = _sr(rets)
    reversed_sr = _sr(flip_rets)
    return {
        "main": common,
        "tax_rates": [
            {"rate": rate, "common": _segment_common(frame, spy_rows, name, rate)}
            for rate in TAX_RATES
        ],
        "cost_target": cost_cells,
        "breakeven": {"common": common, **_breakeven(frame, name)},
        "tails": {"common": common, "values": _tails(frame, name)},
        "bootstrap": {"common": common, "values": _bootstrap_values(rets)},
        "ablations": [
            {
                "name": "eu_only",
                "common": _segment_common(
                    ablation_frame(frame, "eu"), spy_rows, name, tax_rate
                ),
            },
            {
                "name": "us_only",
                "common": _segment_common(
                    ablation_frame(frame, "us"), spy_rows, name, tax_rate
                ),
            },
            {
                "name": "s0",
                "common": _segment_common(
                    daily_legs(days, stops=False), spy_rows, name, tax_rate
                ),
            },
        ],
        "same_bar": {
            "common": common,
            "optimistic_common": optimistic_common,
            "double_touches": touches,
            "optimistic_matches": matches,
        },
        "flip": {
            "common": _segment_common(flipped, spy_rows, name, tax_rate),
            "original_sr": original_sr,
            "reversed_sr": reversed_sr,
            "warning": flip_warning(original_sr, reversed_sr),
        },
        "yearly": {"common": common, "values": _yearly(frame, name)},
    }


def segment_reports(
    frame: pd.DataFrame,
    days: list[Day],
    spy_rows: list[dict],
    stage: str = "dev",
    tax_rate: float = TAX_RATE,
) -> dict[str, dict]:
    """Report tables for every segment of the stage."""
    if stage not in STAGE_SEGMENTS:
        raise ValueError("invalid stage")
    return {
        name: segment_report(frame, days, spy_rows, name, tax_rate)
        for name in STAGE_SEGMENTS[stage]
    }


# Unit 6: the CLI pipeline, the comparison with the Rust run, and the verdict.

RESULT_FILES = (
    "accounts.parquet",
    "days.parquet",
    "random.parquet",
    "removed_bars.parquet",
    "summary.json",
)
MISMATCH_CAP = 50
PLATEAU_IN_SAMPLE = {
    "A": {"S0": 1.369, "S1": 1.350, "S2": 1.043, "S3": 1.452},
    "B": {"S0": 1.245, "S1": 1.353, "S2": 1.236, "S3": 1.484},
    "C": {"S0": 1.453, "S1": 1.510, "S2": 1.207, "S3": 1.584},
}
HOLM_NOTE = "B equals the first Holm step for one setting"
_PLAIN_TABLES = ("breakeven", "tails", "bootstrap", "same_bar", "flip", "yearly")
_ABLATION_NAMES = ("eu_only", "us_only", "s0")
_ABSOLUTE_LABELS = ("psr", "percentile")


def report_combinations(stage: str) -> list[dict]:
    """Every executed report table and its parameters, in output order."""
    if stage not in STAGE_SEGMENTS:
        raise ValueError("invalid stage")
    rows: list[dict] = []
    for segment in STAGE_SEGMENTS[stage]:
        rows.extend(
            {"segment": segment, "table": "tax", "rate": rate} for rate in TAX_RATES
        )
        rows.extend(
            {"segment": segment, "table": "cost_target", "pip": pip, "target": target}
            for pip in COST_PIPS
            for target in COST_TARGETS
        )
        rows.extend(
            {"segment": segment, "table": "ablation", "name": name}
            for name in _ABLATION_NAMES
        )
        rows.extend({"segment": segment, "table": name} for name in _PLAIN_TABLES)
    return rows


def clock_summary(clock: dict) -> dict:
    """The summary.json clock_check block from a time_checks report."""
    return {
        season: {
            "top": [
                [int(minute), float(mean)]
                for minute, mean in clock["top_minutes"][season]
            ],
            "sunday_median": float(clock["sunday_median"][season]),
            "sundays": int(clock["sundays"][season]),
        }
        for season in ("winter", "summer")
    }


def python_summary(
    stage: str,
    bars: Bars,
    cleaned: Bars,
    removed: list[str],
    clock: dict,
    excluded: list[dict],
    days: list[Day],
    spy_digest: str,
    stats: dict[str, dict],
    gates: dict[str, bool | None],
    control: dict,
    reports: dict[str, dict],
) -> dict:
    """The Python summary mirroring the Rust summary.json key for key."""
    fields = (
        "w",
        "b",
        "sr",
        "cagr_pretax",
        "cagr_aftertax",
        "drawdown_pretax",
        "drawdown_aftertax",
        "max_leverage",
        "max_leg_notional_to_equity",
    )
    return {
        "stage": stage,
        "input_sha256": {**bars.sha256, "SPY.json": spy_digest},
        "counts": {
            "bars_input": int(bars.parsed),
            "bars_clean": int(cleaned.stamp.size),
            "removed_bars": len(removed),
            "calculation_days": len(days),
            "excluded_days": len(excluded),
        },
        "first_calculation_day": days[0].date.isoformat(),
        "excluded_days": excluded,
        "clock_check": clock_summary(clock),
        "gates": {
            "A": gates["A"],
            "B": gates["B"],
            "C": gates["C"],
            "D": control["gate_d"],
            "H": gates["H"],
        },
        "psr": stats["dev"]["psr"],
        "psr_threshold": PSR_THRESHOLD,
        "random_control": control,
        "reports": reports,
        "segments": [
            {"segment": name, **{field: stats[name][field] for field in fields}}
            for name in STAGE_SEGMENTS[stage]
        ],
        "report_combinations": report_combinations(stage),
        "plateau_in_sample": PLATEAU_IN_SAMPLE,
        "holm": HOLM_NOTE,
    }


def compare_tree(
    expected: object,
    actual: object,
    label: str = "summary",
    differences: list[str] | None = None,
) -> list[str]:
    """Depth-first summary comparison: structure, booleans, strings and dates
    exact; integers exact; numbers |a-b| <= 1e-10 * max(1, |a|, |b|); PSR and
    percentile absolute 1e-9; null matches null."""
    if differences is None:
        differences = []
    if isinstance(expected, dict):
        if not isinstance(actual, dict) or expected.keys() != actual.keys():
            differences.append(f"{label}: fields differ")
            return differences
        for key, value in expected.items():
            compare_tree(value, actual[key], f"{label}.{key}", differences)
    elif isinstance(expected, list):
        if not isinstance(actual, list) or len(expected) != len(actual):
            differences.append(f"{label}: length differs")
            return differences
        for index, (left, right) in enumerate(zip(expected, actual, strict=True)):
            compare_tree(left, right, f"{label}[{index}]", differences)
    elif isinstance(expected, bool) or isinstance(actual, bool):
        if not isinstance(expected, bool) or not isinstance(actual, bool):
            differences.append(f"{label}: type differs")
        elif expected != actual:
            differences.append(f"{label}: mismatch")
    elif expected is None or actual is None:
        if expected is not None or actual is not None:
            differences.append(f"{label}: null mismatch")
    elif isinstance(expected, str) or isinstance(actual, str):
        if not isinstance(expected, str) or not isinstance(actual, str):
            differences.append(f"{label}: type differs")
        elif expected != actual:
            differences.append(f"{label}: mismatch")
    elif isinstance(expected, int) and isinstance(actual, int):
        if expected != actual:
            differences.append(f"{label}: mismatch")
    elif isinstance(expected, (int, float)) and isinstance(actual, (int, float)):
        if isinstance(expected, int) and isinstance(actual, int):
            if expected != actual:
                differences.append(f"{label}: mismatch")
            return differences
        left, right = float(expected), float(actual)
        name = label.rsplit(".", 1)[-1]
        tolerance = (
            1e-9
            if name in _ABSOLUTE_LABELS
            else 1e-10 * max(1.0, abs(left), abs(right))
        )
        if not math.isfinite(right) or abs(left - right) > tolerance:
            differences.append(f"{label}: {expected} vs {actual}")
    else:
        differences.append(f"{label}: type differs")
    return differences


def compare_table(expected: pa.Table, path: Path, label: str) -> list[str]:
    """Column-by-column comparison of a built table with the Rust parquet.

    Dates, times, booleans, integers and strings must match exactly with
    null matching null; float columns use 1e-10 * max(1, |a|, |b|).
    """
    actual = pq.read_table(path)
    names = [field.name for field in expected.schema]
    if names != actual.schema.names:
        return [f"{label}: columns differ"]
    differences: list[str] = []
    for name in names:
        mine = _single_array(expected.column(name))
        theirs = _single_array(actual.column(name))
        if pa.types.is_floating(mine.type):
            left = np.asarray(mine.to_numpy(zero_copy_only=False), dtype="float64")
            right = np.asarray(theirs.to_numpy(zero_copy_only=False), dtype="float64")
            left_null = np.isnan(left)
            right_null = np.isnan(right)
            scale = np.maximum(1.0, np.maximum(np.abs(left), np.abs(right)))
            bad = (left_null != right_null) | (
                ~left_null & ~right_null & (np.abs(left - right) > 1e-10 * scale)
            )
            if bad.any():
                index = int(np.flatnonzero(bad)[0])
                differences.append(
                    f"{label}.{name}[{index}]: {left[index]} vs {right[index]}"
                )
        elif not mine.equals(theirs):
            index = _first_value_difference(mine, theirs)
            differences.append(
                f"{label}.{name}[{index}]: {mine[index].as_py()} vs {theirs[index].as_py()}"
            )
    return differences


def _single_array(column: pa.ChunkedArray) -> pa.Array:
    combined = column.combine_chunks()
    if isinstance(combined, pa.ChunkedArray):
        return (
            combined.chunk(0) if combined.num_chunks else pa.array([], type=column.type)
        )
    return combined


def _first_value_difference(mine: pa.Array, theirs: pa.Array) -> int:
    """The first position where two same-typed arrays differ; -1 if unclear."""
    left = mine.to_numpy(zero_copy_only=False)
    right = theirs.to_numpy(zero_copy_only=False)
    if left.dtype == object:
        for index, (a, b) in enumerate(zip(left, right, strict=True)):
            if a != b:
                return index
        return -1
    differs = left != right
    if left.dtype == np.float64 and right.dtype == np.float64:
        differs &= ~(np.isnan(left) & np.isnan(right))
    positions = np.flatnonzero(differs)
    return int(positions[0]) if positions.size else -1


_LEG_TYPES = (
    ("entry_price", pa.float64()),
    ("entry_fallback", pa.bool_()),
    ("boundary_exit_price", pa.float64()),
    ("sigma", pa.float64()),
    ("stop_initial", pa.float64()),
    ("flag_min", pa.int64()),
    ("stopped", pa.bool_()),
    ("exit_min", pa.int64()),
    ("exit_price", pa.float64()),
    ("r_nostop", pa.float64()),
    ("r", pa.float64()),
    ("a", pa.float64()),
    ("c", pa.float64()),
)
_DAYS_SCHEMA = pa.schema(
    [
        ("date", pa.string()),
        ("us_dst", pa.bool_()),
        ("eu_entry_min", pa.int64()),
        ("us_entry_min", pa.int64()),
        ("us_exit_min_boundary", pa.int64()),
        *(
            (f"{leg}_{field}", type_)
            for leg in ("eu", "us")
            for field, type_ in _LEG_TYPES
        ),
        ("u", pa.float64()),
        ("sigma_hat", pa.float64()),
        ("leverage", pa.float64()),
        ("R", pa.float64()),
    ]
)
_ACCOUNT_SCHEMA = pa.schema(
    [
        ("segment", pa.string()),
        ("date", pa.string()),
        ("equity_start", pa.float64()),
        ("R", pa.float64()),
        ("tax_paid", pa.float64()),
        ("equity_end", pa.float64()),
        ("equity_pretax", pa.float64()),
    ]
)
_RANDOM_SCHEMA = pa.schema(
    [
        ("round", pa.int64()),
        ("day_index", pa.int64()),
        ("leg", pa.string()),
        ("K", pa.int64()),
        ("start_min", pa.int64()),
        ("exit_min", pa.int64()),
        ("stopped", pa.bool_()),
        ("a", pa.float64()),
        ("c", pa.float64()),
        ("valid", pa.bool_()),
    ]
)


def days_table(frame: pd.DataFrame) -> pa.Table:
    """The days.parquet table built from a daily_legs frame."""
    columns = {}
    for field in _DAYS_SCHEMA:
        values = frame[field.name]
        if field.type == pa.string():
            array = pa.array([value.isoformat() for value in values], type=field.type)
        elif field.type == pa.bool_():
            array = pa.array([bool(value) for value in values], type=field.type)
        elif field.type == pa.int64():
            array = pa.array(
                [None if pd.isna(value) else int(value) for value in values],
                type=field.type,
            )
        else:
            array = pa.array(
                [None if pd.isna(value) else float(value) for value in values],
                type=field.type,
            )
        columns[field.name] = array
    return pa.table(columns, schema=_DAYS_SCHEMA)


def accounts_table(accounts: pd.DataFrame) -> pa.Table:
    """The accounts.parquet table built from a run_segments frame."""
    return pa.table(
        {
            "segment": pa.array([str(value) for value in accounts["segment"]]),
            "date": pa.array([value.isoformat() for value in accounts["date"]]),
            **{
                name: pa.array([float(value) for value in accounts[name]])
                for name in _ACCOUNT_SCHEMA.names[2:]
            },
        },
        schema=_ACCOUNT_SCHEMA,
    )


def _nullable(values: pd.Series, dtype: str, type: pa.DataType) -> pa.Array:
    """A pandas nullable column as a pyarrow array with nulls preserved."""
    mask = values.notna().to_numpy()
    plain = values.to_numpy(dtype=dtype, na_value=False if dtype == "bool" else 0)
    return pa.array(plain, mask=~mask, type=type)


def random_table(result: pd.DataFrame) -> pa.Table:
    """The random.parquet table built from a random_control frame."""
    return pa.table(
        {
            "round": pa.array(result["round"].to_numpy()),
            "day_index": pa.array(result["day_index"].to_numpy()),
            "leg": pa.array(result["leg"].to_numpy(), type=pa.string()),
            "K": pa.array(result["K"].to_numpy()),
            "start_min": _nullable(result["start_min"], "int64", pa.int64()),
            "exit_min": _nullable(result["exit_min"], "int64", pa.int64()),
            "stopped": _nullable(result["stopped"], "bool", pa.bool_()),
            "a": _nullable(result["a"], "float64", pa.float64()),
            "c": _nullable(result["c"], "float64", pa.float64()),
            "valid": pa.array(result["valid"].to_numpy()),
        },
        schema=_RANDOM_SCHEMA,
    )


def iso_timestamp(text: str) -> str:
    """A cleaned-bar stamp "YYYYMMDD HHMMSS" as "YYYY-MM-DDTHH:MM:SS"."""
    stamp = text.replace(" ", "T")
    return (
        f"{stamp[:4]}-{stamp[4:6]}-{stamp[6:8]}"
        f"T{stamp[9:11]}:{stamp[11:13]}:{stamp[13:15]}"
    )


def removed_table(removed: list[str]) -> pa.Table:
    """The removed_bars.parquet table built from clean_outliers stamps."""
    return pa.table(
        {
            "timestamp": pa.array(
                [iso_timestamp(value) for value in removed], type=pa.string()
            )
        }
    )


def verify_manifest(directory: Path) -> str:
    """Check SHA256SUMS against the result files; return its own SHA-256."""
    path = directory / "SHA256SUMS"
    lines = path.read_text().splitlines()
    if lines != [f"{digest(directory / name)}  {name}" for name in RESULT_FILES]:
        raise ValueError("invalid or mismatching results manifest")
    return digest(path)


def _input_hashes(histdata_dir: Path, spy: Path) -> dict[str, str]:
    """SHA-256 map of the dev-run input files: the dev zips and SPY.json."""
    hashes = {name: digest(histdata_dir / name) for name in STAGE_ZIPS["dev"]}
    hashes["SPY.json"] = digest(spy)
    return hashes


def require_holdout(
    dev_match: Path | None,
    ledger: Path | None,
    histdata_dir: Path,
    spy: Path,
) -> None:
    """Refuse the holdout stage without an authorized, matching dev record."""
    if dev_match is None or ledger is None:
        raise ValueError("holdout requires --dev-match and --ledger")
    envelope = json.loads(dev_match.read_text())
    if not isinstance(envelope, dict) or envelope.get("match") is not True:
        raise ValueError("dev comparison must match")
    results_digest = verify_manifest(dev_match.parent)
    if envelope.get("results_sha256") != results_digest:
        raise ValueError("dev envelope results hash mismatch")
    actual = json.loads((dev_match.parent / "summary.json").read_text())
    if any(
        compare_tree(envelope.get(key), actual, f"dev envelope {key}")
        for key in ("rust_summary", "python_summary")
    ):
        raise ValueError("dev envelope summary mismatch")
    records = [
        json.loads(line) for line in ledger.read_text().splitlines() if line.strip()
    ]
    if any(not isinstance(row, dict) for row in records):
        raise ValueError("invalid ledger record")
    records = [row for row in records if row.get("hypothesis") == "H006"]
    numbered = list(enumerate(records))
    if any(
        row.get("kind") == "run" and row.get("stage") == "holdout"
        for _, row in numbered
    ):
        raise ValueError("H006 holdout already used")
    dev_runs = [
        (index, row)
        for index, row in numbered
        if row.get("kind") == "run" and row.get("stage") == "dev"
    ]
    if not dev_runs:
        raise ValueError("missing H006 dev run")
    run_index, dev_run = dev_runs[-1]
    if dev_run.get("data_sha256") != _input_hashes(histdata_dir, spy):
        raise ValueError("H006 dev run input hashes mismatch")
    if dev_run.get("results_sha256") != results_digest:
        raise ValueError("H006 dev run results hash mismatch")
    dev_verdicts = [
        (index, row)
        for index, row in numbered
        if index > run_index
        and row.get("kind") == "verdict"
        and row.get("stage") == "dev"
    ]
    if not dev_verdicts:
        raise ValueError("missing H006 dev verdict")
    verdict_index, dev_verdict = dev_verdicts[-1]
    gates = dev_verdict.get("gates")
    if (
        dev_verdict.get("result") != "pass"
        or dev_verdict.get("results_sha256") != results_digest
        or not isinstance(gates, dict)
        or any(gates.get(gate) is not True for gate in ("A", "B", "C", "D"))
    ):
        raise ValueError("H006 dev ledger gates or results mismatch")
    if not any(
        index > verdict_index
        and row.get("kind") == "holdout_open"
        and row.get("stage") == "holdout"
        for index, row in numbered
    ):
        raise ValueError("missing H006 holdout_open")


def run(
    run_directory: Path,
    stage: str,
    histdata_dir: Path,
    spy: Path,
    dev_match: Path | None = None,
    ledger: Path | None = None,
) -> tuple[Path, str, bool]:
    """Compute everything independently and compare with the Rust run.

    Returns the verdict path, its hash and whether every key matched. A
    mismatch still writes a match=false verdict; the caller exits non-zero.
    """
    if stage == "holdout":
        require_holdout(dev_match, ledger, histdata_dir, spy)
    directory = checked_run_directory(run_directory)
    results_digest = verify_manifest(directory)
    bars = read_histdata(histdata_dir, stage)
    validate_clock(bars.stamp)
    cleaned, removed = clean_outliers(bars)
    clock = time_checks(cleaned)
    print("clock_check", json.dumps(clock_summary(clock)), flush=True)
    days, _ = computation_days(cleaned)
    excluded = excluded_days(bars.stamp, days)
    spy_rows = read_spy(spy, stage)
    frame = daily_legs(days)
    accounts, stats, gates = run_segments(frame, spy_rows, stage)
    control_table, control = random_control(frame, days)
    if control_table is None:
        raise ValueError("random control table missing")
    reports = segment_reports(frame, days, spy_rows, stage)
    summary = python_summary(
        stage,
        bars,
        cleaned,
        removed,
        clock,
        excluded,
        days,
        digest(spy),
        stats,
        gates,
        control,
        reports,
    )
    rust_summary = json.loads((directory / "summary.json").read_text())
    differences = compare_tree(summary, rust_summary)
    differences += compare_table(days_table(frame), directory / "days.parquet", "days")
    differences += compare_table(
        accounts_table(accounts), directory / "accounts.parquet", "accounts"
    )
    differences += compare_table(
        random_table(control_table), directory / "random.parquet", "random"
    )
    differences += compare_table(
        removed_table(removed), directory / "removed_bars.parquet", "removed_bars"
    )
    verdict = {
        "python_summary": summary,
        "rust_summary": rust_summary,
        "match": not differences,
        "mismatches": differences[:MISMATCH_CAP],
        "results_sha256": results_digest,
    }
    path, verdict_digest = write_verdict(directory, verdict)
    return path, verdict_digest, not differences


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--run", required=True, type=Path)
    parser.add_argument("--stage", required=True, choices=("dev", "holdout"))
    parser.add_argument("--histdata-dir", required=True, type=Path)
    parser.add_argument("--spy", required=True, type=Path)
    parser.add_argument("--dev-match", type=Path)
    parser.add_argument("--ledger", type=Path)
    args = parser.parse_args()
    try:
        path, verdict_digest, matched = run(
            args.run,
            args.stage,
            args.histdata_dir,
            args.spy,
            args.dev_match,
            args.ledger,
        )
    except (
        OSError,
        ValueError,
        TypeError,
        KeyError,
        ArithmeticError,
        AttributeError,
    ) as error:
        print(f"H006 input or computation failed: {error}", file=sys.stderr)
        return 1
    print(f"{path} sha256={verdict_digest} match={str(matched).lower()}")
    return 0 if matched else 1


if __name__ == "__main__":
    raise SystemExit(main())
