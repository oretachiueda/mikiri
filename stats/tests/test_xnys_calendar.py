from xnys_calendar import OUTPUT, generate


def test_committed_calendar_matches_generator():
    assert OUTPUT.read_text() == generate(), (
        "Run: cd stats && uv run python xnys_calendar.py"
    )
