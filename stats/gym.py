import argparse
import json
from collections.abc import Callable
from dataclasses import dataclass, replace
from datetime import date, timedelta
from functools import cache
from hashlib import sha256
from math import sqrt
from pathlib import Path
from statistics import NormalDist
from tempfile import TemporaryDirectory
from types import MappingProxyType
from zipfile import ZipFile

import numpy as np

import h001
import h006


@dataclass(frozen=True)
class Outcome:
    scenario: str
    passed: bool
    values: dict


@dataclass(frozen=True)
class Pipeline:
    swap: Callable
    gate_b: Callable
    trades: Callable
    width: Callable
    fill: Callable


@dataclass(frozen=True)
class Scenario:
    name: str
    check: Callable


def seed_from(paths: list[Path]) -> int:
    digest = sha256()
    for path in sorted(paths):
        digest.update(path.read_bytes())
    return int.from_bytes(digest.digest()[:8], "big")


def uniforms(seed, stream, count):
    key = h006.splitmix64(
        np.array(
            [np.uint64(seed) ^ h006.splitmix64(np.array([stream], dtype=np.uint64))[0]]
        )
    )[0]
    counters = key + np.arange(count, dtype=np.uint64)
    return ((h006.splitmix64(counters) >> np.uint64(11)).astype(np.float64) + 1) / (
        2**53 + 1
    )


def normals(seed, stream, count):
    u = uniforms(seed, stream, 2 * count)
    return np.sqrt(-2 * np.log(u[:count])) * np.cos(2 * np.pi * u[count:])


def write_histdata(directory, pair, year, stamps, open, high, low, close):
    member = f"DAT_ASCII_{pair}_M1_{year}.csv"
    path = directory / f"{pair}_{year}.zip"
    lines = (
        f"{h006._stamp_text(int(s))};{o};{h};{l};{c};0\r\n"
        for s, o, h, l, c in zip(stamps, open, high, low, close, strict=True)
    )
    with ZipFile(path, "w") as archive:
        archive.writestr(member, "".join(lines))
    return path


def read_histdata_zip(path, pair, year):
    member = f"DAT_ASCII_{pair}_M1_{year}.csv"
    return h006._parse_zip(
        path.name, h006._zip_lines(h006._zip_text(path, member), b"99999999")
    )


