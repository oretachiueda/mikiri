from pathlib import Path
from zipfile import ZipFile

import numpy as np

import gym


def test_scenarios():
    seed = gym.seed_from([Path(gym.__file__)])
    for name, scenario in gym.SCENARIOS.items():
        assert scenario.check(gym.REFERENCE, seed).passed
        for mutant, pipeline in gym.MUTANTS.items():
            target = "G2" if mutant.startswith("swap_") else "G3"
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
