import numpy as np
import pytest

from overfit import cpcv_splits, pbo_cscv


def test_pbo_and_ties():
    rows = np.array([[1, 2], [2, 1], [3, 0], [4, -1], [1, 2], [2, 1], [3, 0], [4, -1]])
    pbo, logits = pbo_cscv(rows, 4)
    assert len(logits) == 6
    assert pbo == sum(x <= 0 for x in logits) / 6
    assert pbo_cscv(np.column_stack((rows[:, 0], rows[:, 0])), 4)[0] == 1
    with pytest.raises(ValueError):
        pbo_cscv(np.ones((8, 2)), 4)


def test_cpcv_purges_label_overlap_and_embargo():
    splits, paths = cpcv_splits(12, 4, 2, 1, 2)
    pairs = list(splits)
    assert len(pairs) == 6
    assert paths == 3
    for train, test in pairs:
        assert all(not (t - 1 <= i <= t + 2) for i in train for t in test)
    with pytest.raises(ValueError):
        cpcv_splits(3, 4, 2, 1, 2)
