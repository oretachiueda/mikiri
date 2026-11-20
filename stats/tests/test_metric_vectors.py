"""Fixed metric vectors computed once with empyrical-reloaded 0.5.0 (Apache-2.0); not a dependency."""

import math

from h006 import _drawdown, _sr

VECTORS = [
    (
        [0.01, 0.02, 0.015, 0.005, 0.03, 0.012, 0.018, 0.007, 0.025, 0.011],
        30.463302430398368,
        0.0,
    ),
    (
        [0.02, -0.01, 0.015, -0.025, 0.03, -0.005, 0.01, -0.012, 0.018, 0.004],
        4.1579487189486981,
        -0.025000000000000081,
    ),
    (
        [0.05, 0.02, -0.20, -0.10, 0.08, 0.12, 0.15, 0.04, 0.03, 0.02],
        3.2500404150309685,
        -0.27999999999999992,
    ),
    (
        [0.01, 0.0, -0.01, 0.02, 0.0, -0.005, 0.015, -0.01, 0.005, 0.01],
        5.4007873441756491,
        -0.010000000000000051,
    ),
]


def test_sharpe_vectors():
    for returns, expected, _ in VECTORS:
        actual = _sr(returns)
        assert actual is not None
        assert math.isclose(actual, expected, rel_tol=1e-12)


def test_drawdown_vectors():
    for returns, _, expected in VECTORS:
        wealth = [
            math.prod(1 + r for r in returns[:i]) for i in range(1, len(returns) + 1)
        ]
        assert math.isclose(-_drawdown(wealth), expected, rel_tol=1e-12, abs_tol=1e-15)
