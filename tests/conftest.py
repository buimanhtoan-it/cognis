"""Shared pytest configuration for cognis tests.

Centralizes:

- A pinned hypothesis profile in CI (per cross-cutting checklist in tasks.md).
- A repo-root path helper so integration tests can locate fixtures without
  hard-coded paths.
"""

from __future__ import annotations

import os
import pathlib

from hypothesis import HealthCheck, settings

# CI profile: pinned seed so flaky-test budget = 0. Local devs see the default.
settings.register_profile(
    "ci",
    deadline=None,
    max_examples=200,
    derandomize=True,
    suppress_health_check=[HealthCheck.too_slow],
    print_blob=True,
)
settings.register_profile(
    "dev",
    deadline=None,
    max_examples=50,
    print_blob=True,
)
settings.load_profile(os.environ.get("HYPOTHESIS_PROFILE", "dev"))


REPO_ROOT: pathlib.Path = pathlib.Path(__file__).resolve().parent.parent
"""Absolute path to the cognis repository root."""

FIXTURES_ROOT: pathlib.Path = REPO_ROOT / "tests" / "fixtures"
"""Absolute path to the fixtures directory (`tests/fixtures/`)."""
