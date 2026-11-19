from datetime import timedelta

import h001
from gym import SwapCase, Trade


def weekday_swap(case: SwapCase) -> float:
    days = (case.exit - case.entry).days
    calendar = [case.entry + timedelta(days=offset) for offset in range(days)]
    weekdays = sum(1 for day in calendar if day.weekday() < 5)
    weekend_carry = sum(2 for day in calendar if day.weekday() == 4)
    return (
        case.notional
        * (case.held_rate - case.sold_rate - case.markup)
        / 365
        * (weekdays + weekend_carry)
    )


def swap_without_weekend_carry(case: SwapCase) -> float:
    total = 0.0
    day = case.entry
    while day < case.exit:
        if day.weekday() < 5:
            total += (
                case.notional * (case.held_rate - case.sold_rate - case.markup) / 365
            )
        day += timedelta(days=1)
    return total


def swap_markup_credited(case: SwapCase) -> float:
    total = 0.0
    day = case.entry
    while day < case.exit:
        if day.weekday() < 5:
            days = 3 if day.weekday() == 4 else 1
            total += (
                case.notional
                * (case.held_rate - case.sold_rate + case.markup)
                / 365
                * days
            )
        day += timedelta(days=1)
    return total


def gate_b_trials_as_length(returns, n_cumulative):
    return h001.psr(list(returns)[:n_cumulative]) >= 1 - 0.05 / (n_cumulative + 1)


def _direction(change):
    return int(change > 0) - int(change < 0)


def momentum_with_entry_bar_confirmation(bars, config):
    trades = []
    i = 1
    while i + config["hold"] < len(bars["stamp"]):
        signal = _direction(bars["close"][i - 1] - bars["open"][i - 1])
        entry_bar = _direction(bars["close"][i] - bars["open"][i])
        if signal and entry_bar == signal:
            end = i + config["hold"]
            trades.append(
                Trade(
                    i,
                    config["sign"] * signal,
                    bars["open"][i],
                    None,
                    end,
                    bars["open"][end],
                )
            )
            i = end
        else:
            i += 1
    return trades


def stop_fill_better_side(bars, j, level, direction):
    return min(bars["open"][j], level) if direction < 0 else max(bars["open"][j], level)
