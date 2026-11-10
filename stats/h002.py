"""Recompute the H002 earnings announcement screen from Rust's monthly returns."""

import argparse
import json
import math
import re
import sys
from datetime import date
from pathlib import Path
from statistics import fmean

import pandas as pd
import pyarrow as pa
import pyarrow.parquet as pq

from h001 import checked_run_directory, psr, write_verdict

PORTS = ("p01", "p02", "p03", "p04", "p05")
MAIN_MONTHS = [
    (year, month)
    for year in range(1994, 2025)
    for month in range(1, 13)
    if (year, month) <= (2024, 10)
]
RECENT_START = (2014, 11)
PSR_THRESHOLD = 0.975
ANNUAL_THRESHOLD = 10.0
ANNUAL_COST2X_THRESHOLD = 14.0
LS_TOLERANCE = 1e-6
RETURN_TOLERANCE = 1e-12
PSR_TOLERANCE = 1e-9


def read_months(path: Path) -> list[dict]:
    schema = pq.read_schema(path)
    required = {
        "month_end": pa.date32(),
        "period": pa.string(),
        "recent": pa.bool_(),
        **dict.fromkeys((*PORTS, "ls", "e"), pa.float64()),
    }
    if len(schema) != len(required) or any(
        name not in schema.names or schema.field(name).type != kind
        for name, kind in required.items()
    ):
        raise ValueError("invalid months schema")
    return pd.read_parquet(path).to_dict(orient="records")


def compute(rows: list[dict]) -> tuple[dict, list[str]]:
    periods: dict[str, list[dict]] = {"main": [], "reference": []}
    recent = []
    seen: set[tuple[int, int]] = set()
    main_months = []
    differences = []
    for i, row in enumerate(rows):
        day = row["month_end"]
        if type(day) is not date:
            raise ValueError("invalid month_end")
        month = day.year, day.month
        if month in seen or month > (2024, 10):
            raise ValueError("duplicate or out-of-range month")
        seen.add(month)
        period = "main" if month >= (1994, 1) else "reference"
        is_recent = month >= RECENT_START
        ports = [row[key] for key in PORTS]
        if not all(math.isfinite(value) for value in (*ports, row["ls"])):
            raise ValueError("non-finite portfolio return")
        if abs(row["ls"] - (ports[-1] - ports[0])) > LS_TOLERANCE:
            raise ValueError("LS does not match 05 minus 01")
        e = ports[-1] - fmean(ports)
        if not math.isfinite(e):
            raise ValueError("non-finite excess return")
        for key, value in {"period": period, "recent": is_recent, "e": e}.items():
            if not matches(value, row[key], RETURN_TOLERANCE):
                differences.append(f"months[{i}].{key}: mismatch")
        calculated = row | {"e": e}
        periods[period].append(calculated)
        if period == "main":
            main_months.append(month)
        if is_recent:
            recent.append(calculated)

    if sorted(main_months) != MAIN_MONTHS or len(recent) != 120:
        raise ValueError("expected 370 consecutive main and 120 recent months")
    main, reference = periods["main"], periods["reference"]
    main_e = [row["e"] for row in main]
    if all(e == main_e[0] for e in main_e):
        raise ValueError("PSR variance must be positive and finite")
    mean_main, mean_recent = fmean(main_e), fmean(row["e"] for row in recent)
    probability = psr(main_e)
    annual_recent = mean_recent * 12

    def port_means(months: list[dict]) -> list[float]:
        return [fmean(row[key] for row in months) for key in PORTS]

    return {
        "main_n": len(main),
        "recent_n": len(recent),
        "reference_n": len(reference),
        "mean_e_main": mean_main,
        "mean_e_recent": mean_recent,
        "mean_e_reference": fmean(row["e"] for row in reference) if reference else None,
        "psr_main": probability,
        "annual_e_recent": annual_recent,
        "gate_a": mean_main > 0,
        "gate_b": probability >= PSR_THRESHOLD,
        "gate_c": mean_recent > 0,
        "gate_d": annual_recent >= ANNUAL_THRESHOLD,
        "gate_d_cost2x": annual_recent >= ANNUAL_COST2X_THRESHOLD,
        "mean_ls_main": fmean(row["ls"] for row in main),
        "mean_ls_recent": fmean(row["ls"] for row in recent),
        "port_means_main": port_means(main),
        "port_means_recent": port_means(recent),
        "port_means_reference": port_means(reference) if reference else None,
    }, differences


def matches(expected: object, actual: object, tolerance: float) -> bool:
    if isinstance(expected, float):
        return (
            isinstance(actual, (float, int))
            and not isinstance(actual, bool)
            and math.isfinite(actual)
            and abs(expected - actual) <= tolerance
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


def run(directory: Path) -> tuple[Path, str, bool]:
    directory = checked_run_directory(directory)
    calculated, differences = compute(read_months(directory / "months.parquet"))
    rust_summary = json.loads((directory / "summary.json").read_text())
    if not isinstance(rust_summary, dict):
        raise TypeError("invalid summary")
    # The monthly output omits the original bytes and excluded reference months.
    digest = rust_summary.get("input_sha256")
    excluded = rust_summary.get("reference_excluded")
    if not isinstance(digest, str) or re.fullmatch(r"[0-9a-f]{64}", digest) is None:
        differences.append("summary.input_sha256: invalid")
    if type(excluded) is not int or excluded < 0:
        differences.append("summary.reference_excluded: invalid")
    calculated.update(input_sha256=digest, reference_excluded=excluded)
    if calculated.keys() != rust_summary.keys():
        differences.append("summary: fields differ")
    for key, value in calculated.items():
        tolerance = PSR_TOLERANCE if key == "psr_main" else RETURN_TOLERANCE
        if not matches(value, rust_summary.get(key), tolerance):
            differences.append(f"summary.{key}: mismatch")
    matched = not differences
    path, verdict_hash = write_verdict(
        directory,
        {
            "python_summary": calculated,
            "rust_summary": rust_summary,
            "match": matched,
            "mismatches": differences,
        },
    )
    return path, verdict_hash, matched


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--run", required=True, type=Path)
    args = parser.parse_args()
    try:
        path, digest, matched = run(args.run)
    except (OSError, ValueError, TypeError, KeyError, ArithmeticError):
        print("H002 input or computation failed", file=sys.stderr)
        return 1
    print(f"{path} sha256={digest} match={str(matched).lower()}")
    return 0 if matched else 1


if __name__ == "__main__":
    sys.exit(main())
