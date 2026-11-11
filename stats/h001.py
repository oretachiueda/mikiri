"""Independently recompute the preregistered H001 statistics from daily bars."""

import argparse
import hashlib
import json
import math
import sys
import tempfile
from dataclasses import asdict, dataclass
from datetime import date
from itertools import pairwise
from pathlib import Path
from statistics import NormalDist, fmean

import pandas as pd
import pyarrow as pa
import pyarrow.parquet as pq

END = date(2026, 10, 30)
COST_PER_DAY = 0.0004 / 4
Summary = dict[str, float | int | bool | None]


@dataclass(frozen=True)
class Bar:
    date: date
    close: float
    div_cash: float
    split_factor: float


@dataclass(frozen=True)
class Cycle:
    plus1: date
    period: str
    recent: bool
    in_days: int
    out_days: int
    in_mean: float
    out_mean: float
    d: float


def read_bars(path: Path) -> list[Bar]:
    schema = pq.read_schema(path)
    required = {"date", "close", "div_cash", "split_factor"}
    if set(schema.names) != required or len(schema.names) != len(required):
        raise ValueError("invalid bars columns")
    if not pa.types.is_date(schema.field("date").type) or any(
        not pa.types.is_floating(schema.field(name).type)
        for name in required - {"date"}
    ):
        raise ValueError("invalid bars types")
    frame = pd.read_parquet(path)
    if frame.isna().any().any():
        raise ValueError("null bar value")
    bars = [Bar(**row) for row in frame.to_dict(orient="records")]
    validate_bars(bars)
    return bars


def validate_bars(bars: list[Bar]) -> None:
    if not bars:
        raise ValueError("empty bars")
    if any(a.date >= b.date for a, b in pairwise(bars)):
        raise ValueError("dates must be strictly increasing")
    if any(
        not all(math.isfinite(x) for x in (b.close, b.div_cash, b.split_factor))
        or b.close <= 0
        or b.div_cash < 0
        or b.split_factor <= 0
        for b in bars
    ):
        raise ValueError("invalid bar value")


def cycles(bars: list[Bar]) -> list[Cycle]:
    validate_bars(bars)
    returns = {
        b.date: (b.close * b.split_factor + b.div_cash) / a.close - 1
        for a, b in pairwise(bars)
    }
    if not all(math.isfinite(r) for r in returns.values()):
        raise ValueError("non-finite daily return")
    months: dict[tuple[int, int], list[int]] = {}
    for i, bar in enumerate(bars):
        months.setdefault((bar.date.year, bar.date.month), []).append(i)
    result = []
    for indices in months.values():
        first = indices[0]
        plus1 = bars[first].date
        if (
            first < 2
            or plus1 > END
            or len(indices) < 3
            or (indices[-1] == len(bars) - 1 and bars[-1].date < END)
        ):
            continue
        start = first - 1
        # The last preregistered cycle ends before October 30, even without November.
        stop = next((i for i in indices if bars[i].date >= END), indices[-1])
        if stop <= start + 4:
            continue
        inside = [returns[b.date] for b in bars[start : first + 3]]
        outside = [returns[b.date] for b in bars[first + 3 : stop]]
        in_mean, out_mean = fmean(inside), fmean(outside)
        result.append(
            Cycle(
                plus1=plus1,
                period="main" if plus1 >= date(2006, 1, 1) else "reference",
                recent=plus1 >= date(2016, 11, 1),
                in_days=len(inside),
                out_days=len(outside),
                in_mean=in_mean,
                out_mean=out_mean,
                d=in_mean - COST_PER_DAY - out_mean,
            )
        )
    return result


def psr(values: list[float]) -> float:
    if len(values) < 2 or not all(math.isfinite(x) for x in values):
        raise ValueError("PSR needs at least two finite observations")
    avg = fmean(values)
    m2, m3, m4 = (fmean((x - avg) ** j for x in values) for j in (2, 3, 4))
    if m2 <= 0 or not math.isfinite(m2):
        raise ValueError("PSR variance must be positive and finite")
    sr = avg / math.sqrt(m2)
    g3, g4 = m3 / m2**1.5, m4 / m2**2
    denominator = 1 - g3 * sr + (g4 - 1) / 4 * sr**2
    if denominator <= 0 or not math.isfinite(denominator):
        raise ValueError("invalid PSR denominator")
    value = NormalDist().cdf(sr * math.sqrt(len(values) - 1) / math.sqrt(denominator))
    if not math.isfinite(value):
        raise ValueError("non-finite PSR")
    return value


