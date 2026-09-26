"""Load and validate the small TOML deployment contract before allocating sessions."""

import tomllib
from dataclasses import dataclass
from pathlib import Path
from typing import Self

# Each setting and the largest value the shipped deployment treats as intentional: owners
# duplicate the graph set, pending charts each hold one 1024x1024 image, and a batch pays for
# the decoder's key/value cache.
CAPACITIES = (("session_size", 8), ("queue_size", 4096), ("batch_size", 32))
SETTING_NAMES = frozenset(name for name, _ in CAPACITIES)


@dataclass(frozen=True)
class Settings:
    """Bound model owners, pending requests, and charts per inference call."""

    session_size: int = 1
    queue_size: int = 128
    batch_size: int = 1

    def __post_init__(self) -> None:
        """Reject booleans, nonintegers, and capacities that disable bounded scheduling."""
        # Each owner loads its own copy of every graph, so an unbounded owner count would only
        # fail after minutes of loading and gigabytes of device memory. Every capacity is capped
        # so a typo fails at startup instead.
        for name, maximum in CAPACITIES:
            value = getattr(self, name)
            if type(value) is not int or not 1 <= value <= maximum:
                raise ValueError(f"{name} must be an integer between 1 and {maximum}")

    @classmethod
    def load(cls, path: Path) -> Self:
        """Parse one explicit file and reject misspellings instead of ignoring settings."""
        with path.open("rb") as source:
            values = tomllib.load(source)
        unknown = values.keys() - SETTING_NAMES
        if unknown:
            raise ValueError(
                f"Unknown configuration keys: {', '.join(sorted(unknown))}"
            )
        return cls(**values)
