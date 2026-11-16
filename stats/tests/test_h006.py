import hashlib
import io
import json
import math
import statistics
import sys
import zipfile
from dataclasses import replace
from datetime import date, datetime, timedelta

import numpy as np
import pandas as pd
import pyarrow as pa
import pyarrow.parquet as pq
import pytest

import h006 as h


class Unseekable(io.RawIOBase):
    def __init__(self, stream):
        self.stream = stream

    def writable(self):
        return True

    def write(self, data):
        return self.stream.write(data)


def line(day, minute, price):
    clock = f"{minute // 60:02d}{minute % 60:02d}00"
    return f"{day:08d} {clock};{price:.6f};{price:.6f};{price:.6f};{price:.6f};0"


def write_zip(directory, name, lines, csv_name=None):
    tag = name[len("HISTDATA_COM_ASCII_EURUSD_M1_") : -len(".zip")]
    member = csv_name or f"DAT_ASCII_EURUSD_M1_{tag}.csv"
    path = directory / name
    with (
        open(path, "wb") as raw,
        zipfile.ZipFile(Unseekable(raw), "w", zipfile.ZIP_DEFLATED) as archive,
    ):
        archive.writestr(member, "".join(text + "\n" for text in lines))
        archive.writestr(member[:-4] + ".txt", "")
    return path


def year_lines(year, day=15, count=3):
    return [
        line(year * 10000 + 100 + day, 600 + i, 1.30 + i * 0.0001) for i in range(count)
    ]


@pytest.fixture(scope="module")
def histdata(tmp_path_factory):
    directory = tmp_path_factory.mktemp("histdata")
    for year in range(2007, 2024):
        write_zip(directory, f"{h.ZIP_PREFIX}{year}.zip", year_lines(year))
    write_zip(
        directory,
        f"{h.ZIP_PREFIX}2024.zip",
        [
            line(20240116, 600, 1.30),
            line(20241031, 600, 1.31),
            "20261225 0900;garbage",
            line(20241231, 600, 1.32),
        ],
    )
    for month in (1, 5, 10):
        write_zip(
            directory,
            f"{h.ZIP_PREFIX}2026{month:02d}.zip",
            [line(20260000 + month * 100 + 20, 600, 1.33)],
        )
    write_zip(directory, f"{h.ZIP_PREFIX}2025.zip", year_lines(2025))
    for month in (2, 3, 4, 6, 7, 8, 9):
        write_zip(directory, f"{h.ZIP_PREFIX}2026{month:02d}.zip", [])
    return directory


def test_splitmix64_reference_values():
    values = np.array([0, 1, 2, 2**32, 999 * 2**32 + 8801], dtype=np.uint64)
    assert h.splitmix64(values).tolist() == [
        13866863570256181660,
        8826110057919892546,
        17342297256639978221,
        5572763319858028600,
        11747482445946130720,
    ]


def test_zip_fixture_uses_data_descriptors(histdata):
    path = histdata / f"{h.ZIP_PREFIX}2007.zip"
    flags = int.from_bytes(path.read_bytes()[6:8], "little")
    assert flags & 0x0008
    with zipfile.ZipFile(path) as archive:
        assert archive.getinfo("DAT_ASCII_EURUSD_M1_2007.csv").compress_type == (
            zipfile.ZIP_DEFLATED
        )


