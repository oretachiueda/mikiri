import hashlib
import io
import zipfile
from datetime import date, datetime, timedelta

import numpy as np
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
