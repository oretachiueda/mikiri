import pytest

from s002 import d_offsets


def test_d_offsets_reference_and_bounds():
    offsets = d_offsets(7258)
    assert offsets[:10] == [1501, 3107, 5893, 5182, 6974, 6493, 1900, 5680, 1960, 931]
    assert len(set(offsets)) == 1000
    assert all(252 <= offset <= 7258 - 252 for offset in offsets)
    with pytest.raises(ValueError, match="calendar too short"):
        d_offsets(1502)


def write_cot(path, rows):
    import csv
    import io
    import zipfile

    out = io.StringIO(newline="")
    writer = csv.writer(out)
    writer.writerow(
        [
            " Market and Exchange Names ",
            "As of Date in Form YYYY-MM-DD",
            "CFTC Contract Market Code",
            "Commercial Positions-Long (All)",
            "Commercial Positions-Short (All)",
        ]
        + [f"unused{i}" for i in range(124)]
    )
    for row in rows:
        writer.writerow(list(row) + [""] * 124)
    with zipfile.ZipFile(path, "w") as archive:
        archive.writestr("cot.txt", out.getvalue().encode("latin-1"))


def test_cot_format_ecu_cutoff_and_duplicate(tmp_path):
    from datetime import date

    from s002 import read_cot

    write_cot(
        tmp_path / "a.zip",
        [
            ("EUR", "1998-12-29", "099741", 1, 3),
            ("EURO, FX", "1999-01-05", "099741", 1, 3),
            ("JPY", "2024-11-05", "097741", "bad", "bad"),
        ],
    )
    assert read_cot(tmp_path)["EUR"] == [(date(1999, 1, 5), 0.5)]
    write_cot(tmp_path / "b.zip", [("EURO", "1999-01-05", "099741", 1, 3)])
    with pytest.raises(ValueError, match="duplicate COT"):
        read_cot(tmp_path)


def test_fred_blank_calendar_and_future_invariance(tmp_path):
    from datetime import date

    from s002 import FX, read_fred, return_calendar

    path = tmp_path / "DEXJPUS.csv"
    path.write_text(
        "observation_date,DEXJPUS\n2024-10-29,150\n2024-10-30,\n2024-10-31,151\n2024-11-01,invalid\n"
    )
    data = read_fred(path)
    assert data[date(2024, 10, 30)] is None
    fred = {name: {} for name in FX}
    fred["DEXJPUS"] = data
    assert return_calendar(fred) == [date(2024, 10, 29), date(2024, 10, 31)]
    path.write_text(path.read_text().replace("invalid", "999"))
    assert read_fred(path) == data


def test_spy_json_future_invariance(tmp_path):
    from datetime import date

    from s002 import read_spy

    path = tmp_path / "spy.json"
    path.write_text(
        '[{"date":"2024-10-31T00:00:00Z","adjClose":100},{"date":"2024-11-01T00:00:00Z","adjClose":"bad"}]'
    )
    assert read_spy(path) == {date(2024, 10, 31): 100.0}
    path.write_text(path.read_text().replace('"bad"', "500"))
    assert read_spy(path) == {date(2024, 10, 31): 100.0}


def test_slot_duplicate_and_z_windows():
    from datetime import date, timedelta

    from s002 import latest_slots, slot, z_values

    start = date(1992, 10, 6)
    reports = [(start + timedelta(days=7 * i), float(i % 7)) for i in range(156)]
    assert slot(start) == 0
    later = (start + timedelta(days=3), 10.0)
    assert latest_slots([later, reports[0]])[0] == later
    assert z_values(reports, 52)[reports[50][0]] is None
    assert z_values(reports, 52)[reports[51][0]] is not None
    assert z_values(reports, 156)[reports[155][0]] is not None
    missing = reports[:20] + reports[21:]
    assert z_values(missing, 156)[reports[155][0]] is None


def test_public_dates_and_signal_calendar():
    from datetime import date

    from s002 import public_and_signal

    calendar = [
        date.fromisoformat(s)
        for s in (
            "2013-11-29",
            "2013-12-02",
            "2019-03-29",
            "2019-04-01",
            "2024-01-08",
            "2024-01-09",
        )
    ]
    assert public_and_signal(date(2013, 10, 1), calendar) == (calendar[0], calendar[1])
    assert public_and_signal(date(2019, 3, 12), calendar) == (calendar[2], calendar[3])
    assert public_and_signal(date(2024, 1, 1), calendar) == (calendar[4], calendar[5])


def test_readers_preserve_earlier_values_when_future_changes(tmp_path):
    from datetime import date

    from s002 import read_cot, read_fred, read_spy

    cot = tmp_path / "a.zip"
    write_cot(
        cot,
        [("JPY", "2024-01-02", "097741", 1, 3), ("JPY", "2024-01-09", "097741", 2, 3)],
    )
    fred = tmp_path / "DEXJPUS.csv"
    fred.write_text("observation_date,DEXJPUS\n2024-01-02,150\n2024-01-09,151\n")
    spy = tmp_path / "spy.json"
    spy.write_text(
        '[{"date":"2024-01-02T00:00:00Z","adjClose":100},{"date":"2024-01-09T00:00:00Z","adjClose":101}]'
    )
    day = date(2024, 1, 2)
    before = (read_cot(tmp_path)["JPY"][0], read_fred(fred)[day], read_spy(spy)[day])
    write_cot(
        cot,
        [
            ("JPY", "2024-01-02", "097741", 1, 3),
            ("JPY", "2024-01-09", "097741", 100, 1),
        ],
    )
    fred.write_text(fred.read_text().replace(",151", ",999"))
    spy.write_text(spy.read_text().replace(":101", ":999"))
    assert (
        read_cot(tmp_path)["JPY"][0],
        read_fred(fred)[day],
        read_spy(spy)[day],
    ) == before