def synthetic_m1(seed, path, minutes, substeps, step, start):
    prices = start + np.cumsum(normals(seed, path, minutes * substeps) * step)
    prices = prices.reshape(minutes, substeps)
    opens = np.concatenate(([start], prices[:-1, -1]))
    assert np.all(prices > 0) and start > 0
    days = np.arange(
        (minutes // 1440 // 5 + 2) * 7, dtype="timedelta64[D]"
    ) + np.datetime64("2024-01-01")
    days = days[np.arange(len(days)) % 7 < 5]
    dates = np.char.replace(days.astype(str), "-", "").astype(np.int64)
    stamps = (
        dates[:, None] * 1000000
        + np.arange(1440)[None, :] // 60 * 10000
        + np.arange(1440)[None, :] % 60 * 100
    ).ravel()[:minutes]
    return {
        "stamp": np.array(stamps),
        "open": opens,
        "high": np.maximum(opens, prices.max(axis=1)),
        "low": np.minimum(opens, prices.min(axis=1)),
        "close": prices[:, -1],
    }


def hourly(m1):
    hours = m1["stamp"] // 10000 * 10000
    stamps, starts = np.unique(hours, return_index=True)
    ends = np.r_[starts[1:], len(hours)]
    return {
        "stamp": stamps,
        "open": m1["open"][starts],
        "high": np.maximum.reduceat(m1["high"], starts),
        "low": np.minimum.reduceat(m1["low"], starts),
        "close": m1["close"][ends - 1],
    }


@dataclass(frozen=True)
class Trade:
    entry: int
    direction: int
    entry_price: float
    stop: float | None
    exit: int
    exit_price: float


def _trades(bars, config, offset):
    trades = []
    i = 1
    while i + config["hold"] < len(bars["stamp"]):
        signal = int(np.sign(bars["close"][i - offset] - bars["open"][i - offset]))
        if signal:
            end = i + config["hold"]
            trades.append(
                Trade(
                    i,
                    config["sign"] * signal,
                    bars["open"][i],
                    None,
                    end,
                    bars["open"][end],
                )
            )
            i = end
        else:
            i += 1
    return trades


def momentum_trades(bars, config):
    return _trades(bars, config, 1)


def signal_uses_entry_bar(bars, config):
    return _trades(bars, config, 0)


def daily_returns(bars, trades):
    days, inverse = np.unique(bars["stamp"] // 1000000, return_inverse=True)
    returns = np.zeros(len(days))
    for trade in trades:
        returns[inverse[trade.exit]] += (
            (trade.exit_price - trade.entry_price) * trade.direction / trade.entry_price
        )
    return returns


PATHS = 20
MINUTES = 6 * 5 * 24 * 60
SUBSTEPS = 4
STEP = 0.00001
START = 1.0
H = 4


@cache
def paths_for_seed(seed):
    paths = []
    with TemporaryDirectory() as directory:
        for path in range(PATHS):
            m1 = synthetic_m1(seed, path, MINUTES, SUBSTEPS, STEP, START)
            years = m1["stamp"] // 10000000000
            parts = []
            for year in np.unique(years):
                mask = years == year
                filename = write_histdata(
                    Path(directory),
                    "EURUSD",
                    int(year),
                    *(
                        m1[key][mask]
                        for key in ("stamp", "open", "high", "low", "close")
                    ),
                )
                parts.append(read_histdata_zip(filename, "EURUSD", int(year)))
            bars = hourly(
                {key: np.concatenate([part[key] for part in parts]) for key in m1}
            )
            for values in bars.values():
                values.setflags(write=False)
            paths.append(MappingProxyType(bars))
    return tuple(paths)


def check_g1(pipeline, seed):
    means = [[], []]
    for bars in paths_for_seed(seed):
        for cell, sign in enumerate((1, -1)):
            trades = pipeline.trades(bars, {"sign": sign, "hold": H})
            means[cell].append(daily_returns(bars, trades).mean())
    values = {
        str(sign): float(np.mean(sample) / (np.std(sample, ddof=1) / sqrt(PATHS)))
        for sign, sample in zip((1, -1), means, strict=True)
    }
    return Outcome("G1", all(abs(value) < 3 for value in values.values()), values)


def stop_width(bars, i):
    return np.mean(bars["high"][i - 20 : i] - bars["low"][i - 20 : i])


def stop_width_includes_entry_bar(bars, i):
    return np.mean(bars["high"][i - 19 : i + 1] - bars["low"][i - 19 : i + 1])


def next_open_fill(bars, j, level, direction):
    return bars["open"][j + 1]


def stop_fill_at_level(bars, j, level, direction):
    return level


def stop_trades(bars, config, pipeline, start=20, single=False):
    trades = []
    i = start
    while i + config["hold"] < len(bars["stamp"]):
        direction = int(np.sign(bars["close"][i - 1] - bars["open"][i - 1]))
        if not direction:
            i += 1
            continue
        entry = bars["open"][i]
        stop = entry - direction * config["k"] * pipeline.width(bars, i)
        end = i + config["hold"]
        price = bars["open"][end]
        for j in range(i, end):
            if (direction > 0 and bars["low"][j] <= stop) or (
                direction < 0 and bars["high"][j] >= stop
            ):
                end = j + 1
                price = pipeline.fill(bars, j, stop, direction)
                break
        trades.append(Trade(i, direction, entry, stop, end, price))
        i = end
        if single:
            break
    return trades


K = 0.1
STOP_HOLD = 1


def check_g4(pipeline, seed):
    samples = [[], []]
    for bars in paths_for_seed(seed):
        for sample, candidate in zip(
            samples, (pipeline, replace(pipeline, fill=stop_fill_at_level)), strict=True
        ):
            sample.append(
                daily_returns(
                    bars, stop_trades(bars, {"k": K, "hold": STOP_HOLD}, candidate)
                ).mean()
            )
    ts = [float(np.mean(x) / (np.std(x, ddof=1) / sqrt(PATHS))) for x in samples]
    return Outcome(
        "G4", abs(ts[0]) < 3 and abs(ts[1]) >= 3, {"reference": ts[0], "level": ts[1]}
    )


def check_g5(pipeline, seed):
    checked = changed = 0
    for bars in paths_for_seed(seed)[:2]:
        config = {"k": K, "hold": STOP_HOLD}
        for trade in stop_trades(bars, config, pipeline):
            altered = {key: value.copy() for key, value in bars.items()}
            i = trade.entry
            for key in ("open", "high", "low", "close"):
                start = i + (key == "open")
                altered[key][start:] = trade.entry_price + 1.7 * (
                    altered[key][start:] - trade.entry_price
                )
            other = next(
                (
                    t
                    for t in stop_trades(altered, config, pipeline, i, True)
                    if t.entry == i
                ),
                None,
            )
            checked += 1
            changed += other is None or (
                other.entry,
                other.direction,
                other.entry_price,
                other.stop,
            ) != (trade.entry, trade.direction, trade.entry_price, trade.stop)
    return Outcome("G5", changed == 0, {"checked": checked, "changed": changed})


@dataclass(frozen=True)
class SwapCase:
    notional: float
    held_rate: float
    sold_rate: float
    entry: date
    exit: date
    markup: float = 0.0025


def reference_swap(case):
    day = case.entry
    total = 0.0
    while day < case.exit:
        if day.weekday() < 5:
            days = 3 if day.weekday() == 4 else 1
            total += (
                case.notional
                * (case.held_rate - case.sold_rate - case.markup)
                / 365
                * days
            )
        day += timedelta(days=1)
    return total


def reference_gate_b(returns, n_cumulative):
    return h001.psr(list(returns)) >= 1 - 0.05 / (n_cumulative + 1)


def annualized_gate(returns, n_cumulative):
    r = np.asarray(returns)
    sr = r.mean() / r.std() * sqrt(252)
    z = sr * sqrt(len(r) - 1) / sqrt(1 + sr**2 / 2)
    return NormalDist().cdf(z) >= 1 - 0.05 / (n_cumulative + 1)


REFERENCE = Pipeline(
    reference_swap, reference_gate_b, momentum_trades, stop_width, next_open_fill
)
MUTANTS = {
    "stop_fill_at_level": replace(REFERENCE, fill=stop_fill_at_level),
    "stop_width_includes_entry_bar": replace(
        REFERENCE, width=stop_width_includes_entry_bar
    ),
    "signal_uses_entry_bar": replace(REFERENCE, trades=signal_uses_entry_bar),
    "swap_sign_flipped": replace(REFERENCE, swap=lambda c: -reference_swap(c)),
    "swap_zero": replace(REFERENCE, swap=lambda c: 0.0),
    "gate_always_reject": replace(REFERENCE, gate_b=lambda r, n: False),
    "gate_annualized_sr": replace(REFERENCE, gate_b=annualized_gate),
    "gate_no_n": replace(REFERENCE, gate_b=lambda r, n: h001.psr(list(r)) >= 0.95),
}

CASE = SwapCase(100000, 0.0525, 0.035, date(2024, 1, 1), date(2024, 1, 16))
# 100000 * (0.0525 - 0.035 - 0.0025) / 365 * 15.
ORACLE = 61.64383561643836
M = 1000
T = 2520
N = 252
SR = 1 / sqrt(252)
Z = NormalDist().inv_cdf(1 - 0.05 / (N + 1))
CUT = Z / sqrt(T - 1 - Z**2 / 2)
EXPECTED = 1 - NormalDist(SR, sqrt((1 + SR**2 / 2) / T)).cdf(CUT)
# Allow 0.03 for the normal approximation to the finite-sample PSR distribution.
BAND = 4 * sqrt(EXPECTED * (1 - EXPECTED) / M) + 0.03


def check_g2(pipeline, seed):
    value = pipeline.swap(CASE)
    return Outcome(
        "G2",
        abs(value - ORACLE) <= 1e-12 and value * ORACLE > 0,
        {"swap": value, "oracle": ORACLE},
    )


def check_g3(pipeline, seed):
    rate = sum(pipeline.gate_b(normals(seed, i, T) + SR, N) for i in range(M)) / M
    return Outcome(
        "G3",
        abs(rate - EXPECTED) <= BAND,
        {"rate": rate, "expected": EXPECTED, "band": BAND},
    )


SCENARIOS = {
    "G1": Scenario("G1", check_g1),
    "G2": Scenario("G2", check_g2),
    "G3": Scenario("G3", check_g3),
    "G4": Scenario("G4", check_g4),
    "G5": Scenario("G5", check_g5),
}


@dataclass(frozen=True)
class HiddenCase:
    name: str
    scenario: str
    pipeline: Pipeline
    expected_pass: bool


def seeded_files():
    root = Path(__file__).resolve().parent
    return [
        root / "gym.py",
        *sorted(
            p
            for p in (root / "gym_hidden").rglob("*")
            if p.is_file() and "__pycache__" not in p.parts
        ),
    ]


def main(argv=None):
    parser = argparse.ArgumentParser()
    parser.add_argument("--mutant", choices=MUTANTS)
    args = parser.parse_args(argv)
    files = seeded_files()
    seed = seed_from(files)
    pipeline = MUTANTS[args.mutant] if args.mutant else REFERENCE
    outcomes = {
        name: scenario.check(pipeline, seed) for name, scenario in SCENARIOS.items()
    }
    print(
        json.dumps(
            {
                "seed": seed,
                "files": {
                    str(p.relative_to(Path(__file__).resolve().parent)): sha256(
                        p.read_bytes()
                    ).hexdigest()
                    for p in files
                },
                "scenarios": {
                    name: {"passed": o.passed, "values": o.values}
                    for name, o in outcomes.items()
                },
            }
        )
    )
    return int(not all(o.passed for o in outcomes.values()))


if __name__ == "__main__":
    raise SystemExit(main())
