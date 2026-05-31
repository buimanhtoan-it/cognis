"""Shared cognis branding helpers."""

from __future__ import annotations

import sys
from pathlib import Path

from cognis import __version__

TAGLINE = "Software Cognition Engine"


def logo_path() -> Path | None:
    """Return the bundled logo path when the asset ships with the package."""
    candidate = Path(__file__).resolve().parent / "assets" / "logo.png"
    return candidate if candidate.is_file() else None


def format_banner(*, prog: str = "cognis") -> str:
    """Return a one-line startup banner for CLI and daemon entry points."""
    return f"{prog} v{__version__} — {TAGLINE}"


def echo_banner(*, prog: str = "cognis", file: object | None = None) -> None:
    """Print the startup banner when attached to an interactive terminal."""
    stream = file if file is not None else sys.stderr
    isatty = getattr(stream, "isatty", lambda: False)
    if isatty():
        print(format_banner(prog=prog), file=stream)  # type: ignore[arg-type]
