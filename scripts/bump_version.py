#!/usr/bin/env python3
"""One-command version bump across every file that must carry the version.

Why this exists: a release used to require hand-editing the version in 5 places
(pyproject, the extension package.json + its lockfile twice, the README, the
CHANGELOG). Hand-editing N files is how a release ships with mismatched versions
(extension 0.7.0 talking to engine 0.6.x). This makes the bump a single command
and a single source of intent, and adds a ``--check`` mode CI can run to fail the
build if the version files ever drift apart.

Docs/READMEs intentionally carry **no** hardcoded version (the README shows a
git-tag badge; docs use ``<version>`` placeholders), so they never need bumping.

Usage::

    python scripts/bump_version.py 0.8.0     # write the new version everywhere
    python scripts/bump_version.py --check    # assert all files already agree
"""

from __future__ import annotations

import argparse
import datetime
import re
import sys
from pathlib import Path

_ROOT = Path(__file__).resolve().parent.parent
_PYPROJECT = _ROOT / "pyproject.toml"
_PKG = _ROOT / "apps" / "cognis-vscode" / "package.json"
_LOCK = _ROOT / "apps" / "cognis-vscode" / "package-lock.json"
_CHANGELOG = _ROOT / "CHANGELOG.md"

_SEMVER = re.compile(r"^\d+\.\d+\.\d+$")


def _read(path: Path) -> str:
    return path.read_text(encoding="utf-8")


def _pyproject_version(text: str) -> str | None:
    m = re.search(r'(?m)^version\s*=\s*"([^"]+)"', text)
    return m.group(1) if m else None


def _first_json_version(text: str) -> str | None:
    m = re.search(r'"version"\s*:\s*"([^"]+)"', text)
    return m.group(1) if m else None


def current_versions() -> dict[str, str | None]:
    """Return the version string each machine file currently declares."""
    lock_text = _read(_LOCK)
    lock_versions = re.findall(r'"version"\s*:\s*"([^"]+)"', lock_text)
    return {
        "pyproject.toml": _pyproject_version(_read(_PYPROJECT)),
        "package.json": _first_json_version(_read(_PKG)),
        # package-lock declares the version twice (root + packages[""]); both must match.
        "package-lock.json[root]": lock_versions[0] if lock_versions else None,
        "package-lock.json[packages]": lock_versions[1] if len(lock_versions) > 1 else None,
    }


def check() -> int:
    """Assert every machine file agrees on the version. Exit 1 on drift."""
    versions = current_versions()
    for name, value in versions.items():
        print(f"  {name:<32} {value}")
    distinct = {v for v in versions.values() if v is not None}
    if None in versions.values():
        print("FAIL: a version field could not be read.", file=sys.stderr)
        return 1
    if len(distinct) != 1:
        print(f"FAIL: version drift across files: {sorted(distinct)}", file=sys.stderr)
        return 1
    print(f"PASS: all version files agree on {distinct.pop()}.")
    return 0


def _bump_pyproject(new: str) -> None:
    text = _read(_PYPROJECT)
    text, n = re.subn(r'(?m)^(version\s*=\s*)"[^"]+"', rf'\g<1>"{new}"', text, count=1)
    if n != 1:
        raise SystemExit("ERROR: could not find the [project] version in pyproject.toml")
    _PYPROJECT.write_text(text, encoding="utf-8")


def _bump_json_first(path: Path, new: str, count: int) -> None:
    """Replace the first *count* ``"version": "..."`` occurrences (format-preserving).

    package.json has exactly one (root); package-lock has two (root +
    packages[""]) as its first two version keys — dependency versions appear far
    later in the file, so a top-anchored count is precise.
    """
    text = _read(path)
    text, n = re.subn(r'("version"\s*:\s*)"[^"]+"', rf'\g<1>"{new}"', text, count=count)
    if n != count:
        raise SystemExit(f"ERROR: expected {count} version field(s) in {path.name}, replaced {n}")
    path.write_text(text, encoding="utf-8")


def _scaffold_changelog(new: str) -> None:
    text = _read(_CHANGELOG)
    if f"## [{new}]" in text:
        return  # section already present; leave the human's notes intact
    today = datetime.date.today().isoformat()
    anchor = "## [Unreleased]\n"
    if anchor not in text:
        print("WARN: no '## [Unreleased]' section in CHANGELOG; skipping scaffold.")
        return
    new_section = f"{anchor}\n## [{new}] — {today}\n\n_TODO: summarize this release._\n"
    _CHANGELOG.write_text(text.replace(anchor, new_section, 1), encoding="utf-8")


def bump(new: str) -> int:
    if not _SEMVER.match(new):
        print(f"ERROR: '{new}' is not a MAJOR.MINOR.PATCH version.", file=sys.stderr)
        return 1
    _bump_pyproject(new)
    _bump_json_first(_PKG, new, count=1)
    _bump_json_first(_LOCK, new, count=2)
    _scaffold_changelog(new)
    print(f"Bumped to {new}:")
    for name, value in current_versions().items():
        print(f"  {name:<32} {value}")
    print(
        "\nNext: fill the new CHANGELOG section, then verify with "
        "`python scripts/bump_version.py --check`."
    )
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description="Bump the project version across all files.")
    parser.add_argument("version", nargs="?", help="New MAJOR.MINOR.PATCH version.")
    parser.add_argument(
        "--check",
        action="store_true",
        help="Only verify all version files agree (CI gate); make no changes.",
    )
    args = parser.parse_args()

    if args.check:
        return check()
    if not args.version:
        parser.error("provide a version (e.g. 0.8.0) or use --check")
    return bump(args.version)


if __name__ == "__main__":
    sys.exit(main())
