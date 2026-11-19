from pathlib import Path
from zipfile import ZipFile

import numpy as np

import gym
from gym_hidden import CASES


def test_scenarios():
    seed = gym.seed_from(gym.seeded_files())
    for name, scenario in gym.SCENARIOS.items():
        assert scenario.check(gym.REFERENCE, seed).passed
        for mutant, pipeline in gym.MUTANTS.items():
            target = (
                "G4"
                if mutant == "stop_fill_at_level"
                else "G5"
                if mutant == "stop_width_includes_entry_bar"
                else "G1"
                if mutant.startswith("signal_")
                else "G2"
                if mutant.startswith("swap_")
                else "G3"
            )
            assert scenario.check(pipeline, seed).passed == (name != target), mutant


def test_histdata(tmp_path):
    stamps = [20240101000000, 20240101000100]
    values = [1.0, 1.1]
    path = gym.write_histdata(
        tmp_path, "EURUSD", 2024, stamps, values, values, values, values
    )
    with ZipFile(path) as archive:
        assert archive.namelist() == ["DAT_ASCII_EURUSD_M1_2024.csv"]
        assert archive.read(archive.namelist()[0]).count(b"\r\n") == 2
    result = gym.read_histdata_zip(path, "EURUSD", 2024)
    assert result["stamp"].tolist() == stamps
    for key in ("open", "high", "low", "close"):
        assert result[key].tolist() == values


def test_seed(tmp_path):
    a, b = tmp_path / "a", tmp_path / "b"
    a.write_bytes(b"a")
    b.write_bytes(b"b")
    seed = gym.seed_from([a, b])
    assert seed == gym.seed_from([b, a])
    b.write_bytes(b"c")
    assert seed != gym.seed_from([a, b])


def test_normals():
    values = gym.normals(42, 0, 100000)
    assert np.array_equal(values, gym.normals(42, 0, 100000))
    assert abs(values.mean()) < 0.02
    assert abs(values.std() - 1) < 0.02


def test_trades_and_daily_returns():
    bars = gym.hourly(gym.synthetic_m1(42, 0, 3 * 1440, 4, 0.00001, 1.0))
    trades = gym.momentum_trades(bars, {"sign": 1, "hold": 4})
    assert trades
    for trade in trades:
        assert trade.entry >= 1
        assert trade.direction == np.sign(
            bars["close"][trade.entry - 1] - bars["open"][trade.entry - 1]
        )
    expected = sum(
        (t.exit_price - t.entry_price) * t.direction / t.entry_price for t in trades
    )
    assert np.isclose(gym.daily_returns(bars, trades).sum(), expected)


def test_hidden():
    seed = gym.seed_from(gym.seeded_files())
    for case in CASES:
        assert (
            gym.SCENARIOS[case.scenario].check(case.pipeline, seed).passed
            == case.expected_pass
        ), case.name


def test_no_protected_paths(monkeypatch):
    import builtins
    import zipfile

    root = Path(gym.__file__).resolve().parent.parent
    protected = [root / name for name in ("data", "results", "live", "secrets")]

    def guard(path):
        if isinstance(path, (str, bytes, Path)):
            resolved = Path(path).resolve()
            assert not any(resolved.is_relative_to(p) for p in protected)

    original_open, original_path_open, original_zip = (
        builtins.open,
        Path.open,
        zipfile.ZipFile,
    )

    def open_guard(path, *args, **kwargs):
        guard(path)
        return original_open(path, *args, **kwargs)

    def path_guard(path, *args, **kwargs):
        guard(path)
        return original_path_open(path, *args, **kwargs)

    def zip_guard(path, *args, **kwargs):
        guard(path)
        return original_zip(path, *args, **kwargs)

    monkeypatch.setattr(builtins, "open", open_guard)
    monkeypatch.setattr(Path, "open", path_guard)
    monkeypatch.setattr(zipfile, "ZipFile", zip_guard)
    monkeypatch.setattr(gym, "ZipFile", zip_guard)
    gym.paths_for_seed.cache_clear()
    assert gym.main([]) == 0