def test_read_dev_opens_only_2024_zips_and_cuts_rows(histdata):
    bars = h.read_histdata(histdata, "dev")
    assert list(bars.sha256) == sorted(h.DEV_ZIPS)
    for name, digest in bars.sha256.items():
        assert digest == hashlib.sha256((histdata / name).read_bytes()).hexdigest()
    stamps = bars.stamp.tolist()
    assert stamps[0] == 20070115 * 1000000 + 100000
    assert 20241031 * 1000000 + 100000 in stamps
    assert all(value // 1000000 <= 20241031 for value in stamps)
    assert bars.parsed == len(stamps) == 17 * 3 + 2
    holdout = h.read_histdata(histdata, "holdout")
    assert list(holdout.sha256) == sorted(h.ALL_ZIPS)
    assert holdout.parsed == bars.parsed + 7
    assert all(value // 1000000 <= 20261030 for value in holdout.stamp)


def test_read_rejects_unknown_and_missing_names(histdata, tmp_path):
    write_zip(histdata, "extra.zip", [])
    with pytest.raises(ValueError, match="unexpected file"):
        h.read_histdata(histdata, "dev")
    (histdata / "extra.zip").unlink()
    missing = tmp_path / "missing"
    missing.mkdir()
    for year in range(2007, 2024):
        write_zip(missing, f"{h.ZIP_PREFIX}{year}.zip", year_lines(year))
    with pytest.raises(ValueError, match="missing HistData zip: .*2024"):
        h.read_histdata(missing, "dev")


def test_read_rejects_missing_csv_member(tmp_path):
    directory = tmp_path / "member"
    directory.mkdir()
    for year in range(2007, 2025):
        if year == 2010:
            write_zip(
                directory,
                f"{h.ZIP_PREFIX}{year}.zip",
                year_lines(year),
                csv_name="DAT_ASCII_EURUSD_M1_2009.csv",
            )
        else:
            write_zip(directory, f"{h.ZIP_PREFIX}{year}.zip", year_lines(year))
    with pytest.raises(ValueError, match="missing CSV member"):
        h.read_histdata(directory, "dev")


@pytest.mark.parametrize(
    "bad_line,reason",
    [
        ("20240115 090000;1.3;1.3;1.3", "line"),
        ("20240115_090000;1.3;1.3;1.3;1.3;0", "timestamp"),
        ("20240115 090000;1.3;1.3;1.3;1.3;0;7", "line"),
        ("20240115 090000;1.3;1.3;1.3;0.0;0", "price"),
        ("20240115 090000;1.3;1.3;1.3;-1.0;0", "price"),
        ("20240115 090000;nan;1.3;1.3;1.3;0", "value"),
        ("20240115 090000;1.3;1.3;1.3;inf;0", "value"),
        ("20240115 090030;1.3;1.3;1.3;1.3;0", "seconds"),
    ],
)
def test_read_rejects_malformed_values(tmp_path, bad_line, reason):
    directory = tmp_path / f"bad-{reason}"
    directory.mkdir(exist_ok=True)
    for year in range(2007, 2025):
        lines = [bad_line] if year == 2012 else year_lines(year)
        write_zip(directory, f"{h.ZIP_PREFIX}{year}.zip", lines)
    with pytest.raises(ValueError, match=reason):
        h.read_histdata(directory, "dev")


def test_read_rejects_duplicate_and_out_of_order(tmp_path):
    directory = tmp_path / "order"
    directory.mkdir()
    base = year_lines(2008)
    for year, lines in (
        (2007, year_lines(2007)),
        (2008, [base[0]] + base),
    ):
        write_zip(directory, f"{h.ZIP_PREFIX}{year}.zip", lines)
    for year in range(2009, 2025):
        write_zip(directory, f"{h.ZIP_PREFIX}{year}.zip", year_lines(year))
    with pytest.raises(ValueError, match="duplicate"):
        h.read_histdata(directory, "dev")
    write_zip(directory, f"{h.ZIP_PREFIX}2008.zip", list(reversed(base)))
    with pytest.raises(ValueError, match="out of order"):
        h.read_histdata(directory, "dev")


def make_bars(rows):
    rows = sorted(rows, key=lambda row: row[0] * 1440 + row[1])
    stamp = np.array(
        [
            day * 1000000 + minute // 60 * 10000 + minute % 60 * 100
            for day, minute, *_ in rows
        ],
        dtype=np.int64,
    )
    values = {
        key: np.array([row[i] for row in rows], dtype=np.float64)
        for i, key in ((2, "open"), (3, "high"), (4, "low"), (5, "close"))
    }
    return h.Bars(stamp=stamp, sha256={}, parsed=len(rows), **values)


def flat(day, start, stop, price=1.30, skip=()):
    return [
        (day, minute, price, price, price, price)
        for minute in range(start, stop)
        if minute not in skip
    ]


def test_clean_outliers_rule_edges_and_original_neighbors():
    rows = flat(20240102, 0, 100)
    rows[0] = (20240102, 0, 9.99, 9.99, 9.99, 9.99)
    rows[99] = (20240102, 99, 0.01, 0.01, 0.01, 0.01)
    rows[50] = (20240102, 50, 1.40, 1.40, 1.40, 1.40)
    rows[60] = (20240102, 60, 1.3377, 1.3377, 1.3377, 1.3377)
    rows[61] = (20240102, 61, 1.3429, 1.3429, 1.3429, 1.3429)
    kept, removed = h.clean_outliers(make_bars(rows))
    assert removed == ["20240102 005000", "20240102 010100"]
    assert kept.stamp.size == len(rows) - 2
    rows[40] = (20240102, 40, 1.50, 1.50, 1.50, 1.50)
    rows[41] = (20240102, 41, 1.50, 1.50, 1.50, 1.50)
    _, removed = h.clean_outliers(make_bars(rows))
    assert removed == [
        "20240102 004000",
        "20240102 004100",
        "20240102 005000",
        "20240102 010100",
    ]


def test_clean_outliers_joins_years_at_the_boundary():
    rows = flat(20071231, 1300, 1345) + [(20080102, 3, 1.60, 1.60, 1.60, 1.60)]
    rows += flat(20080102, 4, 36)
    _, removed = h.clean_outliers(make_bars(rows))
    assert removed == ["20080102 000300"]


def season_bars(day, spike_at=(510, 511), jump=0.005):
    rows = flat(day, 420, 600, price=1.30)
    for position, minute in enumerate(spike_at):
        price = 1.30 + jump * position
        rows[minute - 420] = (day, minute, price, price, price, price)
    return rows


def set_open(rows, day, minute, price):
    position = next(
        i for i, row in enumerate(rows) if row[0] == day and row[1] == minute
    )
    rows[position] = (day, minute, price, price, price, price)


def test_time_checks_pass_with_0830_spike_and_1700_sundays():
    rows = []
    for day in (20240109, 20240110, 20240116, 20240117):
        rows += season_bars(day)
    for day in (20240709, 20240710, 20240716, 20240717):
        rows += season_bars(day)
    for day in (20240107, 20240114, 20240707, 20240714):
        rows += flat(day, 1020, 1030)
    report = h.time_checks(make_bars(rows))
    assert report["peak_minute"] == {"winter": 510, "summer": 510}
    assert report["sunday_median"] == {"winter": 1020.0, "summer": 1020.0}
    assert report["sundays"] == {"winter": 2, "summer": 2}
    assert report["top_minutes"]["winter"][0] == (
        510,
        pytest.approx(abs(np.log(1.305 / 1.30))),
    )


def test_time_checks_failures():
    quiet = flat(20240109, 420, 600) + flat(20240114, 1020, 1030)
    quiet += flat(20240709, 420, 600) + flat(20240714, 1020, 1030)
    with pytest.raises(ValueError, match="R1 minute check failed"):
        h.time_checks(make_bars(quiet))
    late = season_bars(20240109) + flat(20240114, 1140, 1150)
    late += season_bars(20240709) + flat(20240714, 1140, 1150)
    with pytest.raises(ValueError, match="R1 Sunday check failed"):
        h.time_checks(make_bars(late))
    tie = flat(20240109, 420, 600) + flat(20240709, 420, 600)
    for day in (20240109, 20240709):
        set_open(tie, day, 511, 1.305)
        set_open(tie, day, 521, 1.305)
    tie += flat(20240114, 1020, 1030) + flat(20240714, 1020, 1030)
    with pytest.raises(ValueError, match="R1 minute check failed"):
        h.time_checks(make_bars(tie))


def test_ny_jst_conversions_and_exit_minute():
    summer = datetime(2020, 7, 15, 14, 45, tzinfo=h.NY).astimezone(h.JST)
    assert summer == datetime(2020, 7, 16, 3, 45, tzinfo=h.JST)
    winter = datetime(2020, 1, 15, 13, 45, tzinfo=h.NY).astimezone(h.JST)
    assert winter == datetime(2020, 1, 16, 3, 45, tzinfo=h.JST)
    assert h.ny_minutes(date(2020, 7, 15))[1] == 885
    assert h.ny_minutes(date(2020, 1, 15))[1] == 825


def test_us_dst_calendar_rules():
    assert h.us_dst(date(2007, 3, 11)) and not h.us_dst(date(2007, 3, 10))
    assert h.us_dst(date(2007, 11, 3)) and not h.us_dst(date(2007, 11, 4))
    assert h.us_dst(date(2024, 3, 10)) and not h.us_dst(date(2024, 11, 3))
    assert not h.us_dst(date(2006, 3, 12))


def test_every_day_closes_at_jst_0345_outside_the_wide_spread_window():
    days = h.computation_days(
        make_bars(flat(20240108, 0, 1440) + flat(20240715, 0, 1440))
    )[0]
    assert days
    for day in days:
        close = datetime(
            day.date.year, day.date.month, day.date.day, tzinfo=h.NY
        ) + timedelta(minutes=day.us_exit_min)
        jst = close.astimezone(h.JST)
        assert (jst.hour, jst.minute) == (3, 45)
        assert not 4 * 60 <= jst.hour * 60 + jst.minute < 8 * 60


def test_summer_time_mismatch_weeks_shorten_the_european_leg():
    assert h.ny_minutes(date(2024, 3, 18))[0] == 180
    assert h.ny_minutes(date(2024, 10, 28))[0] == 180
    assert h.ny_minutes(date(2024, 6, 10))[0] == 120
    assert h.ny_minutes(date(2024, 1, 8))[0] == 120
    for day, eu in ((date(2024, 3, 18), 5), (date(2024, 6, 10), 6)):
        assert 480 - h.ny_minutes(day)[0] == eu * 60


def test_validate_clock_round_trip_and_fold_handling():
    good = flat(20240108, 0, 1440) + flat(20240610, 0, 1440)
    h.validate_clock(make_bars(good).stamp)
    ambiguous = flat(20241103, 60, 120)
    h.validate_clock(make_bars(ambiguous).stamp)
    gap = flat(20240310, 120, 240)
    with pytest.raises(ValueError, match="New York wall time"):
        h.validate_clock(make_bars(gap).stamp)


def test_boundary_price_fallback_picks_the_last_near_bar():
    minutes = [120] + [m for m in range(470, 490) if m != 480] + [825]
    rows = [(20240108, m, 1.30 + m * 0.0001, 1.30, 1.30, 1.30) for m in minutes]
    day = h.computation_days(make_bars(rows))[0][0]
    assert day.us_entry_min == 480
    assert day.us_entry_price == pytest.approx(1.30 + 479 * 0.0001)
    assert day.us_entry_fallback
    assert day.eu_entry_price == pytest.approx(1.30 + 120 * 0.0001)
    assert not day.eu_entry_fallback
    price, exact = h.boundary_price(day.mins, day.open, np.array([480]))
    assert price[0] == pytest.approx(1.30 + 479 * 0.0001) and not exact[0]
    rows = [row for row in rows if row[1] < 475 or row[1] == 825]
    days, stats = h.computation_days(make_bars(rows))
    assert days == []
    assert stats == {"weekday_dates": 1, "computation_days": 0, "excluded_weekdays": 1}


def test_computation_days_skip_weekends_and_preparation_start():
    rows = flat(20240106, 0, 1440) + flat(20240107, 0, 1440)
    rows += flat(20240108, 0, 1440) + flat(20240109, 0, 1440)
    rows += flat(20070101, 0, 1440)
    days, stats = h.computation_days(
        make_bars(sorted(rows, key=lambda row: row[0] * 1440 + row[1]))
    )
    assert [day.date for day in days] == [date(2024, 1, 8), date(2024, 1, 9)]
    assert stats == {"weekday_dates": 2, "computation_days": 2, "excluded_weekdays": 0}
    assert days[0].eu_entry_min == 120 and days[0].us_exit_min == 825
    assert days[0].us_dst is False


def test_summer_day_has_edt_minutes_and_dst_flag():
    rows = flat(20240715, 0, 1440)
    days, _ = h.computation_days(make_bars(rows))
    day = days[0]
    assert day.us_dst is True
    assert (day.eu_entry_min, day.us_exit_min) == (120, 885)
    assert day.eu_entry_price == pytest.approx(1.30)


def walk_arrays(entries):
    mins = np.array([e[0] for e in entries], dtype=np.int64)
    return (
        mins,
        np.array([e[2] for e in entries]),
        np.array([e[3] for e in entries]),
        np.array([e[1] for e in entries]),
    )


def test_walk_long_gap_fills_at_open():
    entry, sigma = 1.30, 0.01
    arrays = walk_arrays([(480, entry, entry, entry), (500, 1.283, 1.284, 1.283)])
    walk = h.walk_window(*arrays, 480, 345, entry, sigma, direction=1)
    assert walk.stopped and walk.exit_min == 500
    assert walk.exit_price == pytest.approx(1.283)
    assert walk.exit_price is not None
    assert walk.exit_price < entry * math.exp(-sigma)


def test_walk_long_stops_at_level_price():
    entry, sigma = 1.30, 0.01
    arrays = walk_arrays(
        [(480, entry, entry, entry), (500, 1.29, 1.295, entry * math.exp(-0.0105))]
    )
    walk = h.walk_window(*arrays, 480, 345, entry, sigma, direction=1)
    assert walk.stopped and walk.exit_min == 500
    assert walk.exit_price == pytest.approx(entry * math.exp(-sigma))


def test_walk_flag_moves_stop_from_the_next_bar():
    entry, sigma = 1.30, 0.01
    arrays = walk_arrays(
        [
            (480, entry, entry, entry),
            (510, entry, entry * math.exp(0.0105), entry * math.exp(-0.005)),
            (
                520,
                entry * math.exp(0.001),
                entry * math.exp(0.001),
                entry * math.exp(-0.004),
            ),
        ]
    )
    walk = h.walk_window(*arrays, 480, 345, entry, sigma, direction=1)
    assert walk.stopped and walk.exit_min == 520 and walk.flag_min == 510
    assert walk.exit_price == pytest.approx(entry)


def test_walk_short_stop_and_flag():
    entry, sigma = 1.30, 0.01
    arrays = walk_arrays(
        [(120, entry, entry, entry), (200, 1.31, entry * math.exp(0.0105), 1.3095)]
    )
    walk = h.walk_window(*arrays, 120, 360, entry, sigma, direction=-1)
    assert walk.stopped and walk.exit_min == 200
    assert walk.exit_price == pytest.approx(entry * math.exp(sigma))
    arrays = walk_arrays(
        [
            (120, entry, entry, entry),
            (210, 1.30, 1.30, entry * math.exp(-0.0105)),
            (
                220,
                entry * math.exp(-0.001),
                entry * math.exp(0.004),
                entry * math.exp(-0.001),
            ),
        ]
    )
    walk = h.walk_window(*arrays, 120, 360, entry, sigma, direction=-1)
    assert walk.stopped and walk.exit_min == 220 and walk.flag_min == 210
    assert walk.exit_price == pytest.approx(entry)


def test_walk_time_exit_excludes_boundary_bar_and_no_stop_mode():
    entry, sigma = 1.30, 0.01
    arrays = walk_arrays(
        [(480, entry, entry, entry), (600, 1.29, 1.29, 1.2905), (825, 1.20, 1.25, 1.20)]
    )
    walk = h.walk_window(*arrays, 480, 345, entry, sigma, direction=1)
    assert not walk.stopped and walk.exit_min == 825 and walk.exit_price is None
    assert walk.flag_min is None and walk.double_touches == 0
    idle = h.walk_window(*arrays, 480, 345, entry, sigma, direction=1, use_stop=False)
    assert not idle.stopped and idle.exit_min == 825


def test_walk_counts_double_touches_and_optimistic_agrees():
    entry, sigma = 1.30, 0.01
    arrays = walk_arrays(
        [
            (480, entry, entry, entry),
            (500, 1.30, entry * math.exp(0.0105), entry * math.exp(-0.0105)),
        ]
    )
    walk = h.walk_window(*arrays, 480, 345, entry, sigma, direction=1)
    assert walk.double_touches == 1 and walk.optimistic_matches
    assert walk.exit_price == pytest.approx(entry * math.exp(-sigma))


def test_walk_does_not_count_straddles_after_the_exit_bar():
    entry, sigma = 1.30, 0.01
    straddle_high, straddle_low = entry * math.exp(0.0105), entry * math.exp(-0.0105)
    arrays = walk_arrays(
        [
            (480, entry, entry, entry),
            (490, entry - 0.014, entry - 0.010, entry - 0.018),
            (600, entry - 0.004, straddle_high, straddle_low),
        ]
    )
    walk = h.walk_window(*arrays, 480, 345, entry, sigma, direction=1)
    assert walk.stopped and walk.exit_min == 490
    assert walk.flag_min is None
    assert walk.double_touches == 0


def weekday_dates(start, count):
    dates, current = [], start
    while len(dates) < count:
        if current.weekday() < 5:
            dates.append(current)
        current += timedelta(days=1)
    return dates


def synth_bars(count, craft=None):
    rows = []
    for index, day in enumerate(weekday_dates(date(2024, 1, 8), count)):
        ymd = day.year * 10000 + day.month * 100 + day.day
        eu_min, exit_min = h.ny_minutes(day)
        p_e, p_m = 1.30, 1.30 + 0.001 * (index % 17)
        p_x = p_m + 0.0005 * (index % 5)
        default = [
            (ymd, eu_min, p_e, p_e, p_e, p_e),
            (ymd, 480, p_m, p_m, p_m, p_m),
            (ymd, exit_min, p_x, p_x, p_x, p_x),
        ]
        crafted = craft(index, ymd, eu_min, exit_min, p_e, p_m, p_x) if craft else None
        rows += crafted if crafted is not None else default
    return make_bars(rows)


def synth_days(count, craft=None):
    return h.computation_days(synth_bars(count, craft))[0]


def stopped_us_day(index, ymd, eu_min, exit_min, p_e, p_m, p_x):
    """Day whose US leg gaps below its stop at minute 500 and fills at open."""
    return [
        (ymd, eu_min, p_e, p_e, p_e, p_e),
        (ymd, 480, p_m, p_m, p_m, p_m),
        (ymd, 500, 0.99 * p_m, 0.991 * p_m, 0.985 * p_m, 0.99 * p_m),
        (ymd, exit_min, p_x, p_x, p_x, p_x),
    ]


def spy_row(day, close, div=0.0, split=1.0):
    return {"date": day, "close": close, "div_cash": div, "split_factor": split}


def frame_of(rows):
    return pd.DataFrame(
        {
            "date": [day for day, _, _ in rows],
            "R": [ret for _, ret, _ in rows],
            "leverage": [lev for _, _, lev in rows],
        }
    )


def flat_spy(*days_, close=100.0):
    return [spy_row(day, close) for day in days_]


def test_tax_ledger_profit_loss_carry_expiry_and_order():
    ledger = h.TaxLedger(0.2)
    assert ledger.settle(2020, 100) == pytest.approx(20.0)
    assert ledger.settle(2021, -50) == 0.0
    assert ledger.settle(2022, 30) == 0.0
    assert ledger.settle(2023, 40) == pytest.approx(4.0)
    assert ledger.settle(2025, 10) == pytest.approx(2.0)
    assert ledger.settle(2026, -10) == 0.0
    assert ledger.settle(2027, -5) == 0.0
    assert ledger.settle(2029, 12) == 0.0
    assert ledger.settle(2030, 4) == pytest.approx(0.2)


def test_accounts_profit_year_and_loss_year():
    frame = frame_of(
        [
            (date(2019, 12, 2), 0.10, 2.0),
            (date(2019, 12, 3), 0.10, 2.0),
            (date(2020, 1, 2), -0.05, 2.0),
            (date(2020, 1, 3), -0.05, 2.0),
        ]
    )
    spy = flat_spy(date(2019, 12, 2), date(2020, 1, 3))
    accounts, stats = h.account_segment(frame, spy, "recent")
    assert list(accounts.columns) == list(h.ACCOUNT_COLUMNS)
    assert accounts["equity_start"].tolist() == pytest.approx(
        [1.0, 1.1, 1.1673385, 1.108971575]
    )
    assert accounts["equity_pretax"].tolist() == pytest.approx(
        [1.1, 1.21, 1.1495, 1.092025]
    )
    tax_2019 = h.TAX_RATE * (0.10 + 1.1 * 0.10)
    assert accounts["tax_paid"].tolist() == pytest.approx([0.0, tax_2019, 0.0, 0.0])
    assert accounts["equity_end"].tolist() == pytest.approx(
        [1.1, 1.21 - tax_2019, 1.1673385 * 0.95, 1.1673385 * 0.95 * 0.95]
    )
    assert stats["w"] == pytest.approx(float(accounts.iloc[-1]["equity_end"]))
    assert stats["segment"] == "recent"
    assert stats["max_leverage"] == 2.0
    assert stats["max_leg_notional_to_equity"] == pytest.approx(2.0 * h.W_EU)
    assert stats["psr_threshold"] == h.PSR_THRESHOLD
    assert 0.0 < stats["psr"] < 1.0
    elapsed = (date(2020, 1, 3) - date(2019, 12, 2)).days
    assert stats["cagr_pretax"] == pytest.approx(1.092025 ** (365.0 / elapsed) - 1.0)
    assert stats["cagr_aftertax"] == pytest.approx(
        (1.1673385 * 0.95 * 0.95) ** (365.0 / elapsed) - 1.0
    )
    assert stats["drawdown_pretax"] == pytest.approx(1.0 - 1.092025 / 1.21)
    assert stats["drawdown_aftertax"] == pytest.approx(1.0 - 1.05352299625 / 1.1673385)
    expected_sr = (
        math.sqrt(252.0)
        * np.mean([0.1, 0.1, -0.05, -0.05])
        / np.std([0.1, 0.1, -0.05, -0.05], ddof=1)
    )
    assert stats["sr"] == pytest.approx(float(expected_sr))


def test_tax_carryforward_expiry_and_final_partial_year():
    frame = frame_of(
        [
            (date(2010, 1, 4), -0.10, 1.0),
            (date(2011, 1, 3), 0.05, 1.0),
            (date(2015, 1, 5), 0.05, 1.0),
        ]
    )
    spy = flat_spy(date(2007, 7, 2), date(2010, 1, 4), date(2015, 1, 5))
    accounts, _ = h.account_segment(frame, spy, "dev")
    assert accounts["tax_paid"].tolist() == pytest.approx(
        [0.0, 0.0, h.TAX_RATE * (0.945 * 0.05)]
    )
    assert accounts["equity_end"].tolist() == pytest.approx(
        [0.9, 0.945, 0.945 * 1.05 - h.TAX_RATE * (0.945 * 0.05)]
    )


def test_segment_restart_ignores_earlier_losses():
    frame = frame_of(
        [
            (date(2007, 7, 2), 0.0, 1.0),
            (date(2019, 3, 4), -0.10, 1.0),
            (date(2020, 6, 1), 0.10, 1.0),
            (date(2020, 6, 2), 0.0, 1.0),
        ]
    )
    spy = [
        spy_row(date(2007, 7, 2), 100.0),
        spy_row(date(2010, 1, 4), 110.0, div=2.0),
        spy_row(date(2015, 6, 1), 55.0, split=2.0),
        spy_row(date(2020, 6, 1), 60.0),
        spy_row(date(2024, 10, 31), 60.0),
    ]
    dev_accounts, dev_stats = h.account_segment(frame, spy, "dev")
    recent_accounts, recent_stats = h.account_segment(frame, spy, "recent")
    assert dev_accounts["tax_paid"].tolist() == pytest.approx([0.0, 0.0, 0.0, 0.0])
    assert recent_accounts["tax_paid"].tolist() == pytest.approx(
        [0.0, h.TAX_RATE * 0.10]
    )
    assert dev_accounts.iloc[0]["equity_start"] == 1.0
    assert recent_accounts.iloc[0]["equity_start"] == 1.0
    assert dev_accounts.iloc[-1]["equity_end"] == pytest.approx(0.99)
    assert recent_accounts.iloc[-1]["equity_end"] == pytest.approx(
        1.1 - 0.1 * h.TAX_RATE
    )
    shares = 1.0 / (1.0002 * 100.0)
    shares = shares + shares * 2.0 / 110.0
    shares = shares * 2.0
    assert dev_stats["b"] == pytest.approx(shares * 60.0 * 0.9998)
    assert recent_stats["b"] == pytest.approx(0.9998 / 1.0002)


def test_segment_errors_psr_and_ruin():
    spy = flat_spy(date(2019, 12, 2), date(2020, 1, 3))
    one_day = frame_of([(date(2019, 12, 2), 0.05, 1.0)])
    with pytest.raises(ValueError, match="PSR"):
        h.account_segment(one_day, spy, "recent")
    constant = frame_of(
        [(date(2019, 12, 2), 0.05, 1.0), (date(2019, 12, 3), 0.05, 1.0)]
    )
    with pytest.raises(ValueError, match="PSR"):
        h.account_segment(constant, spy, "recent")
    ruined = frame_of([(date(2019, 12, 2), -1.0, 1.0), (date(2019, 12, 3), 0.05, 1.0)])
    with pytest.raises(ValueError, match="equity ruined"):
        h.account_segment(ruined, spy, "recent")
    no_leverage = frame_of(
        [(date(2019, 11, 1), None, None), (date(2019, 11, 4), 0.01, 1.0)]
    )
    with pytest.raises(ValueError, match="leverage"):
        h.account_segment(no_leverage, spy, "recent")
    empty = frame_of([(date(2007, 1, 3), 0.01, 1.0)])
    with pytest.raises(ValueError, match="no calculation days"):
        h.account_segment(empty, spy, "recent")
    with pytest.raises(ValueError, match="no SPY rows"):
        h.account_segment(constant, flat_spy(date(2026, 10, 30)), "recent")


def alternating(start, count, up, down):
    dates = weekday_dates(start, count)
    return frame_of(
        [(day, up if index % 2 else down, 1.0) for index, day in enumerate(dates)]
    )


def test_run_segments_and_gates():
    frame = pd.concat(
        [
            alternating(date(2007, 7, 2), 100, 0.01, -0.004),
            alternating(date(2019, 11, 1), 40, 0.008, -0.003),
            alternating(date(2024, 11, 3), 40, 0.008, -0.003),
        ],
        ignore_index=True,
    )
    spy = flat_spy(
        date(2007, 7, 2),
        date(2019, 11, 1),
        date(2024, 10, 31),
        date(2024, 11, 4),
        date(2026, 10, 30),
    )
    accounts, stats, gates = h.run_segments(frame, spy, "holdout")
    assert list(accounts.columns) == list(h.ACCOUNT_COLUMNS)
    assert accounts["segment"].tolist().count("dev") == 140
    assert len(accounts) == 220
    assert set(stats) == {"dev", "recent", "holdout"}
    assert gates == {"A": True, "B": True, "C": True, "H": True}
    assert stats["dev"]["w"] == pytest.approx(
        float(accounts[accounts["segment"] == "dev"].iloc[-1]["equity_end"])
    )

    dev_only, dev_stats, dev_gates = h.run_segments(frame, spy, "dev")
    assert set(dev_stats) == {"dev", "recent"}
    assert len(dev_only) == 180 and dev_gates["H"] is None

    booming = [
        spy_row(date(2007, 7, 2), 100.0),
        spy_row(date(2019, 11, 1), 1000.0),
        spy_row(date(2024, 10, 31), 1000.0),
        spy_row(date(2024, 11, 4), 1000.0),
        spy_row(date(2026, 10, 30), 1000.0),
    ]
    _, weak_stats, weak_gates = h.run_segments(frame, booming, "holdout")
    assert weak_gates["A"] is False and weak_gates["C"] is True
    assert weak_stats["dev"]["b"] == pytest.approx(10.0 * 0.9998 / 1.0002)

    losing = pd.concat(
        [
            alternating(date(2007, 7, 2), 100, -0.004, -0.010),
            alternating(date(2019, 11, 1), 40, 0.008, -0.003),
            alternating(date(2024, 11, 3), 40, -0.008, -0.012),
        ],
        ignore_index=True,
    )
    _, _, losing_gates = h.run_segments(losing, spy, "holdout")
    assert losing_gates["B"] is False and losing_gates["H"] is False


def test_read_spy_cutoff_and_validation(tmp_path):
    rows = [
        {"date": "2024-10-31", "close": 100.0, "divCash": 0.0, "splitFactor": 1.0},
        {"date": "2024-11-01", "close": 101.0, "divCash": 0.0, "splitFactor": 1.0},
        {"date": "2026-10-30", "close": 102.0, "divCash": 0.0, "splitFactor": 1.0},
        {"date": "2026-10-31", "close": 103.0, "divCash": 0.0, "splitFactor": 1.0},
    ]
    path = tmp_path / "SPY.json"
    path.write_text(json.dumps(rows))
    dev_rows = h.read_spy(path, "dev")
    holdout_rows = h.read_spy(path, "holdout")
    assert [row["date"] for row in dev_rows] == [date(2024, 10, 31)]
    assert [row["date"] for row in holdout_rows] == [
        date(2024, 10, 31),
        date(2024, 11, 1),
        date(2026, 10, 30),
    ]
    assert dev_rows[0]["close"] == 100.0 and dev_rows[0]["div_cash"] == 0.0

    def write(entry):
        path.write_text(json.dumps([entry]))
        return path

    base = {"date": "2024-10-31", "close": 100.0, "divCash": 0.0, "splitFactor": 1.0}
    with pytest.raises(ValueError, match="non-finite SPY"):
        h.read_spy(write({**base, "close": float("nan")}), "dev")
    with pytest.raises(ValueError, match="non-positive SPY"):
        h.read_spy(write({**base, "close": 0.0}), "dev")
    with pytest.raises(ValueError, match="strictly increasing"):
        path.write_text(json.dumps([rows[0], rows[0]]))
        h.read_spy(path, "dev")
    with pytest.raises(ValueError, match="non-empty JSON array"):
        path.write_text("[]")
        h.read_spy(path, "dev")
    with pytest.raises(ValueError, match="no SPY rows"):
        h.read_spy(
            write(
                {"date": "2026-12-31", "close": 1.0, "divCash": 0.0, "splitFactor": 1.0}
            ),
            "dev",
        )
    with pytest.raises(ValueError, match="invalid stage"):
        h.read_spy(path, "run")


def test_daily_legs_layout_and_preparation_period():
    frame = h.daily_legs(
        synth_days(
            65, lambda i, *unused: stopped_us_day(i, *unused) if i == 60 else None
        )
    )
    assert list(frame.columns) == list(h.DAY_COLUMNS)
    first = frame.iloc[0]
    assert first["us_entry_min"] == 480 and first["eu_entry_min"] == 120
    assert not first["eu_stopped"] and not first["us_stopped"]
    assert (
        first["eu_exit_min"] == 480
        and first["us_exit_min"] == first["us_exit_min_boundary"]
    )
    assert first["eu_exit_price"] == pytest.approx(first["eu_boundary_exit_price"])
    assert first["eu_r"] == pytest.approx(first["eu_r_nostop"])
    for column in (
        "eu_sigma",
        "eu_stop_initial",
        "eu_flag_min",
        "sigma_hat",
        "leverage",
        "R",
    ):
        assert pd.isna(frame.iloc[0][column]) and pd.isna(frame.iloc[59][column])
    walked = frame.iloc[60]
    assert walked["eu_sigma"] > 0 and walked["us_sigma"] > 0
    assert walked["eu_stop_initial"] == pytest.approx(
        walked["eu_entry_price"] * math.exp(walked["eu_sigma"])
    )
    assert walked["us_stop_initial"] == pytest.approx(
        walked["us_entry_price"] * math.exp(-walked["us_sigma"])
    )
    assert bool(walked["us_stopped"]) and walked["us_exit_min"] == 500
    assert walked["us_exit_price"] == pytest.approx(0.99 * walked["us_entry_price"])
    assert walked["us_r_nostop"] == pytest.approx(
        math.log(walked["us_boundary_exit_price"] / walked["us_entry_price"])
    )


def test_sigma_windows_exclude_today_and_need_sixty_days():
    frame = h.daily_legs(synth_days(61))
    expected = float(np.std(frame["eu_r_nostop"][:60], ddof=1))
    assert frame.iloc[60]["eu_sigma"] == pytest.approx(expected)
    assert frame.iloc[60]["sigma_hat"] == pytest.approx(
        math.sqrt(252.0) * float(np.std(frame["u"][:60], ddof=1))
    )
    assert pd.isna(frame.iloc[59]["eu_sigma"])

    def bump(index, ymd, eu_min, exit_min, p_e, p_m, p_x):
        if index != 60:
            return None
        p_m = 1.35
        return [
            (ymd, eu_min, p_e, p_e, p_e, p_e),
            (ymd, 480, p_m, p_m, p_m, p_m),
            (ymd, exit_min, p_m + 0.001, p_m + 0.001, p_m + 0.001, p_m + 0.001),
        ]

    moved = h.daily_legs(synth_days(61, bump))
    assert moved.iloc[60]["eu_sigma"] == pytest.approx(expected)
    assert moved.iloc[60]["eu_r_nostop"] != pytest.approx(frame.iloc[60]["eu_r_nostop"])


def test_u_leverage_and_R_formulas():
    frame = h.daily_legs(
        synth_days(
            65, lambda i, *unused: stopped_us_day(i, *unused) if i == 62 else None
        )
    )
    expected_u = h.W_EU * frame["eu_r"] + h.W_US * frame["us_r"]
    assert np.allclose(frame["u"], expected_u, rtol=0, atol=1e-15)
    for position in (60, 62, 64):
        sigma_hat = math.sqrt(252.0) * float(
            np.std(frame["u"][position - 60 : position], ddof=1)
        )
        assert frame.iloc[position]["sigma_hat"] == pytest.approx(sigma_hat)
        leverage = min(0.30 / sigma_hat, h.LEVERAGE_CAP)
        assert frame.iloc[position]["leverage"] == pytest.approx(leverage)
        expected_r = leverage * (
            h.W_EU * (frame.iloc[position]["eu_a"] - frame.iloc[position]["eu_c"])
            + h.W_US * (frame.iloc[position]["us_a"] - frame.iloc[position]["us_c"])
        )
        assert frame.iloc[position]["R"] == pytest.approx(expected_r)


def test_future_bars_leave_earlier_days_unchanged():
    baseline = h.daily_legs(synth_days(70))

    def scramble(index, ymd, eu_min, exit_min, p_e, p_m, p_x):
        if index < 66:
            return None
        return [
            (ymd, eu_min, p_e * 1.05, p_e * 1.05, p_e * 1.05, p_e * 1.05),
            (ymd, 480, p_m * 1.05, p_m * 1.05, p_m * 1.05, p_m * 1.05),
            (ymd, exit_min, p_x * 1.05, p_x * 1.05, p_x * 1.05, p_x * 1.05),
        ]

    changed = h.daily_legs(synth_days(70, scramble))
    pd.testing.assert_frame_equal(
        baseline.iloc[:66].reset_index(drop=True),
        changed.iloc[:66].reset_index(drop=True),
    )
    assert not changed.iloc[66:].equals(baseline.iloc[66:])


def test_missing_entry_bar_uses_fallback_price_but_walks_from_boundary():
    def craft(index, ymd, eu_min, exit_min, p_e, p_m, p_x):
        if index != 60:
            return None
        q = 1.28
        return [
            (ymd, eu_min, p_e, p_e, p_e, p_e),
            (ymd, 479, q, q, q, q),
            (ymd, 500, 0.99 * q, 0.991 * q, 0.985 * q, 0.99 * q),
            (ymd, exit_min, p_x, p_x, p_x, p_x),
        ]

    frame = h.daily_legs(synth_days(61, craft))
    row = frame.iloc[60]
    assert bool(row["us_entry_fallback"])
    assert row["us_entry_price"] == pytest.approx(1.28)
    assert row["eu_boundary_exit_price"] == pytest.approx(1.28)
    assert bool(row["us_stopped"]) and row["us_exit_min"] == 500
    assert row["us_exit_price"] == pytest.approx(0.99 * 1.28)


def test_cost_uses_the_fill_price():
    frame = h.daily_legs(
        synth_days(
            61, lambda i, *unused: stopped_us_day(i, *unused) if i == 60 else None
        )
    )
    row = frame.iloc[60]
    half = 0.4 / 2 * 1e-4
    assert row["us_c"] == pytest.approx(
        half / row["us_entry_price"] + half / row["us_exit_price"]
    )
    doubled = h.daily_legs(
        synth_days(
            61, lambda i, *unused: stopped_us_day(i, *unused) if i == 60 else None
        ),
        pip=0.8,
    )
    assert doubled.iloc[60]["us_c"] == pytest.approx(2 * row["us_c"])
    assert doubled.iloc[60]["us_a"] == pytest.approx(row["us_a"])
    assert doubled.iloc[60]["us_r"] == pytest.approx(row["us_r"])


def test_flat_prices_raise_sigma_leg_error():
    rows = []
    for day in weekday_dates(date(2024, 1, 8), 61):
        ymd = day.year * 10000 + day.month * 100 + day.day
        eu_min, exit_min = h.ny_minutes(day)
        for minute in (eu_min, 480, exit_min):
            rows.append((ymd, minute, 1.30, 1.30, 1.30, 1.30))
    with pytest.raises(ValueError, match="sigma_leg"):
        h.daily_legs(h.computation_days(make_bars(rows))[0])


def test_nonfinite_leg_return_raises():
    days = synth_days(61)
    days[60] = replace(days[60], eu_entry_price=float("nan"))
    with pytest.raises(ValueError, match="non-finite leg return"):
        h.daily_legs(days)


def dense_day(target_date, spike_minutes=(), drop_minutes=()):
    """A day with a bar at (almost) every minute: gentle waves plus spikes."""
    eu_min, exit_min = h.ny_minutes(target_date)
    kept = [m for m in range(1440) if m not in set(drop_minutes)]
    minutes = np.array(kept)
    opens = 1.30 + 0.0008 * np.sin(0.37 * minutes) + 0.0004 * np.cos(0.11 * minutes)
    highs = opens + 0.0006
    lows = opens - 0.0006
    for minute in spike_minutes:
        position = minutes == minute
        highs[position] = opens[position] * 1.01
        lows[position] = opens[position] * 0.99
    return (
        h.Day(
            date=target_date,
            eu_entry_min=eu_min,
            us_entry_min=480,
            us_exit_min=exit_min,
            us_dst=h.us_dst(target_date),
            mins=minutes,
            open=opens,
            high=highs,
            low=lows,
            close=opens,
            eu_entry_price=float(opens[minutes == eu_min][0]),
            eu_entry_fallback=False,
            us_entry_price=float(opens[minutes == 480][0]),
            us_entry_fallback=False,
            us_exit_price=float(opens[minutes == exit_min][0]),
            us_exit_fallback=False,
        ),
        opens,
        highs,
        lows,
    )


def empty_day(target_date):
    eu_min, exit_min = h.ny_minutes(target_date)
    return h.Day(
        date=target_date,
        eu_entry_min=eu_min,
        us_entry_min=480,
        us_exit_min=exit_min,
        us_dst=h.us_dst(target_date),
        mins=np.array([], dtype=np.int64),
        open=np.array([], dtype=np.float64),
        high=np.array([], dtype=np.float64),
        low=np.array([], dtype=np.float64),
        close=np.array([], dtype=np.float64),
        eu_entry_price=1.30,
        eu_entry_fallback=False,
        us_entry_price=1.30,
        us_entry_fallback=False,
        us_exit_price=1.30,
        us_exit_fallback=False,
    )


def control_frame(days_list, rets, leverage=8.0, sigma=0.004, us_exit=None):
    return pd.DataFrame(
        {
            "date": [day.date for day in days_list],
            "eu_exit_min": [480 for _ in days_list],
            "us_exit_min": [us_exit or day.us_exit_min for day in days_list],
            "eu_sigma": [sigma for _ in days_list],
            "us_sigma": [sigma for _ in days_list],
            "leverage": [leverage for _ in days_list],
            "R": rets,
        }
    )


def test_fallback_prices_same_date_only():
    values = np.full(10, np.nan)
    values[2] = 1.20
    values[7] = 1.30
    price = h._fallback_prices(values, np.arange(10))
    assert np.isnan(price[0]) and np.isnan(price[1])
    assert price[2] == 1.20 and price[7] == 1.30
    assert price[3] == 1.20 and price[6] == 1.20
    assert price[8] == 1.30 and price[9] == 1.30


def test_window_walk_k_covers_len_zero_and_friday():
    open_d = np.full(1440, 1.30)
    high_d = np.full(1440, 1.31)
    low_d = np.full(1440, 1.29)
    K, entry, stopped, exit_min, exit_price = h._window_walk(
        low_d, high_d, open_d, 0, 0.004, direction=1, end_max=1438
    )
    assert K == 1439
    assert not stopped.any()
    assert (exit_min == np.arange(1439)).all()
    assert (exit_price == entry).all()
    assert (
        h._window_walk(low_d, high_d, open_d, 0, 0.004, direction=1, end_max=1018)[0]
        == 1019
    )
    assert (
        h._window_walk(low_d, high_d, open_d, 1439, 0.004, direction=1, end_max=1438)[0]
        == 0
    )


def test_window_walk_matches_scalar_reference():
    rng = np.random.default_rng(11)
    minutes, opens, highs, lows = [], [], [], []
    for minute in range(1440):
        if minute % 9 == 4:
            continue
        price = (
            1.30 + 0.0012 * math.sin(minute / 11.0) + float(rng.uniform(-3e-4, 3e-4))
        )
        high, low = price + 5e-4, price - 5e-4
        if minute in (260, 261):
            high = price * 1.012
        if minute in (620, 621):
            low = price * 0.988
        if minute == 900:
            price, high, low = 1.24, 1.2405, 1.2395
        if minute == 1100:
            price, high, low = 1.37, 1.3705, 1.3695
        minutes.append(minute)
        opens.append(price)
        highs.append(high)
        lows.append(low)
    mins = np.array(minutes)
    opens_a, highs_a, lows_a = np.array(opens), np.array(highs), np.array(lows)
    open_d = np.full(1440, np.nan)
    high_d = np.full(1440, np.nan)
    low_d = np.full(1440, np.nan)
    open_d[mins], high_d[mins], low_d[mins] = opens_a, highs_a, lows_a
    total_stopped = total_time = 0
    for direction in (-1, 1):
        for length in (0, 1, 25, 140):
            K, entry, stopped, exit_min, exit_price = h._window_walk(
                low_d, high_d, open_d, length, 0.004, direction=direction, end_max=1438
            )
            checks = list(range(0, K, 7)) + [K - 1]
            end_prices = h._fallback_prices(open_d, np.array(checks) + length)
            stopped_seen = time_seen = 0
            for position, m in enumerate(checks):
                if not np.isfinite(entry[m]):
                    assert not stopped[m]
                    continue
                walk = h.walk_window(
                    mins,
                    highs_a,
                    lows_a,
                    opens_a,
                    m,
                    length,
                    float(entry[m]),
                    0.004,
                    direction=direction,
                )
                assert bool(stopped[m]) == walk.stopped
                assert int(exit_min[m]) == walk.exit_min
                if walk.stopped:
                    stopped_seen += 1
                    assert exit_price[m] == pytest.approx(walk.exit_price)
                else:
                    time_seen += 1
                    assert exit_price[m] == pytest.approx(end_prices[position])
            total_stopped += stopped_seen
            total_time += time_seen
    assert total_stopped > 0 and total_time > 0


def test_random_control_start_minutes_layout_and_k():
    dates = weekday_dates(date(2024, 1, 8), 8)
    days = [dense_day(value)[0] for value in dates]
    rets = [0.001 * ((i % 5) - 2) for i in range(8)]
    frame = control_frame(days, rets)
    table, control = h.random_control(frame, days, rounds=5)
    assert table is not None
    assert list(table.columns) == list(h.RANDOM_COLUMNS)
    assert len(table) == 5 * 8 * 2
    assert table.iloc[0]["round"] == 0 and table.iloc[0]["leg"] == "eu"
    assert table.iloc[1]["leg"] == "us" and table.iloc[2]["day_index"] == 1
    assert table.iloc[-1]["round"] == 4
    assert table.iloc[0]["valid"] == table.iloc[1]["valid"]
    for row in (table.iloc[0], table.iloc[3], table.iloc[2 * 16 + 5], table.iloc[-1]):
        leg_id = 0 if row["leg"] == "eu" else 1
        n = np.uint64(int(row["round"])) * np.uint64(2**32) + np.uint64(
            2 * int(row["day_index"]) + leg_id
        )
        expected = int(h.splitmix64(np.array([n]))[0]) % int(row["K"])
        assert int(row["start_min"]) == expected
    friday = table[table["day_index"] == 4]
    assert set(friday[friday["leg"] == "eu"]["K"]) == {659}
    assert set(friday[friday["leg"] == "us"]["K"]) == {674}
    monday = table[(table["day_index"] == 0) & (table["leg"] == "eu")]
    assert set(monday["K"]) == {1079}
    assert len(control["round_sr"]) == 5 and len(control["excluded_days"]) == 5
    assert control["percentile"] is not None
    hits = sum(
        1
        for value in control["round_sr"]
        if value is not None and value <= control["actual_sr"]
    )
    assert control["percentile"] == pytest.approx(hits / 5)


def test_stopped_leg_shortens_window():
    days = [dense_day(date(2024, 1, 8))[0], dense_day(date(2024, 1, 9))[0]]
    frame = control_frame(days, [0.01, -0.004], us_exit=500)
    table, _ = h.random_control(frame, days, rounds=3)
    assert table is not None
    us = table[table["leg"] == "us"]
    eu = table[table["leg"] == "eu"]
    assert set(us["K"]) == {1419}
    assert set(eu["K"]) == {1079}
    assert (us["exit_min"] <= us["start_min"] + 20).all()


def test_overlap_and_missing_price_exclude_days():
    dates = weekday_dates(date(2024, 1, 8), 6)
    days = [dense_day(value)[0] for value in dates]
    frame = control_frame(days, [0.002, -0.001, 0.0015, -0.0005, 0.001, -0.002])
    table, _ = h.random_control(frame, days, rounds=40)
    assert table is not None
    eu = table[table["leg"] == "eu"][["round", "day_index", "start_min", "valid"]]
    us = table[table["leg"] == "us"][["round", "day_index", "start_min"]]
    merged = eu.merge(us, on=["round", "day_index"], suffixes=("_eu", "_us"))
    assert len(merged) == 240
    assert not merged["valid"].all()
    for row in merged.itertuples():
        index = int(row.day_index)
        eu_len = 480 - days[index].eu_entry_min
        us_len = days[index].us_exit_min - 480
        overlap = max(row.start_min_eu, row.start_min_us) < min(
            row.start_min_eu + eu_len, row.start_min_us + us_len
        )
        assert bool(row.valid) == (not overlap)

    empties = [empty_day(date(2024, 1, 8)), dense_day(date(2024, 1, 9))[0]]
    frame2 = control_frame(empties, [0.01, -0.004])
    table2, _ = h.random_control(frame2, empties, rounds=3)
    assert table2 is not None
    missing = table2[table2["day_index"] == 0]
    assert not bool(missing["valid"].any())
    assert bool(np.isnan(missing["a"].to_numpy()).all())
    assert bool(np.isnan(missing["c"].to_numpy()).all())
    assert bool(missing["start_min"].notna().to_numpy().all())


def test_gate_d_percentile_and_not_computable():
    dates = weekday_dates(date(2024, 1, 8), 12)
    days = [dense_day(value)[0] for value in dates]
    rets = [0.02 if i % 2 else -0.004 for i in range(12)]
    frame = control_frame(days, rets)
    _, control = h.random_control(frame, days, rounds=8, table=False)
    assert control["actual_sr"] > 3
    assert all(value <= control["actual_sr"] for value in control["round_sr"])
    assert control["percentile"] == 1.0 and control["gate_d"] is True
    flipped = control_frame(days, [-value for value in rets])
    _, weak = h.random_control(flipped, days, rounds=8, table=False)
    assert weak["percentile"] < 0.5 and weak["gate_d"] is False
    blanks = [empty_day(value) for value in weekday_dates(date(2024, 1, 8), 4)]
    dead_frame = control_frame(blanks, [0.01, -0.02, 0.005, -0.001])
    _, dead = h.random_control(dead_frame, blanks, rounds=3, table=False)
    assert dead["percentile"] is None and dead["gate_d"] is False
    assert dead["excluded_days"] == [4, 4, 4]
    assert all(value is None for value in dead["round_sr"])


def test_tail_quantiles_use_nearest_rank():
    tails = h._tail_values([float(value) for value in range(1, 101)])
    assert tails["p01"] == 1.0
    assert tails["p05"] == 5.0
    assert tails["worst"] == 1.0
    assert tails["win_loss_ratio"] is None
    assert tails["win_rate"] == 1.0
    mixed = h._tail_values([-2.0, -1.0, 0.0, 3.0, 4.0, 5.0, 6.0])
    assert mixed["p01"] == -2.0
    assert mixed["p05"] == -2.0
    assert mixed["worst"] == -2.0
    assert mixed["win_loss_ratio"] == pytest.approx(4.5 / 1.5)
    assert mixed["win_rate"] == pytest.approx(4 / 7)
    twenty_one = h._tail_values([float(value) for value in range(-21, 0)])
    assert twenty_one["p05"] == -20.0


def ref_splitmix64(n):
    mask = (1 << 64) - 1
    v = (n ^ 20261115) & mask
    v = (v + 0x9E3779B97F4A7C15) & mask
    v = ((v ^ (v >> 30)) * 0xBF58476D1CE4E5B9) & mask
    v = ((v ^ (v >> 27)) * 0x94D049BB133111EB) & mask
    return v ^ (v >> 31)


def ref_bootstrap(rets, rounds=1000):
    count = len(rets)
    draws = []
    for d in range(rounds):
        equity = 1.0
        peak = 1.0
        worst = 0.0
        ruined = False
        for i in range(count):
            index = ref_splitmix64(d * 2**32 + i) % count
            equity *= 1.0 + rets[index]
            if equity <= 0 or not math.isfinite(equity):
                ruined = True
                break
            peak = max(peak, equity)
            worst = max(worst, 1.0 - equity / peak)
        draws.append(1.0 if ruined else worst)
    return sorted(draws)


def test_bootstrap_drawdowns_match_splitmix_reference():
    assert ref_splitmix64(0) == 13866863570256181660
    rets = [0.01, -0.02, 0.005, -1.2, 0.03, -0.01]
    draws = h._bootstrap_drawdowns(rets)
    expected = ref_bootstrap(rets)
    ruined = sum(1 for value in expected if value == 1.0)
    assert 0 < ruined < 1000
    assert draws.tolist() == pytest.approx(expected)
    values = h._bootstrap_values(rets)
    assert values["p05"] == pytest.approx(expected[49])
    assert values["p50"] == pytest.approx(expected[499])
    assert values["p95"] == pytest.approx(expected[949])


def test_breakeven_multiple_linearity():
    rows = [
        (date(2019, 12, 2), 0.02, 1.0, 0.001, 0.0002, -0.001, 0.0001),
        (date(2019, 12, 3), 0.01, 2.0, 0.002, 0.0003, 0.001, 0.0002),
        (date(2019, 12, 4), -0.005, 1.5, -0.001, 0.0001, 0.0005, 0.0003),
    ]
    columns = ("date", "R", "leverage", "eu_a", "eu_c", "us_a", "us_c")
    frame = pd.DataFrame(
        {column: [row[index] for row in rows] for index, column in enumerate(columns)}
    )
    gains = [row[2] * (h.W_EU * row[3] + h.W_US * row[5]) for row in rows]
    costs = [row[2] * (h.W_EU * row[4] + h.W_US * row[6]) for row in rows]
    expected = statistics.fmean(gains) / statistics.fmean(costs)
    breakeven = h._breakeven(frame, "recent")
    assert breakeven["multiple"] == pytest.approx(expected)
    assert breakeven["pip"] == pytest.approx(0.4 * expected)
    residual = statistics.fmean(gains) - expected * statistics.fmean(costs)
    assert residual == pytest.approx(0.0, abs=1e-15)
    zero = [(row[0], row[1], row[2], row[3], 0.0, row[5], 0.0) for row in rows]
    free = pd.DataFrame(
        {column: [row[index] for row in zero] for index, column in enumerate(columns)}
    )
    assert h._breakeven(free, "recent") == {"multiple": None, "pip": None}


def test_ablation_legs_sum_to_main_returns():
    leverage = [5.0, 6.0]
    eu_a = [0.001, -0.002]
    us_a = [0.002, 0.001]
    rets = [
        lev * (h.W_EU * a_eu + h.W_US * a_us)
        for lev, a_eu, a_us in zip(leverage, eu_a, us_a, strict=True)
    ]
    frame = pd.DataFrame(
        {
            "date": [date(2019, 12, 2), date(2019, 12, 3)],
            "R": rets,
            "leverage": leverage,
            "eu_a": eu_a,
            "eu_c": [0.0, 0.0],
            "us_a": us_a,
            "us_c": [0.0, 0.0],
        }
    )
    eu_only = h.ablation_frame(frame, "eu")
    us_only = h.ablation_frame(frame, "us")
    assert eu_only["R"].tolist() == pytest.approx(
        [lev * h.W_EU * a_eu for lev, a_eu in zip(leverage, eu_a, strict=True)]
    )
    assert us_only["leverage"].tolist() == leverage
    total = eu_only["R"] + us_only["R"]
    assert total.tolist() == pytest.approx(rets)
    with pytest.raises(ValueError, match="invalid leg"):
        h.ablation_frame(frame, "both")


def test_flip_warning_logic():
    assert h.flip_warning(0.8, 0.3) is True
    assert h.flip_warning(0.8, -0.3) is False
    assert h.flip_warning(-0.8, 0.3) is False
    assert h.flip_warning(0.8, None) is False
    assert h.flip_warning(None, 0.3) is False


def test_yearly_median_even_count_and_seasons():
    def sr(values):
        return math.sqrt(252.0) * statistics.fmean(values) / statistics.stdev(values)

    rets = {
        2020: [0.02, 0.04],
        2021: [0.01, 0.03],
        2022: [-0.03, 0.01],
        2023: [-0.01, 0.01],
        2024: [0.005, 0.005],
    }
    frame = pd.DataFrame(
        [
            {
                "date": date(year, 2, 3 + index),
                "R": ret,
                "us_dst": index == 0,
            }
            for year, values in rets.items()
            for index, ret in enumerate(values)
        ]
    )
    report = h._yearly(frame, "recent")
    assert [entry["year"] for entry in report["years"]] == [
        2020,
        2021,
        2022,
        2023,
        2024,
    ]
    assert [entry["days"] for entry in report["years"]] == [2, 2, 2, 2, 2]
    assert report["years"][4]["sr"] is None
    non_null = [sr(rets[year]) for year in (2020, 2021, 2022, 2023)]
    assert report["median_sr"] == pytest.approx(statistics.median(non_null))
    assert report["winter"]["days"] == 5 and report["summer"]["days"] == 5
    assert report["summer"]["sr"] == pytest.approx(
        sr([0.02, 0.01, -0.03, -0.01, 0.005])
    )
    assert report["winter"]["sr"] == pytest.approx(sr([0.04, 0.03, 0.01, 0.01, 0.005]))


def test_report_common_marks_ruin_and_main_path_errors():
    frame = frame_of(
        [
            (date(2019, 12, 2), 0.10, 1.0),
            (date(2019, 12, 3), -1.0, 1.0),
        ]
    )
    spy = flat_spy(date(2019, 12, 2), date(2019, 12, 3))
    with pytest.raises(ValueError, match="equity ruined"):
        h.account_segment(frame, spy, "recent")
    common = h._segment_common(frame, spy, "recent")
    assert common["ruined"] is True
    assert common["ruin_date"] == "2019-12-03"
    assert common["w"] is None
    assert common["cagr_pretax"] is None and common["cagr_aftertax"] is None
    assert common["drawdown_pretax"] is None and common["drawdown_aftertax"] is None
    assert common["b"] == pytest.approx(0.9998 / 1.0002)
    assert common["max_leverage"] == 1.0
    assert common["max_leg_notional_to_equity"] == pytest.approx(h.W_EU)


def test_segment_report_shape_and_self_checks():
    days = synth_days(70)
    frame = h.daily_legs(days)
    spy = flat_spy(date(2024, 1, 8), date(2024, 4, 30), date(2024, 10, 31))
    report = h.segment_report(frame, days, spy, "dev")
    assert set(report) == {
        "main",
        "tax_rates",
        "cost_target",
        "breakeven",
        "tails",
        "bootstrap",
        "ablations",
        "same_bar",
        "flip",
        "yearly",
    }
    assert list(report["main"]) == list(h.COMMON_FIELDS)
    assert [entry["rate"] for entry in report["tax_rates"]] == [0.0, 0.05]
    cells = {(entry["pip"], entry["target"]) for entry in report["cost_target"]}
    assert cells == {
        (pip, target) for pip in (0.0, 0.4, 0.8, 1.5) for target in (0.30, 0.40)
    }
    main_cell = next(
        entry
        for entry in report["cost_target"]
        if (entry["pip"], entry["target"]) == (0.4, 0.30)
    )
    assert main_cell["common"] == report["main"]
    assert set(report["breakeven"]) == {"common", "multiple", "pip"}
    assert set(report["tails"]["values"]) == {
        "p01",
        "p05",
        "worst",
        "win_loss_ratio",
        "win_rate",
    }
    assert set(report["bootstrap"]["values"]) == {"p05", "p50", "p95"}
    assert [entry["name"] for entry in report["ablations"]] == [
        "eu_only",
        "us_only",
        "s0",
    ]
    assert report["same_bar"]["optimistic_matches"] is True
    assert report["same_bar"]["optimistic_common"] == report["main"]
    assert report["flip"]["warning"] is False
    assert report["flip"]["original_sr"] is not None
    assert report["flip"]["reversed_sr"] is not None
    assert set(report["yearly"]["values"]) == {"years", "median_sr", "winter", "summer"}
    assert report["yearly"]["values"]["years"][-1]["year"] == 2024


# Unit 6: the CLI pipeline, the Rust comparison, and the holdout refusals.


def bar_stamp(ymd, minute):
    return ymd * 1_000_000 + (minute // 60) * 10_000 + (minute % 60) * 100


def test_excluded_days_reasons_and_range():
    eu_min, us_exit = h.ny_minutes(date(2007, 1, 2))
    raw = (
        [bar_stamp(20070102, minute) for minute in (eu_min, 480, us_exit)]
        + [bar_stamp(20070104, 600)]
        + [bar_stamp(20070105, minute) for minute in (eu_min, 480, us_exit)]
    )
    bars = h.Bars(
        stamp=np.array(raw, dtype=np.int64),
        open=np.ones(len(raw)),
        high=np.ones(len(raw)),
        low=np.ones(len(raw)),
        close=np.ones(len(raw)),
        sha256={},
        parsed=len(raw),
    )
    days, _stats = h.computation_days(bars)
    assert [day.date for day in days] == [date(2007, 1, 2), date(2007, 1, 5)]
    assert h.excluded_days(bars.stamp, days) == [
        {"date": "2007-01-01", "reason": "no_bars"},
        {"date": "2007-01-03", "reason": "no_bars"},
        {"date": "2007-01-04", "reason": "missing_boundary"},
    ]


def test_compare_tree_tolerance_rules():
    assert h.compare_tree({"psr": 0.5}, {"psr": 0.5 + 9e-10}) == []
    assert h.compare_tree({"psr": 0.5}, {"psr": 0.5 + 2e-9}) != []
    assert h.compare_tree({"percentile": 1.0}, {"percentile": 1.0 - 9e-10}) == []
    assert h.compare_tree({"w": 1.0}, {"w": 1.0 + 5e-11}) == []
    assert h.compare_tree({"w": 1.0}, {"w": 1.0 + 2e-10}) != []
    assert h.compare_tree({"n": 7}, {"n": 7}) == []
    assert h.compare_tree({"n": 7}, {"n": 8}) == ["summary.n: mismatch"]
    assert h.compare_tree({"x": None}, {"x": None}) == []
    assert h.compare_tree({"x": None}, {"x": 1.0}) == ["summary.x: null mismatch"]
    assert h.compare_tree({"x": 1.0}, {"x": None}) == ["summary.x: null mismatch"]
    assert h.compare_tree({"g": True}, {"g": 1}) == ["summary.g: type differs"]
    assert h.compare_tree({"a": [1, 2]}, {"a": [1]}) == ["summary.a: length differs"]
    assert h.compare_tree({"a": 1}, {"b": 1}) == ["summary: fields differ"]
    assert h.compare_tree("dev", "dev") == []
    assert h.compare_tree("dev", "holdout") == ["summary: mismatch"]


def test_compare_table_matches_and_reports(tmp_path):
    table = pa.table(
        {
            "n": pa.array([1, 2, 3], pa.int64()),
            "x": pa.array([1.0, None, 3.0]),
            "s": pa.array(["a", "b", "c"]),
        }
    )
    path = tmp_path / "out.parquet"
    pq.write_table(table, path)
    assert h.compare_table(table, path, "out") == []
    other = pa.table(
        {
            "n": pa.array([1, 2, 4], pa.int64()),
            "x": pa.array([1.0, None, 3.0 + 2e-9]),
            "s": pa.array(["a", "c", "c"]),
        }
    )
    diffs = h.compare_table(other, path, "out")
    assert len(diffs) == 3
    assert diffs[0] == "out.n[2]: 4 vs 3"
    assert diffs[1].startswith("out.x[2]: 3.000000002 vs 3.0")
    assert diffs[2] == "out.s[1]: c vs b"
    swapped = pa.table(
        {
            "x": pa.array([1.0, None, 3.0]),
            "n": pa.array([1, 2, 3], pa.int64()),
            "s": pa.array(["a", "b", "c"]),
        }
    )
    assert h.compare_table(swapped, path, "out") == ["out: columns differ"]


def test_verify_manifest_checks_files(tmp_path):
    digests = {}
    for name in h.RESULT_FILES:
        (tmp_path / name).write_bytes(name.encode())
        digests[name] = hashlib.sha256((tmp_path / name).read_bytes()).hexdigest()
    manifest = "".join(f"{digests[name]}  {name}\n" for name in h.RESULT_FILES)
    (tmp_path / "SHA256SUMS").write_text(manifest)
    assert h.verify_manifest(tmp_path) == hashlib.sha256(manifest.encode()).hexdigest()
    (tmp_path / "removed_bars.parquet").write_bytes(b"tampered")
    with pytest.raises(ValueError, match="manifest"):
        h.verify_manifest(tmp_path)


def test_iso_timestamp_format():
    assert h.iso_timestamp("20070102 120000") == "2007-01-02T12:00:00"
    assert h.iso_timestamp("20120203 061509") == "2012-02-03T06:15:09"


def test_report_combinations_layout_and_count():
    dev = h.report_combinations("dev")
    assert len(dev) == 38
    assert dev[0] == {"segment": "dev", "table": "tax", "rate": 0.0}
    assert dev[2] == {
        "segment": "dev",
        "table": "cost_target",
        "pip": 0.0,
        "target": 0.30,
    }
    assert dev[10] == {"segment": "dev", "table": "ablation", "name": "eu_only"}
    assert dev[13] == {"segment": "dev", "table": "breakeven"}
    assert dev[19] == {"segment": "recent", "table": "tax", "rate": 0.0}
    holdout = h.report_combinations("holdout")
    assert len(holdout) == 57
    assert holdout[-1] == {"segment": "holdout", "table": "yearly"}
    with pytest.raises(ValueError):
        h.report_combinations("nope")


def test_clock_summary_shape():
    clock = {
        "peak_minute": {"winter": 510, "summer": 510},
        "top_minutes": {
            "winter": [(510, 0.5), (600, 0.25)],
            "summer": [(510, 0.4), (600, 0.2)],
        },
        "sunday_median": {"winter": 1020.0, "summer": 1020.0},
        "sundays": {"winter": 152, "summer": 159},
    }
    summary = h.clock_summary(clock)
    assert summary == {
        "winter": {
            "top": [[510, 0.5], [600, 0.25]],
            "sunday_median": 1020.0,
            "sundays": 152,
        },
        "summer": {
            "top": [[510, 0.4], [600, 0.2]],
            "sunday_median": 1020.0,
            "sundays": 159,
        },
    }


def test_days_table_roundtrip_and_nulls(tmp_path):
    days = synth_days(65)
    frame = h.daily_legs(days)
    table = h.days_table(frame)
    assert table.column("R").null_count == 60
    assert table.schema.field("eu_flag_min").type == pa.int64()
    path = tmp_path / "days.parquet"
    pq.write_table(table, path)
    assert h.compare_table(table, path, "days") == []
    frame.loc[64, "R"] = float(frame.loc[64, "R"]) + 2e-9
    diffs = h.compare_table(h.days_table(frame), path, "days")
    assert len(diffs) == 1
    assert diffs[0].startswith("days.R[64]: ")


def test_accounts_table_roundtrip(tmp_path):
    frame = pd.concat(
        [
            alternating(date(2007, 7, 2), 100, 0.01, -0.004),
            alternating(date(2019, 11, 1), 40, 0.008, -0.003),
        ],
        ignore_index=True,
    )
    spy = flat_spy(date(2007, 7, 2), date(2019, 11, 1), date(2024, 10, 31))
    accounts, _, _ = h.run_segments(frame, spy, "dev")
    table = h.accounts_table(accounts)
    assert table.schema.names == list(h.ACCOUNT_COLUMNS)
    assert table.column("segment").to_pylist()[:2] == ["dev", "dev"]
    path = tmp_path / "accounts.parquet"
    pq.write_table(table, path)
    assert h.compare_table(table, path, "accounts") == []


def test_random_table_roundtrip_and_layout(tmp_path):
    dates = weekday_dates(date(2024, 1, 8), 8)
    days = [dense_day(value)[0] for value in dates]
    frame = control_frame(days, [0.001 * ((i % 5) - 2) for i in range(8)])
    result, _ = h.random_control(frame, days, rounds=3)
    assert result is not None
    table = h.random_table(result)
    assert table.schema.names == list(h.RANDOM_COLUMNS)
    assert table.num_rows == 3 * 8 * 2
    assert table.column("round").to_pylist()[:4] == [0, 0, 0, 0]
    assert table.column("day_index").to_pylist()[:4] == [0, 0, 1, 1]
    assert table.column("leg").to_pylist()[:2] == ["eu", "us"]
    path = tmp_path / "random.parquet"
    pq.write_table(table, path)
    assert h.compare_table(table, path, "random") == []


def test_main_exit_codes(monkeypatch, tmp_path):
    monkeypatch.setattr(
        sys,
        "argv",
        [
            "h006.py",
            "--run",
            str(tmp_path),
            "--stage",
            "dev",
            "--histdata-dir",
            str(tmp_path),
            "--spy",
            str(tmp_path / "SPY.json"),
        ],
    )
    monkeypatch.setattr(
        h, "run", lambda *args, **kwargs: (tmp_path / "verdict.json", "ab" * 32, True)
    )
    assert h.main() == 0
    monkeypatch.setattr(
        h, "run", lambda *args, **kwargs: (tmp_path / "verdict.json", "ab" * 32, False)
    )
    assert h.main() == 1
    monkeypatch.setattr(
        h, "run", lambda *args, **kwargs: (_ for _ in ()).throw(ValueError("boom"))
    )
    assert h.main() == 1


def dev_run_dir(tmp_path, summary, *, envelope_sha=True):
    """A dev run directory with stub results, a valid manifest and envelope."""
    dev = tmp_path / "dev"
    dev.mkdir(exist_ok=True)
    for name in h.RESULT_FILES:
        (dev / name).write_bytes(b"stub")
    (dev / "summary.json").write_text(json.dumps(summary))
    digests = {
        name: hashlib.sha256((dev / name).read_bytes()).hexdigest()
        for name in h.RESULT_FILES
    }
    manifest = "".join(f"{digests[name]}  {name}\n" for name in h.RESULT_FILES)
    (dev / "SHA256SUMS").write_text(manifest)
    manifest_hash = hashlib.sha256(manifest.encode()).hexdigest()
    envelope = {
        "match": True,
        "rust_summary": summary,
        "python_summary": summary,
        "results_sha256": manifest_hash if envelope_sha else "0" * 64,
    }
    (dev / "verdict.json").write_text(json.dumps(envelope))
    return dev / "verdict.json", manifest_hash


def write_ledger(tmp_path, rows):
    ledger = tmp_path / "ledger.jsonl"
    ledger.write_text("".join(json.dumps(row) + "\n" for row in rows))
    return ledger


def authorized_rows(manifest_hash, hashes):
    return [
        {
            "kind": "run",
            "hypothesis": "H006",
            "stage": "dev",
            "data_sha256": hashes,
            "results_sha256": manifest_hash,
        },
        {
            "kind": "verdict",
            "hypothesis": "H006",
            "stage": "dev",
            "result": "pass",
            "gates": {"A": True, "B": True, "C": True, "D": True},
            "results_sha256": manifest_hash,
        },
        {
            "kind": "holdout_open",
            "hypothesis": "H006",
            "stage": "holdout",
            "reason": "test",
        },
    ]


def holdout_inputs(tmp_path, histdata):
    spy = tmp_path / "SPY.json"
    spy.write_text("[]")
    return histdata, spy, h._input_hashes(histdata, spy)


def test_holdout_accepts_authorized_record(tmp_path, histdata):
    histdata, spy, hashes = holdout_inputs(tmp_path, histdata)
    env_path, manifest_hash = dev_run_dir(tmp_path, {"stage": "dev"})
    ledger = write_ledger(tmp_path, authorized_rows(manifest_hash, hashes))
    h.require_holdout(env_path, ledger, histdata, spy)


def test_holdout_refuses_missing_arguments(tmp_path, histdata):
    histdata, spy, hashes = holdout_inputs(tmp_path, histdata)
    env_path, manifest_hash = dev_run_dir(tmp_path, {"stage": "dev"})
    ledger = write_ledger(tmp_path, authorized_rows(manifest_hash, hashes))
    with pytest.raises(ValueError, match="dev-match"):
        h.require_holdout(None, ledger, histdata, spy)
    with pytest.raises(ValueError, match="dev-match"):
        h.require_holdout(env_path, None, histdata, spy)


def test_holdout_refuses_unmatched_envelope(tmp_path, histdata):
    histdata, spy, hashes = holdout_inputs(tmp_path, histdata)
    env_path, manifest_hash = dev_run_dir(tmp_path, {"stage": "dev"})
    ledger = write_ledger(tmp_path, authorized_rows(manifest_hash, hashes))
    envelope = json.loads(env_path.read_text())
    for value in (False, None):
        envelope["match"] = value
        env_path.write_text(json.dumps(envelope))
        with pytest.raises(ValueError, match="must match"):
            h.require_holdout(env_path, ledger, histdata, spy)


def test_holdout_refuses_envelope_results_hash(tmp_path, histdata):
    histdata, spy, hashes = holdout_inputs(tmp_path, histdata)
    env_path, manifest_hash = dev_run_dir(
        tmp_path, {"stage": "dev"}, envelope_sha=False
    )
    ledger = write_ledger(tmp_path, authorized_rows(manifest_hash, hashes))
    with pytest.raises(ValueError, match="results hash"):
        h.require_holdout(env_path, ledger, histdata, spy)


def test_holdout_refuses_summary_mismatch(tmp_path, histdata):
    histdata, spy, hashes = holdout_inputs(tmp_path, histdata)
    env_path, manifest_hash = dev_run_dir(tmp_path, {"stage": "dev"})
    ledger = write_ledger(tmp_path, authorized_rows(manifest_hash, hashes))
    for key in ("rust_summary", "python_summary"):
        envelope = json.loads(env_path.read_text())
        envelope[key] = {"stage": "dev", "extra": 1}
        env_path.write_text(json.dumps(envelope))
        with pytest.raises(ValueError, match="summary mismatch"):
            h.require_holdout(env_path, ledger, histdata, spy)


def test_holdout_refuses_missing_dev_run(tmp_path, histdata):
    histdata, spy, hashes = holdout_inputs(tmp_path, histdata)
    env_path, manifest_hash = dev_run_dir(tmp_path, {"stage": "dev"})
    ledger = write_ledger(tmp_path, authorized_rows(manifest_hash, hashes)[1:])
    with pytest.raises(ValueError, match="missing H006 dev run"):
        h.require_holdout(env_path, ledger, histdata, spy)


def test_holdout_refuses_dev_run_hashes(tmp_path, histdata):
    histdata, spy, hashes = holdout_inputs(tmp_path, histdata)
    rows = authorized_rows("0" * 64, dict(hashes, **{"SPY.json": "0" * 64}))
    env_path, _ = dev_run_dir(tmp_path, {"stage": "dev"})
    ledger = write_ledger(tmp_path, rows)
    with pytest.raises(ValueError, match="input hashes"):
        h.require_holdout(env_path, ledger, histdata, spy)


def test_holdout_refuses_dev_run_results_hash(tmp_path, histdata):
    histdata, spy, hashes = holdout_inputs(tmp_path, histdata)
    rows = authorized_rows("0" * 64, hashes)
    env_path, _ = dev_run_dir(tmp_path, {"stage": "dev"})
    ledger = write_ledger(tmp_path, rows)
    with pytest.raises(ValueError, match="results hash"):
        h.require_holdout(env_path, ledger, histdata, spy)


def test_holdout_refuses_dev_verdict(tmp_path, histdata):
    histdata, spy, hashes = holdout_inputs(tmp_path, histdata)
    env_path, manifest_hash = dev_run_dir(tmp_path, {"stage": "dev"})
    rows = authorized_rows(manifest_hash, hashes)
    ledger = write_ledger(tmp_path, [rows[0], rows[2]])
    with pytest.raises(ValueError, match="missing H006 dev verdict"):
        h.require_holdout(env_path, ledger, histdata, spy)
    for mutate in (
        {"result": "fail"},
        {"gates": {"A": True, "B": False, "C": True, "D": True}},
        {"results_sha256": "0" * 64},
    ):
        rows = authorized_rows(manifest_hash, hashes)
        rows[1] = {**rows[1], **mutate}
        ledger = write_ledger(tmp_path, rows)
        with pytest.raises(ValueError, match="gates or results"):
            h.require_holdout(env_path, ledger, histdata, spy)


def test_holdout_refuses_missing_holdout_open(tmp_path, histdata):
    histdata, spy, hashes = holdout_inputs(tmp_path, histdata)
    env_path, manifest_hash = dev_run_dir(tmp_path, {"stage": "dev"})
    rows = authorized_rows(manifest_hash, hashes)
    ledger = write_ledger(tmp_path, rows[:2])
    with pytest.raises(ValueError, match="holdout_open"):
        h.require_holdout(env_path, ledger, histdata, spy)
    rows[2] = {**rows[2], "hypothesis": "H005"}
    ledger = write_ledger(tmp_path, rows)
    with pytest.raises(ValueError, match="holdout_open"):
        h.require_holdout(env_path, ledger, histdata, spy)


def test_holdout_refuses_used_holdout(tmp_path, histdata):
    histdata, spy, hashes = holdout_inputs(tmp_path, histdata)
    env_path, manifest_hash = dev_run_dir(tmp_path, {"stage": "dev"})
    rows = authorized_rows(manifest_hash, hashes) + [
        {
            "kind": "run",
            "hypothesis": "H006",
            "stage": "holdout",
            "results_sha256": "0" * 64,
        }
    ]
    ledger = write_ledger(tmp_path, rows)
    with pytest.raises(ValueError, match="already used"):
        h.require_holdout(env_path, ledger, histdata, spy)
