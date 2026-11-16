#!/usr/bin/env python3
"""Write synthetic HistData-format EUR/USD and Tiingo SPY inputs."""

import argparse
import datetime as dt
import json
import math
import zipfile
from pathlib import Path


class NonSeekable:
    def __init__(self, file):
        self.file = file

    def write(self, data):
        return self.file.write(data)

    def tell(self):
        return self.file.tell()

    def flush(self):
        return self.file.flush()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--seed", type=int, default=0)
    args = parser.parse_args()
    root = args.out
    dest = root / "zips"
    dest.mkdir(parents=True, exist_ok=False)
    start = dt.date(2007, 1, 1)
    end = dt.date(2026, 10, 30)
    periods = [
        (str(y), dt.date(y, 1, 1), dt.date(y, 12, 31)) for y in range(2007, 2026)
    ]
    periods += [
        (
            f"2026{m:02d}",
            dt.date(2026, m, 1),
            min(dt.date(2026, m + 1, 1) - dt.timedelta(days=1), end),
        )
        for m in range(1, 11)
    ]
    for ident, first, last in periods:
        path = dest / f"HISTDATA_COM_ASCII_EURUSD_M1_{ident}.zip"
        with (
            path.open("wb") as raw,
            zipfile.ZipFile(
                NonSeekable(raw),
                "w",
                compression=zipfile.ZIP_DEFLATED,
                compresslevel=3,
                allowZip64=True,
            ) as z,
        ):
            csv_info = zipfile.ZipInfo(f"DAT_ASCII_EURUSD_M1_{ident}.csv")
            csv_info.compress_type = zipfile.ZIP_DEFLATED
            with z.open(csv_info, "w", force_zip64=True) as f:
                date = first
                while date <= last:
                    wd = date.weekday()
                    day = (date - start).days
                    if wd == 5 or date == start or (date.month, date.day) == (12, 25):
                        date += dt.timedelta(days=1)
                        continue
                    begin = 1020 if wd == 6 else 0
                    stop = 1020 if wd == 4 else 1440
                    phase = args.seed * 0.001
                    amp = 0.00020 + 0.00013 * (1 + math.sin(day * 0.113 + phase))
                    base = (
                        1.12
                        + 0.022 * math.sin(day * 0.013 + phase)
                        + 0.0007 * math.sin(day * 0.29)
                    )
                    lines = []
                    for m in range(begin, stop):
                        if (day % 97 == 0 and m in (200, 650)) or (
                            date == dt.date(2012, 1, 3) and 115 <= m <= 120
                        ):
                            continue
                        value = (
                            base
                            + amp * math.sin(m * 0.016 + day * 0.17)
                            + 0.00003 * math.sin(m * 0.031 + day * 0.43)
                        )
                        if 511 <= m < 561:
                            value += 0.0025 * (561 - m) / 50
                        op = round(value, 6)
                        high, low = op + 0.000045, op - 0.000045
                        if day % 997 == 1 and m == 720:
                            high = op * 1.05
                        lines.append(
                            f"{date:%Y%m%d} {m // 60:02d}{m % 60:02d}00;{op:.6f};{high:.6f};{low:.6f};{op:.6f};0\n"
                        )
                    f.write("".join(lines).encode())
                    date += dt.timedelta(days=1)
            text_info = zipfile.ZipInfo(f"DAT_ASCII_EURUSD_M1_{ident}.txt")
            text_info.compress_type = zipfile.ZIP_DEFLATED
            z.writestr(text_info, "synthetic fixture\n")
        print(ident, flush=True)
    rows = []
    date = start
    while date <= end:
        if date.weekday() < 5 and date != start and (date.month, date.day) != (12, 25):
            i = (date - start).days
            price = 100 * math.exp(
                i * 0.00012 + 0.01 * math.sin(i * 0.02 + args.seed * 0.001)
            )
            rows.append(
                {
                    "date": f"{date.isoformat()}T00:00:00Z",
                    "close": round(price, 6),
                    "divCash": 0,
                    "splitFactor": 1,
                }
            )
        date += dt.timedelta(days=1)
    (root / "SPY.json").write_text(json.dumps(rows))


if __name__ == "__main__":
    main()
