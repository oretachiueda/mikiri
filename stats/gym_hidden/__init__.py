from dataclasses import replace

from gym import REFERENCE, HiddenCase
from gym_hidden.alternatives import (
    gate_b_trials_as_length,
    momentum_with_entry_bar_confirmation,
    stop_fill_better_side,
    swap_markup_credited,
    swap_without_weekend_carry,
    weekday_swap,
)

CASES: list[HiddenCase] = [
    HiddenCase(
        "swap_friday_carries_one_day",
        "G2",
        replace(REFERENCE, swap=swap_without_weekend_carry),
        False,
    ),
    HiddenCase(
        "swap_markup_credited",
        "G2",
        replace(REFERENCE, swap=swap_markup_credited),
        False,
    ),
    HiddenCase(
        "gate_trials_as_sample_length",
        "G3",
        replace(REFERENCE, gate_b=gate_b_trials_as_length),
        False,
    ),
    HiddenCase(
        "trades_confirm_on_entry_bar",
        "G1",
        replace(REFERENCE, trades=momentum_with_entry_bar_confirmation),
        False,
    ),
    HiddenCase(
        "stop_fill_better_side",
        "G4",
        replace(REFERENCE, fill=stop_fill_better_side),
        False,
    ),
    HiddenCase(
        "swap_weekday_index",
        "G2",
        replace(REFERENCE, swap=weekday_swap),
        True,
    ),
]
