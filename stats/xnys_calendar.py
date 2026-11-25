"""Generate the committed XNYS session table."""

from importlib.metadata import version
from pathlib import Path

import exchange_calendars as xc

OUTPUT = Path(__file__).resolve().parent.parent / "crates/guard/src/xnys.txt"


def generate() -> str:
    calendar = xc.get_calendar("XNYS", start="1993-01-01", end="2027-11-19")
    early = set(calendar.early_closes)
    header = (
        "# Generated; do not edit by hand. Range: 1993-01-01 to 2027-11-19.\n"
        "# Regenerate: cd stats && uv run python xnys_calendar.py\n"
        f"# Source: exchange_calendars {version('exchange-calendars')} XNYS.\n"
    )
    return header + "".join(
        f"{day.date()} {'E' if day in early else 'F'}\n" for day in calendar.sessions
    )


if __name__ == "__main__":
    OUTPUT.write_text(generate())
