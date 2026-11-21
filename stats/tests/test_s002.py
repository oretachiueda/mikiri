import pytest

from s002 import d_offsets


def test_d_offsets_reference_and_bounds():
    offsets = d_offsets(7258)
    assert offsets[:10] == [1501, 3107, 5893, 5182, 6974, 6493, 1900, 5680, 1960, 931]
    assert len(set(offsets)) == 1000
    assert all(252 <= offset <= 7258 - 252 for offset in offsets)
    with pytest.raises(ValueError, match="calendar too short"):
        d_offsets(1502)
