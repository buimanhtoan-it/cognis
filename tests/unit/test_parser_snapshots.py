"""Snapshot tests for parser output against fixture expected_symbols.json files (task 6.7).

For each fixture repo (mini-ts-app, mini-py-svc, mini-go-svc) we:
1. Walk all source files under the fixture directory.
2. Parse each file with the appropriate language parser.
3. Assert that every ``qualified_name`` in ``expected_symbols.json`` is present
   in the parsed output.

These tests are "snapshot" in the sense that the expected output was manually
curated in task 5 (test fixtures) and serves as a regression baseline.

Design notes:
- We do NOT assert that NO extra symbols are emitted — parsers may emit more
  than the fixture lists (e.g. helper functions). The fixture only lists the
  "important" public API surface.
- The ``qualified_name`` in the fixture uses the format
  ``<lang>:<file_path>:<name>`` which is the same as the parser output.
"""

from __future__ import annotations

import json
import os
import pathlib
from typing import Any

import pytest

# ---------------------------------------------------------------------------
# Skip if optional tree-sitter deps not installed
# ---------------------------------------------------------------------------
try:
    from cognis_indexer.parsers.go import GoParser
    from cognis_indexer.parsers.python import PythonParser
    from cognis_indexer.parsers.typescript import TypeScriptParser

    _PARSERS_AVAILABLE = True
except ImportError:
    _PARSERS_AVAILABLE = False

pytestmark = pytest.mark.unit

skip_if_no_parsers = pytest.mark.skipif(
    not _PARSERS_AVAILABLE,
    reason="tree-sitter optional deps not installed",
)

FIXTURES_DIR = pathlib.Path(__file__).parent.parent / "fixtures" / "repos"


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _load_expected(fixture_dir: pathlib.Path) -> list[dict[str, Any]]:
    path = fixture_dir / "expected_symbols.json"
    with path.open() as f:
        data = json.load(f)
    return data["symbols"]  # type: ignore[return-value]


def _walk_source_files(fixture_dir: pathlib.Path, extension: str) -> list[tuple[str, str]]:
    """Walk fixture dir and return (file_path, source) pairs.

    ``file_path`` is repo-relative with forward slashes.
    """
    results = []
    for root, _dirs, files in os.walk(fixture_dir):
        for fname in files:
            if fname.endswith(extension):
                abs_path = pathlib.Path(root) / fname
                rel_path = abs_path.relative_to(fixture_dir).as_posix()
                source = abs_path.read_text(encoding="utf-8", errors="replace")
                results.append((rel_path, source))
    return results


def _parse_all(parser: Any, fixture_dir: pathlib.Path, extension: str) -> set[str]:
    """Parse all files in *fixture_dir* with *extension* and return all qualified_names."""
    qnames: set[str] = set()
    for file_path, source in _walk_source_files(fixture_dir, extension):
        symbols = parser.parse(source, file_path)
        for sym in symbols:
            qnames.add(sym.qualified_name)
    return qnames


# ---------------------------------------------------------------------------
# TypeScript snapshot test
# ---------------------------------------------------------------------------


@skip_if_no_parsers
class TestTypeScriptSnapshot:
    """Snapshot: TypeScript parser against mini-ts-app/expected_symbols.json."""

    def test_all_expected_ts_symbols_found(self) -> None:
        fixture_dir = FIXTURES_DIR / "mini-ts-app"
        if not fixture_dir.exists():
            pytest.skip("mini-ts-app fixture not present")

        expected = _load_expected(fixture_dir)
        parser = TypeScriptParser()
        found_qnames = _parse_all(parser, fixture_dir, ".ts")

        missing = []
        for sym_entry in expected:
            qname = sym_entry["qualified_name"]
            if qname not in found_qnames:
                missing.append(qname)

        if missing:
            # Show first 10 missing for readability
            sample = missing[:10]
            pytest.fail(
                f"{len(missing)} expected symbol(s) not found in TS parse output.\n"
                f"First up to 10 missing:\n"
                + "\n".join(f"  - {q}" for q in sample)
                + (f"\n  ... and {len(missing) - 10} more" if len(missing) > 10 else "")
            )

    def test_ts_symbols_have_valid_fields(self) -> None:
        fixture_dir = FIXTURES_DIR / "mini-ts-app"
        if not fixture_dir.exists():
            pytest.skip("mini-ts-app fixture not present")

        parser = TypeScriptParser()
        for file_path, source in _walk_source_files(fixture_dir, ".ts"):
            symbols = parser.parse(source, file_path)
            for sym in symbols:
                assert sym.id, f"empty id in {file_path}"
                assert sym.qualified_name.startswith("ts:"), sym.qualified_name
                assert sym.line_start >= 1
                assert sym.line_end >= sym.line_start
                assert len(sym.content_hash) == 16


