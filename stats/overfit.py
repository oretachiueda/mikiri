"""CSCV overfitting and combinatorial purged cross-validation."""

from itertools import combinations
from math import comb

import numpy as np


def pbo_cscv(matrix, partitions):
    """Return the fraction of nonpositive logits and the CSCV logits.

    OOS ranks use average ranks for ties, ascending from one, divided by N+1.
    IS ties select the first configuration.
    """
    values = np.asarray(matrix, dtype=float)
    if (
        values.ndim != 2
        or values.shape[1] < 2
        or partitions < 2
        or partitions % 2
        or values.shape[0] < partitions * 2
        or not np.isfinite(values).all()
    ):
        raise ValueError("invalid performance matrix or partitions")
    blocks = np.array_split(np.arange(len(values)), partitions)
    logits = []
    for selected in combinations(range(partitions), partitions // 2):
        inside = np.concatenate([blocks[i] for i in selected])
        outside = np.concatenate(
            [blocks[i] for i in range(partitions) if i not in selected]
        )

        def sharpe(rows):
            sample = values[rows]
            std = sample.std(axis=0, ddof=1)
            if np.any(std == 0):
                raise ValueError("Sharpe is undefined for zero variance")
            return sample.mean(axis=0) / std

        winner = np.argmax(sharpe(inside))
        scores = sharpe(outside)
        rank = (
            1
            + np.count_nonzero(scores < scores[winner])
            + (np.count_nonzero(scores == scores[winner]) - 1) / 2
        )
        omega = rank / (values.shape[1] + 1)
        logits.append(float(np.log(omega / (1 - omega))))
    return float(np.mean(np.array(logits) <= 0)), logits


def cpcv_splits(n_obs, n_groups, k_test, label_horizon, embargo):
    """Return an iterator of purged splits and the number of backtest paths."""
    if (
        n_obs < n_groups
        or n_groups < 2
        or not 0 < k_test < n_groups
        or label_horizon < 0
        or embargo < 0
    ):
        raise ValueError("invalid split parameters")
    groups = np.array_split(np.arange(n_obs), n_groups)

    def splits():
        for selected in combinations(range(n_groups), k_test):
            test = np.concatenate([groups[i] for i in selected])
            allowed = np.ones(n_obs, dtype=bool)
            for i in selected:
                start, end = groups[i][0], groups[i][-1]
                allowed[
                    max(0, start - label_horizon) : min(
                        n_obs, end + max(label_horizon, embargo) + 1
                    )
                ] = False
            yield np.flatnonzero(allowed), test

    return splits(), comb(n_groups - 1, k_test - 1)