def compute(bars: list[Bar]) -> tuple[list[Cycle], Summary]:
    validate_bars(bars)
    if bars[0].date > date(2005, 12, 29) or bars[-1].date < END:
        raise ValueError("insufficient date coverage")
    if any(
        a.date >= date(2005, 12, 1)
        and b.date <= date(2026, 10, 31)
        and (b.date - a.date).days > 5
        for a, b in pairwise(bars)
    ):
        raise ValueError("calendar gap exceeds five days")
    calculated = cycles(bars)
    main = [c for c in calculated if c.period == "main"]
    recent = [c.d for c in main if c.recent]
    reference = [c.d for c in calculated if c.period == "reference"]
    if len(main) != 250 or len(recent) != 120:
        raise ValueError("expected 250 main and 120 recent cycles")
    mean_main = fmean(c.d for c in main)
    mean_recent = fmean(recent)
    probability = psr([c.d for c in main])
    return calculated, {
        "main_n": len(main),
        "recent_n": len(recent),
        "mean_d_main": mean_main,
        "mean_d_recent": mean_recent,
        "psr_main": probability,
        "gate_a": mean_main > 0,
        "gate_b": probability >= 0.95,
        "gate_c": mean_recent > 0,
        "in_mean_main": fmean(c.in_mean for c in main),
        "out_mean_main": fmean(c.out_mean for c in main),
        "mean_d_main_cost2x": fmean(
            c.in_mean - 2 * COST_PER_DAY - c.out_mean for c in main
        ),
        "mean_d_reference": fmean(reference) if reference else None,
        "reference_n": len(reference),
    }


def compare(
    calculated: list[Cycle],
    summary: Summary,
    rust_cycles: list[dict],
    rust_summary: dict,
) -> list[str]:
    differences = []

    def check(expected: dict, actual: dict, label: str) -> None:
        if expected.keys() != actual.keys():
            differences.append(f"{label}: fields differ")
        for name, value in expected.items():
            other = actual.get(name)
            if isinstance(value, float):
                equal = (
                    isinstance(other, (float, int))
                    and not isinstance(other, bool)
                    and math.isfinite(other)
                    and abs(value - other) <= 1e-12
                )
            else:
                equal = type(value) is type(other) and value == other
            if not equal:
                differences.append(f"{label}.{name}: mismatch")

    if len(calculated) != len(rust_cycles):
        differences.append("cycles: row count differs")
    for i, (cycle, rust) in enumerate(zip(calculated, rust_cycles, strict=False)):
        check(asdict(cycle), rust, f"cycles[{i}]")
    check(summary, rust_summary, "summary")
    return differences


def checked_run_directory(directory: Path) -> Path:
    directory = directory.resolve(strict=True)
    results = Path(__file__).resolve().parents[1] / "results"
    if not directory.is_relative_to(results) or directory == results:
        raise ValueError("run directory must be under repository results/")
    return directory


def write_verdict(directory: Path, verdict: dict) -> tuple[Path, str]:
    payload = (json.dumps(verdict, indent=2, allow_nan=False) + "\n").encode()
    target = directory / "verdict.json"
    scratch = None
    try:
        with tempfile.NamedTemporaryFile(dir=directory, delete=False) as temp:
            scratch = Path(temp.name)
            temp.write(payload)
        scratch.replace(target)
    finally:
        if scratch is not None:
            scratch.unlink(missing_ok=True)
    return target, hashlib.sha256(payload).hexdigest()


def run(directory: Path) -> tuple[Path, str, bool]:
    directory = checked_run_directory(directory)
    calculated, summary = compute(read_bars(directory / "bars.parquet"))
    rust_summary = json.loads((directory / "summary.json").read_text())
    rust_cycles = pd.read_parquet(directory / "cycles.parquet").to_dict(
        orient="records"
    )
    differences = compare(calculated, summary, rust_cycles, rust_summary)
    verdict = {
        "python_summary": summary,
        "rust_summary": rust_summary,
        "match": not differences,
        "mismatches": differences,
    }
    path, digest = write_verdict(directory, verdict)
    return path, digest, not differences


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--run", required=True, type=Path)
    args = parser.parse_args()
    try:
        path, digest, matched = run(args.run)
    except (OSError, ValueError, TypeError, KeyError):
        print("H001 input or computation failed", file=sys.stderr)
        return 1
    print(f"{path} sha256={digest} match={str(matched).lower()}")
    return 0 if matched else 1


if __name__ == "__main__":
    sys.exit(main())
