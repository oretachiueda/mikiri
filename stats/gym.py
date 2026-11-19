from collections.abc import Callable
from dataclasses import dataclass, replace
from datetime import date, timedelta
from hashlib import sha256
from math import sqrt
from pathlib import Path
from statistics import NormalDist
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
    counters = (
        np.arange(count, dtype=np.uint64)
        ^ np.uint64(seed)
        ^ h006.splitmix64(np.array([stream], dtype=np.uint64))[0]
    )
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


@dataclass(frozen=True)
class SwapCase:
    notional: float
    direction: str
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


REFERENCE = Pipeline(reference_swap, reference_gate_b)
MUTANTS = {
    "swap_sign_flipped": replace(REFERENCE, swap=lambda c: -reference_swap(c)),
    "swap_zero": replace(REFERENCE, swap=lambda c: 0.0),
    "gate_always_reject": replace(REFERENCE, gate_b=lambda r, n: False),
    "gate_annualized_sr": replace(REFERENCE, gate_b=annualized_gate),
    "gate_no_n": replace(REFERENCE, gate_b=lambda r, n: h001.psr(list(r)) >= 0.95),
}

CASE = SwapCase(100000, "short", 0.0525, 0.035, date(2024, 1, 1), date(2024, 1, 16))
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


SCENARIOS = {"G2": Scenario("G2", check_g2), "G3": Scenario("G3", check_g3)}
