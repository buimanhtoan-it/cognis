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


# Fields that are environment-specific (present as an absolute path on some
# installs, ``null`` on others; console-script vs ``python -m`` shape) — pin
# their *presence* but not their concrete type so the golden is portable across
# Windows / Linux / macOS.
_NULLABLE_COMMAND_KEYS = ("cognis_cli", "cognis_mcpd", "cognis_indexd")
_PATH_OR_NULL = "<path-or-null>"


def _normalize_paths(payload: Any) -> Any:
    """Normalize a ``paths`` payload so its skeleton is platform-independent.

    ``commands.cognis_{cli,mcpd,indexd}`` are ``str | null`` by contract (the
    console script may or may not be on PATH), so a raw skeleton encodes
    ``str`` on Linux but ``NoneType`` on Windows. Pin them to a stable sentinel.
    """
    if not isinstance(payload, dict):
        return payload
    out = dict(payload)
    commands = out.get("commands")
    if isinstance(commands, dict):
        out["commands"] = {
            k: (_PATH_OR_NULL if k in _NULLABLE_COMMAND_KEYS else v) for k, v in commands.items()
        }
    return out


def _normalize_mcp_server_block(block: Any) -> Any:
    """Pin the stable MCP server-block contract the extension depends on.

    The block shape varies by environment: with the ``cognis-mcpd`` console
    script on PATH it is ``{command, env}``; otherwise ``{command, args, env}``.
    The passthrough ``env`` keys also vary. The extension only requires
    ``command`` plus ``env.COGNIS_DB_PATH`` (``args`` is optional), so pin that.
    """
    if not isinstance(block, dict):
        return block
    out: dict[str, Any] = {}
    if "command" in block:
        out["command"] = block["command"]
    env = block.get("env")
    if isinstance(env, dict):
        out["env"] = {"COGNIS_DB_PATH": env.get("COGNIS_DB_PATH", "")}
    return out


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
    _assert_or_update("paths.json", _key_skeleton(_normalize_paths(payload)))


def test_mcp_config_contract_snapshot(sample_repo: Path) -> None:
    """`cognis-cli mcp-config` shape is pinned for McpConfigPayload."""
    run_cli(sample_repo, ["init", "--quiet"])
    payload = run_cli_json(sample_repo, ["mcp-config", "--host", "cursor"])
    # The mcpServers key is ``cognis-<slug>-<hash>`` where the hash depends on
    # the (temp) repo path, so it varies per machine/run. Normalize it to a
    # stable placeholder before skeletonizing so the golden is portable.
    normalized = dict(payload)
    servers = payload.get("config", {}).get("mcpServers", {})
    if servers:
        first_block = _normalize_mcp_server_block(next(iter(servers.values())))
        normalized["config"] = {"mcpServers": {"<server>": first_block}}
    # The top-level ``env`` mirrors the server block env: it carries the always
    # present COGNIS_DB_PATH plus environment-specific timeout/passthrough keys
    # (HF_*, COGNIS_MCP_*, ...). Pin only the stable key so the golden is
    # portable across machines/CI.
    if isinstance(normalized.get("env"), dict):
        normalized["env"] = {"COGNIS_DB_PATH": normalized["env"].get("COGNIS_DB_PATH", "")}
    _assert_or_update("mcp_config.json", _key_skeleton(normalized))


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
        "paths": _key_skeleton(_normalize_paths(payload["paths"])),
    }
    _assert_or_update("bootstrap.json", skeleton)


def test_indexd_status_contract_snapshot(sample_repo: Path) -> None:
    """The live indexd status file shape is pinned for IndexStatusReport."""
    paths = run_cli_json(sample_repo, ["paths"])
    db_path = Path(paths["db_path"])
    status_path = Path(paths["indexd_status_path"])
    run_cli(sample_repo, ["init", "--quiet"])

    with IndexdProcess(sample_repo, db_path, status_path, full_rebuild=True) as daemon:
        snapshot = daemon.wait_for_phase("watching", timeout=180.0)

    # The file lists and last_error carry timing/environment-specific values
    # (which files were recently indexed at the sampled instant, whether a
    # transient error occurred). Pin their *shape* — list-of-strings, nullable
    # error — not the runtime contents, so the golden is portable.
    normalized = dict(snapshot)
    for key in ("pending_files", "inflight_files", "recent_files"):
        if key in normalized:
            normalized[key] = ["<file>"]
    if "last_error" in normalized:
        normalized["last_error"] = "<error-or-null>"
    _assert_or_update("indexd_status.json", _key_skeleton(normalized))
