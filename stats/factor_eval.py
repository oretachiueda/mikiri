"""Cross-sectional diagnostics for factors observed at the close."""

from collections.abc import Iterable
from math import sqrt

import pandas as pd


def forward_returns(
    prices: pd.DataFrame, horizons: Iterable[int]
) -> dict[int, pd.DataFrame]:
    """Return close-to-close simple returns keyed by horizon in panel dates.

    Rows are sorted by date. A missing endpoint or zero starting price leaves
    the return missing.
    """
    prices = prices.sort_index()
    base = prices.mask(prices.eq(0))
    returns = {}
    for horizon in horizons:
        if horizon < 1:
            raise ValueError("horizons must be positive")
        returns[horizon] = prices.shift(-horizon).div(base).sub(1)
    return returns


def information_coefficient(
    factor: pd.DataFrame,
    fwd: pd.DataFrame,
    method: str = "spearman",
    min_assets: int = 3,
) -> pd.Series:
    """Correlate matched factor and return values per date using Spearman or Pearson."""
    if min_assets < 2:
        raise ValueError("min_assets must be at least 2")
    if method not in ("spearman", "pearson"):
        raise ValueError("method must be spearman or pearson")
    factor = factor.sort_index()
    fwd = fwd.reindex_like(factor)
    paired = factor.notna() & fwd.notna()
    enough = paired.sum(axis=1) >= min_assets
    factor, fwd = factor.where(paired).loc[enough], fwd.where(paired).loc[enough]
    if method == "spearman":
        factor, fwd = factor.rank(axis=1), fwd.rank(axis=1)
    return factor.corrwith(fwd, axis=1)


def _buckets(factor: pd.DataFrame, quantiles: int) -> pd.DataFrame:
    if quantiles < 2:
        raise ValueError("quantiles must be at least 2")
    factor = factor.sort_index()
    count = factor.count(axis=1)
    ranks = factor.rank(axis=1, method="first")
    return ((ranks - 1) * quantiles).floordiv(count, axis=0).add(1)


def quantile_returns(
    factor: pd.DataFrame, fwd: pd.DataFrame, quantiles: int
) -> pd.DataFrame:
    """Return mean forward returns in buckets 1 through quantiles, plus spread.

    Ties follow asset column order. Missing factors are excluded. Bucket
    assignment uses the full factor row, even when some returns are missing.
    Dates with fewer assets than buckets are skipped.
    """
    buckets = _buckets(factor, quantiles)
    buckets = buckets.loc[buckets.count(axis=1) >= quantiles]
    fwd = fwd.reindex_like(buckets)
    means = pd.DataFrame(
        {q: fwd.where(buckets.eq(q)).mean(axis=1) for q in range(1, quantiles + 1)},
        index=buckets.index,
    )
    means["spread"] = means[quantiles] - means[1]
    return means


def turnover(factor: pd.DataFrame, quantiles: int) -> pd.DataFrame:
    """Return the fraction of each bucket's current members new since the prior date.

    Bucket assignment matches quantile_returns. The first date and dates
    following a skipped date have missing turnover.
    """
    buckets = _buckets(factor, quantiles)
    valid = buckets.count(axis=1) >= quantiles
    comparable = valid & valid.shift(fill_value=False)
    fractions = {}
    for q in range(1, quantiles + 1):
        members = buckets.eq(q)
        entrants = members & ~members.shift(fill_value=False)
        fractions[q] = entrants.sum(axis=1).div(members.sum(axis=1)).where(comparable)
    return pd.DataFrame(fractions, index=buckets.index).loc[valid]


def _panels(
    factor: pd.DataFrame, prices: pd.DataFrame | None
) -> tuple[pd.DataFrame, pd.DataFrame]:
    if prices is None:
        panel = factor.pivot(index="date", columns="asset", values=["factor", "price"])
        return panel["factor"], panel["price"]
    return factor, prices


def ic_decay(
    factor: pd.DataFrame,
    prices: pd.DataFrame | None = None,
    horizons: Iterable[int] = (1,),
    *,
    method: str = "spearman",
    min_assets: int = 3,
) -> pd.Series:
    """Return mean IC per horizon from wide panels or long date/asset/factor/price rows."""
    factor, prices = _panels(factor, prices)
    return pd.Series(
        {
            horizon: information_coefficient(factor, fwd, method, min_assets).mean()
            for horizon, fwd in forward_returns(prices, horizons).items()
        },
        dtype=float,
    )


def summary(
    factor: pd.DataFrame,
    prices: pd.DataFrame | None = None,
    *,
    horizon: int = 1,
    quantiles: int = 5,
    method: str = "spearman",
    min_assets: int = 3,
) -> dict[str, float]:
    """Summarize one horizon from wide panels or long date/asset/factor/price rows.

    IC t-stat uses the sample standard deviation and the number of defined
    daily ICs. Mean turnover weights defined dates and buckets equally.
    """
    factor, prices = _panels(factor, prices)
    fwd = forward_returns(prices, (horizon,))[horizon]
    ic = information_coefficient(factor, fwd, method, min_assets).dropna()
    mean_ic, ic_std = ic.mean(), ic.std()
    return {
        "mean_ic": mean_ic,
        "ic_std": ic_std,
        "ic_t_stat": mean_ic / (ic_std / sqrt(ic.size)) if ic_std > 0 else float("nan"),
        "mean_spread": quantile_returns(factor, fwd, quantiles)["spread"].mean(),
        "mean_turnover": turnover(factor, quantiles).mean().mean(),
    }
