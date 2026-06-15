"""Unit tests for expected_symbols.json fixture files (task 5.4).

Validates that:
  - All 3 fixture repos have a parseable expected_symbols.json
  - Every file_path referenced in symbols resolves to an existing file
  - Each fixture has at least 10 symbols
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest

FIXTURES_ROOT = Path(__file__).resolve().parent.parent / "fixtures" / "repos"

FIXTURE_NAMES = ["mini-ts-app", "mini-py-svc", "mini-go-svc", "mini-cs-app", "mini-java-svc"]


def _load_expected_symbols(fixture: str) -> dict:
    """Load and parse expected_symbols.json for a given fixture."""
    path = FIXTURES_ROOT / fixture / "expected_symbols.json"
    assert path.exists(), f"expected_symbols.json not found at {path}"
    with open(path, encoding="utf-8") as f:
        data = json.load(f)
    return data


class TestExpectedSymbolsSchema:
    """Validate JSON structure and required fields."""

    @pytest.mark.parametrize("fixture", FIXTURE_NAMES)
    def test_parses_correctly(self, fixture: str) -> None:
        data = _load_expected_symbols(fixture)
        assert isinstance(data, dict)
        assert data["version"] == 1
        assert data["language"] in ("typescript", "python", "go", "csharp", "java")
        assert data["fixture"] == fixture
        assert isinstance(data["symbols"], list)

    @pytest.mark.parametrize("fixture", FIXTURE_NAMES)
    def test_symbol_fields(self, fixture: str) -> None:
        data = _load_expected_symbols(fixture)
        for sym in data["symbols"]:
            assert "qualified_name" in sym, f"missing qualified_name in {sym}"
            assert "kind" in sym, f"missing kind in {sym}"
            assert "file_path" in sym, f"missing file_path in {sym}"
            assert "exported" in sym, f"missing exported in {sym}"
            assert "tags" in sym, f"missing tags in {sym}"
            assert sym["kind"] in ("function", "class", "method", "interface", "var", "const"), (
                f"unexpected kind {sym['kind']} in {sym['qualified_name']}"
            )
            assert isinstance(sym["tags"], list)
            assert isinstance(sym["exported"], bool)


class TestExpectedSymbolsFilePaths:
    """Verify every file_path resolves to an actual file."""

    @pytest.mark.parametrize("fixture", FIXTURE_NAMES)
    def test_file_paths_exist(self, fixture: str) -> None:
        data = _load_expected_symbols(fixture)
        fixture_root = FIXTURES_ROOT / fixture
        missing = []
        for sym in data["symbols"]:
            fp = fixture_root / sym["file_path"]
            if not fp.exists():
                missing.append(sym["file_path"])
        assert not missing, f"Fixture {fixture}: these file_paths do not exist: {missing}"


class TestExpectedSymbolsMinCount:
    """Each fixture must have at least 10 symbols."""

    @pytest.mark.parametrize("fixture", FIXTURE_NAMES)
    def test_minimum_symbols(self, fixture: str) -> None:
        data = _load_expected_symbols(fixture)
        count = len(data["symbols"])
        assert count >= 10, f"Fixture {fixture} has only {count} symbols, expected >= 10"
