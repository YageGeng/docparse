"""Load and validate the small TOML deployment contract before allocating sessions."""

import tomllib
from dataclasses import dataclass
from pathlib import Path
from typing import Self


@dataclass(frozen=True)
class Settings:
    """Bound model owners, pending requests, and crops per inference call."""

    session_size: int = 1
    queue_size: int = 128
    batch_size: int = 16

    def __post_init__(self) -> None:
        """Reject booleans, nonintegers, and capacities that disable bounded scheduling."""
        for name in ("session_size", "queue_size", "batch_size"):
            value = getattr(self, name)
            if type(value) is not int or value < 1:
                raise ValueError(f"{name} must be a positive integer")
        if self.batch_size > 32:
            raise ValueError("batch_size must be between 1 and 32")

    @classmethod
    def load(cls, path: Path) -> Self:
        """Parse one explicit file and reject misspellings instead of ignoring settings."""
        with path.open("rb") as source:
            values = tomllib.load(source)
        unknown = values.keys() - {"session_size", "queue_size", "batch_size"}
        if unknown:
            raise ValueError(
                f"Unknown configuration keys: {', '.join(sorted(unknown))}"
            )
        return cls(**values)