# ---------------------------------------------------------------------------
# Python snapshot test
# ---------------------------------------------------------------------------


@skip_if_no_parsers
class TestPythonSnapshot:
    """Snapshot: Python parser against mini-py-svc/expected_symbols.json."""

    def test_all_expected_py_symbols_found(self) -> None:
        fixture_dir = FIXTURES_DIR / "mini-py-svc"
        if not fixture_dir.exists():
            pytest.skip("mini-py-svc fixture not present")

        expected = _load_expected(fixture_dir)
        parser = PythonParser()
        found_qnames = _parse_all(parser, fixture_dir, ".py")

        missing = []
        for sym_entry in expected:
            qname = sym_entry["qualified_name"]
            if qname not in found_qnames:
                missing.append(qname)

        if missing:
            sample = missing[:10]
            pytest.fail(
                f"{len(missing)} expected symbol(s) not found in Python parse output.\n"
                f"First up to 10 missing:\n"
                + "\n".join(f"  - {q}" for q in sample)
                + (f"\n  ... and {len(missing) - 10} more" if len(missing) > 10 else "")
            )

    def test_py_symbols_have_valid_fields(self) -> None:
        fixture_dir = FIXTURES_DIR / "mini-py-svc"
        if not fixture_dir.exists():
            pytest.skip("mini-py-svc fixture not present")

        parser = PythonParser()
        for file_path, source in _walk_source_files(fixture_dir, ".py"):
            symbols = parser.parse(source, file_path)
            for sym in symbols:
                assert sym.id, f"empty id in {file_path}"
                assert sym.qualified_name.startswith("py:"), sym.qualified_name
                assert sym.line_start >= 1
                assert sym.line_end >= sym.line_start
                assert len(sym.content_hash) == 16


# ---------------------------------------------------------------------------
# Go snapshot test
# ---------------------------------------------------------------------------


@skip_if_no_parsers
class TestGoSnapshot:
    """Snapshot: Go parser against mini-go-svc/expected_symbols.json."""

    def test_all_expected_go_symbols_found(self) -> None:
        fixture_dir = FIXTURES_DIR / "mini-go-svc"
        if not fixture_dir.exists():
            pytest.skip("mini-go-svc fixture not present")

        expected = _load_expected(fixture_dir)
        parser = GoParser()
        found_qnames = _parse_all(parser, fixture_dir, ".go")

        missing = []
        for sym_entry in expected:
            qname = sym_entry["qualified_name"]
            if qname not in found_qnames:
                missing.append(qname)

        if missing:
            sample = missing[:10]
            pytest.fail(
                f"{len(missing)} expected symbol(s) not found in Go parse output.\n"
                f"First up to 10 missing:\n"
                + "\n".join(f"  - {q}" for q in sample)
                + (f"\n  ... and {len(missing) - 10} more" if len(missing) > 10 else "")
            )

    def test_go_symbols_have_valid_fields(self) -> None:
        fixture_dir = FIXTURES_DIR / "mini-go-svc"
        if not fixture_dir.exists():
            pytest.skip("mini-go-svc fixture not present")

        parser = GoParser()
        for file_path, source in _walk_source_files(fixture_dir, ".go"):
            symbols = parser.parse(source, file_path)
            for sym in symbols:
                assert sym.id, f"empty id in {file_path}"
                assert sym.qualified_name.startswith("go:"), sym.qualified_name
                assert sym.line_start >= 1
                assert sym.line_end >= sym.line_start
                assert len(sym.content_hash) == 16
