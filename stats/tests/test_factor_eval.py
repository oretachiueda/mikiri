from math import asin, pi, sqrt

import pandas as pd
import pytest
from pandas.testing import assert_frame_equal, assert_series_equal

from factor_eval import (
    forward_returns,
    ic_decay,
    information_coefficient,
    quantile_returns,
    summary,
    turnover,
)
from gym import normals


def synthetic_prices(beta, noise_std=0.02):
    days, assets = 180, 96
    factor = pd.DataFrame(normals(59, 1, days * assets).reshape(days, assets))
    noise = pd.DataFrame(normals(59, 2, days * assets).reshape(days, assets))
    returns = beta * factor + noise_std * noise
    prices = 100 * (1 + returns.shift(fill_value=0)).cumprod()
    return factor, prices


def test_planted_signal():
    beta, noise_std = 0.015, 0.02
    factor, prices = synthetic_prices(beta, noise_std)
    # For standard normal X and independent normal noise, Y = beta*X + noise
    # has Pearson rho = beta / sqrt(beta**2 + noise_std**2).
    # Bivariate normal Spearman rho is 6/pi * asin(Pearson rho / 2).
    pearson = beta / sqrt(beta**2 + noise_std**2)
    expected = 6 / pi * asin(pearson / 2)
    decay = ic_decay(factor, prices, (1, 2))
    assert decay[1] == pytest.approx(expected, abs=0.025)
    assert 0 < decay[2] < decay[1]
    metrics = summary(factor, prices)
    assert metrics["mean_ic"] == pytest.approx(decay[1])
    assert metrics["mean_spread"] > 0


def test_noise_factor_has_small_ic_t_stat():
    factor, prices = synthetic_prices(beta=0)
    metrics = summary(factor, prices)
    assert abs(metrics["ic_t_stat"]) < 3
    assert metrics["ic_t_stat"] == pytest.approx(
        metrics["mean_ic"] * sqrt(len(factor) - 1) / metrics["ic_std"]
    )


def test_forward_returns_use_exact_endpoints_without_filling():
    prices = pd.DataFrame({"A": [121.0, 100.0, None]}, index=[2, 0, 1])
    fwd = forward_returns(prices, (1, 2))
    assert fwd[1]["A"].isna().all()
    assert fwd[2].loc[0, "A"] == pytest.approx(0.21)
    assert fwd[2].loc[[1, 2], "A"].isna().all()


def test_diagnostics_at_t_are_unchanged_by_prices_after_t_plus_horizon():
    factor, prices = synthetic_prices(beta=0.015)
    date, horizon = 10, 3
    changed = prices.copy()
    changed.loc[date + horizon + 1 :] *= 7
    original_fwd = forward_returns(prices, (horizon,))[horizon]
    changed_fwd = forward_returns(changed, (horizon,))[horizon]
    assert not original_fwd.loc[date + 1].equals(changed_fwd.loc[date + 1])

    def at_date(panel):
        fwd = forward_returns(panel, (horizon,))[horizon]
        return (
            information_coefficient(factor, fwd).loc[date],
            quantile_returns(factor, fwd, 5).loc[[date]],
            turnover(factor, 5).loc[[date]],
        )

    original_ic, original_quantiles, original_turnover = at_date(prices)
    changed_ic, changed_quantiles, changed_turnover = at_date(changed)
    assert changed_ic == original_ic
    assert_frame_equal(changed_quantiles, original_quantiles)
    assert_frame_equal(changed_turnover, original_turnover)


def test_quantile_ties_missing_values_and_sparse_dates():
    factor = pd.DataFrame(
        [[1, 1, None, 2], [None, None, None, 4], [1, 1, None, 2]],
        columns=list("ABCD"),
    )
    fwd = pd.DataFrame(
        [[0.1, None, 9.0, 0.7], [0.2, 0.3, 9.0, 0.6], [0.2, 0.3, 9.0, 0.6]],
        columns=factor.columns,
    )
    result = quantile_returns(factor, fwd, 3)
    assert result.index.tolist() == [0, 2]
    assert result.loc[0, 1] == 0.1
    assert pd.isna(result.loc[0, 2])
    assert result.loc[0, 3] == 0.7
    assert result.loc[0, "spread"] == pytest.approx(0.6)
    assert turnover(factor, 3).reindex([0, 2]).isna().all().all()
    assert information_coefficient(factor, fwd).index.tolist() == [2]
    assert information_coefficient(factor, fwd, min_assets=2).index.tolist() == [0, 2]


def test_spearman_ranks_only_the_matched_cross_section():
    factor = pd.DataFrame([[1, 2, 3, 4], [1, 1, 1, 1]])
    fwd = pd.DataFrame([[1, None, 2, 3], [1, 2, 3, 4]])
    spearman = information_coefficient(factor, fwd)
    assert spearman.loc[0] == pytest.approx(1.0)
    assert pd.isna(spearman.loc[1])
    pearson = information_coefficient(factor.iloc[:1], fwd, method="pearson")
    assert 0 < pearson.loc[0] < spearman.loc[0]


def test_turnover_constant_and_reversing_ranks():
    ascending = [1, 2, 3, 4, 5, 6]
    descending = ascending[::-1]
    constant = turnover(pd.DataFrame([ascending] * 3), 3)
    reversing = turnover(pd.DataFrame([ascending, descending, ascending]), 3)
    assert constant.iloc[0].isna().all()
    assert constant.iloc[1:].eq(0).all().all()
    assert reversing[[1, 3]].iloc[1:].eq(1).all().all()
    assert reversing[2].iloc[1:].eq(0).all()

    partial = pd.DataFrame([ascending, [1, 2, 3, 5, 4, 6]])
    assert turnover(partial, 3).loc[1].tolist() == [0, 0.5, 0.5]


def test_long_and_wide_inputs_agree():
    factor, prices = synthetic_prices(beta=0.015)
    long = (
        pd.concat({"factor": factor, "price": prices}, axis=1)
        .rename_axis(index="date", columns=[None, "asset"])
        .stack("asset")
        .reset_index()
    )
    assert_series_equal(
        ic_decay(long, horizons=(1, 2)), ic_decay(factor, prices, (1, 2))
    )
    assert summary(long) == pytest.approx(summary(factor, prices))


def test_invalid_diagnostic_parameters():
    panel = pd.DataFrame([[1, 2]])
    with pytest.raises(ValueError, match="horizons must be positive"):
        forward_returns(panel, (0,))
    with pytest.raises(ValueError, match="min_assets must be at least 2"):
        information_coefficient(panel, panel, min_assets=1)
    with pytest.raises(ValueError, match="method must be spearman or pearson"):
        information_coefficient(panel, panel, method="kendall")
    with pytest.raises(ValueError, match="quantiles must be at least 2"):
        turnover(panel, 1)
