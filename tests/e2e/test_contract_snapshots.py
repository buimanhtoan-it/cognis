"""Generate + verify golden contract snapshots of real Python app output.

The VS Code extension (TypeScript) parses JSON emitted by the Python CLI and
the indexd status file. Those JSON shapes are a cross-language contract: if the
Python side renames or drops a field, the extension silently breaks. Pure unit
tests on either side can't catch that because each mocks the other.

This module captures the *real* CLI / daemon output into normalized golden
files under ``tests/e2e/contracts/``. A companion TypeScript test
(``apps/cognis-vscode/src/test/contractParity.test.ts``) loads the same golden
files and runs them through the extension's real parsers/interfaces, so a drift
on either side fails a test.

Regenerate the goldens after an intentional contract change with::

    COGNIS_UPDATE_CONTRACTS=1 pytest -m e2e -k contract_snapshots

Without that env var the test asserts the freshly captured output still matches
the committed golden (key sets only — values are environment-specific paths).
"""

from __future__ import annotations

import json
import os
from pathlib import Path
from typing import Any

import pytest

from tests.e2e.harness import (
    IndexdProcess,
    run_cli,
    run_cli_json,
)

pytestmark = pytest.mark.e2e

CONTRACTS_DIR = Path(__file__).resolve().parent / "contracts"


def _key_skeleton(value: Any) -> Any:
    """Reduce a JSON value to its *structure* (keys + types), dropping values.

    This keeps the golden files stable across machines (absolute paths, pids,
    timestamps differ) while still pinning the contract shape the extension
    depends on.
    """
    if isinstance(value, dict):
        return {k: _key_skeleton(v) for k, v in sorted(value.items())}
    if isinstance(value, list):
        # Represent a list by the skeleton of its first element (or "empty").
        if not value:
            return ["<empty>"]
        return [_key_skeleton(value[0])]
    return type(value).__name__


def _assert_or_update(name: str, skeleton: Any) -> None:
    """Compare *skeleton* against the committed golden, or rewrite it."""
    CONTRACTS_DIR.mkdir(parents=True, exist_ok=True)
    golden_path = CONTRACTS_DIR / name
    serialized = json.dumps(skeleton, indent=2, sort_keys=True) + "\n"

    if os.environ.get("COGNIS_UPDATE_CONTRACTS") == "1":
        golden_path.write_text(serialized, encoding="utf-8")
        return

    assert golden_path.exists(), (
        f"missing golden {golden_path.name}; regenerate with "
        f"COGNIS_UPDATE_CONTRACTS=1 pytest -m e2e -k contract_snapshots"
    )
    expected = golden_path.read_text(encoding="utf-8")
    assert serialized == expected, (
        f"contract drift in {name}: the real CLI/daemon output no longer matches "
        f"the committed golden. If this change is intentional, regenerate with "
        f"COGNIS_UPDATE_CONTRACTS=1 and update the matching TypeScript interface.\n"
        f"--- expected ---\n{expected}\n--- actual ---\n{serialized}"
    )


def test_paths_contract_snapshot(sample_repo: Path) -> None:
    """`cognis-cli paths` shape is pinned for the extension's WorkspacePaths type."""
    payload = run_cli_json(sample_repo, ["paths"])
    _assert_or_update("paths.json", _key_skeleton(payload))


def test_mcp_config_contract_snapshot(sample_repo: Path) -> None:
    """`cognis-cli mcp-config` shape is pinned for McpConfigPayload."""
    run_cli(sample_repo, ["init", "--quiet"])
    payload = run_cli_json(sample_repo, ["mcp-config", "--host", "cursor"])
    _assert_or_update("mcp_config.json", _key_skeleton(payload))


def test_health_contract_snapshot(sample_repo: Path) -> None:
    """`cognis-cli health --json` shape is pinned for HealthReport."""
    run_cli(sample_repo, ["init", "--quiet"])
    payload = run_cli_json(sample_repo, ["health", "--json"])
    # Health check keys vary by environment (vector/embedder availability), so
    # snapshot only the stable top-level shape + the per-check field shape.
    skeleton = {
        "overall": type(payload["overall"]).__name__,
        "runtime_version": type(payload["runtime_version"]).__name__,
        "checks": {"<check>": _key_skeleton(next(iter(payload["checks"].values())))},
    }
    _assert_or_update("health.json", skeleton)


def test_bootstrap_contract_snapshot(sample_repo: Path) -> None:
    """`cognis-cli bootstrap --json` shape is pinned for BootstrapPayload."""
    payload = run_cli_json(
        sample_repo, ["bootstrap", "--skip-embeddings", "--json", "."], timeout=240.0
    )
    # phases is a heterogeneous list; pin just the top-level keys + paths shape.
    skeleton = {
        "keys": sorted(payload.keys()),
        "paths": _key_skeleton(payload["paths"]),
    }
    _assert_or_update("bootstrap.json", skeleton)


def test_indexd_status_contract_snapshot(sample_repo: Path) -> None:
    """The live indexd status file shape is pinned for IndexStatusReport."""
    paths = run_cli_json(sample_repo, ["paths"])
    db_path = Path(paths["db_path"])
    status_path = Path(paths["indexd_status_path"])
    run_cli(sample_repo, ["init", "--quiet"])

    with IndexdProcess(sample_repo, db_path, status_path, full_rebuild=True) as daemon:
        snapshot = daemon.wait_for_phase("watching", timeout=90.0)

    _assert_or_update("indexd_status.json", _key_skeleton(snapshot))
