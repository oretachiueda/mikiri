"""Independent S002 computations."""


def d_offsets(t: int) -> list[int]:
    """Generate the 1,000 distinct preregistered circular shifts."""
    if t - 503 < 1000:
        raise ValueError("calendar too short for 1,000 distinct offsets")
    mask = (1 << 64) - 1
    state = 2026112101
    seen: set[int] = set()
    offsets = []
    while len(offsets) < 1000:
        state = (state + 0x9E3779B97F4A7C15) & mask
        z = state
        z = ((z ^ (z >> 30)) * 0xBF58476D1CE4E5B9) & mask
        z = ((z ^ (z >> 27)) * 0x94D049BB133111EB) & mask
        offset = 252 + ((z ^ (z >> 31)) % (t - 503))
        if offset not in seen:
            seen.add(offset)
            offsets.append(offset)
    return offsets
