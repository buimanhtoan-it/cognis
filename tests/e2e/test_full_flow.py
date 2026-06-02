"""Full cross-app flow E2E: the real "Set Up for AI" path over process boundaries.

This reproduces, with real subprocesses, the sequence the VS Code extension
runs on a fresh-user "Set Up for AI" click:

    1. `cognis-cli paths`        → resolve workspace paths + entrypoints
    2. `cognis-cli init`         → materialize .cognis/
    3. `cognis-cli mcp-config`   → emit MCP client config
    4. `cognis-indexd --full-rebuild` → cold-index the repo, then watch
    5. `cognis-cli health`       → confirm the index is queryable
    6. `cognis-mcpd` (stdio)     → AI agent queries the indexed DB

Each step asserts the cross-app contract: the JSON shapes the extension reads,
the shared DB the daemon writes and the MCP server reads, and the live status
file the status bar polls. A drift in any app's output fails here even though
the per-app unit/integration tests still pass against their mocks.
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from tests.e2e.harness import (
    IndexdProcess,
    run_cli,
    run_cli_json,
)

pytestmark = pytest.mark.e2e


# Fields the extension's TypeScript interfaces depend on. If the CLI drops or
# renames any of these, the extension breaks — so we pin them here.
_PATHS_REQUIRED_KEYS = {
    "repo_root",
    "cognis_dir",
    "config_path",
    "db_path",
    "indexd_status_path",
    "audit_log_path",
    "capsule_cache_dir",
    "golden_set_path",
    "runtime_version",
    "commands",
}
_PATHS_COMMAND_KEYS = {
    "python",
    "cognis_cli",
    "cognis_mcpd",
    "cognis_indexd",
    "cognis_cli_module",
    "cognis_mcpd_module",
    "cognis_indexd_module",
}


def test_paths_contract_matches_extension_expectations(sample_repo: Path) -> None:
    """`cognis-cli paths` JSON carries every field the extension's WorkspacePaths type reads."""
    payload = run_cli_json(sample_repo, ["paths"])

    assert payload.keys() >= _PATHS_REQUIRED_KEYS, (
        f"paths payload missing keys: {_PATHS_REQUIRED_KEYS - payload.keys()}"
    )
    assert payload["commands"].keys() >= _PATHS_COMMAND_KEYS, (
        f"commands missing keys: {_PATHS_COMMAND_KEYS - payload['commands'].keys()}"
    )
    # The extension joins db_path/status_path under .cognis; assert they live there.
    cognis_dir = Path(payload["cognis_dir"])
    assert Path(payload["db_path"]).parent == cognis_dir
    assert Path(payload["indexd_status_path"]).parent == cognis_dir
    # Module names the extension hard-codes must be exactly these.
    assert payload["commands"]["cognis_cli_module"] == "cognis.cli.main"
    assert payload["commands"]["cognis_indexd_module"] == "cognis_indexd.main"
    assert payload["commands"]["cognis_mcpd_module"] == "cognis_mcpd.main"


def test_full_setup_flow_indexes_and_serves_mcp(sample_repo: Path) -> None:
    """The end-to-end fresh-user flow produces a queryable, MCP-served index."""
    # --- Step 1: paths -----------------------------------------------------
    paths = run_cli_json(sample_repo, ["paths"])
    db_path = Path(paths["db_path"])
    status_path = Path(paths["indexd_status_path"])

    # --- Step 2: init ------------------------------------------------------
    init = run_cli(sample_repo, ["init", "--quiet"])
    assert init.exit_code == 0, init.stderr
    assert Path(paths["config_path"]).exists(), "init must create config.yaml"

    # --- Step 3: mcp-config ------------------------------------------------
    mcp_cfg = run_cli_json(sample_repo, ["mcp-config", "--host", "cursor"])
    server_name = mcp_cfg["server_name"]
    # Contract: server name is cognis-<repo-slug> and matches the extension's
    # deriveMcpServerName for the same folder ("workspace").
    assert server_name == "cognis-workspace", server_name
    server_block = mcp_cfg["config"]["mcpServers"][server_name]
    assert server_block["env"]["COGNIS_DB_PATH"] == str(db_path), (
        "MCP server env must point at the same UCKG db the indexer writes"
    )

    # --- Step 4: indexd --full-rebuild (cold index, then watch) ------------
    with IndexdProcess(
        sample_repo,
        db_path,
        status_path,
        full_rebuild=True,
        env={"COGNIS_MCP_WARM_SEMANTIC_ON_STARTUP": "0"},
    ) as daemon:
        watching = daemon.wait_for_phase("watching", timeout=90.0)
        assert watching["active"] is True
        assert watching["progress_percent"] == 100.0
        assert isinstance(watching.get("pid"), int)

    # DB is now populated and the daemon has shut down cleanly.
    assert db_path.exists(), "indexd must create the UCKG database"

    # --- Step 5: health ----------------------------------------------------
    health = run_cli_json(sample_repo, ["health", "--json"])
    assert health["overall"] in {"ok", "warn"}, health
    assert health["checks"]["index"]["status"] == "ok", (
        f"index check should pass after cold index: {health['checks']['index']}"
    )

    # --- Step 6: MCP stdio round-trip --------------------------------------
    # Single-token query: symbol_search does a substring LIKE match, so a
    # multi-word query would never match a symbol name.
    hits = _call_mcp_tool(sample_repo, db_path, "symbol_search", {"query": "verify", "k": 8})
    names = {hit.get("name") for hit in hits}
    assert "verify" in names, (
        f"MCP symbol_search should surface the indexed 'verify' symbol, got {names}"
    )


def test_semantic_search_over_stdio_does_not_hang(sample_repo: Path) -> None:
    """Regression: semantic_search must return results, not time out.

    This reproduces the real "indexing doesn't work" symptom an AI agent hits:
    a fresh repo indexed WITH embeddings, then ``semantic_search`` called over
    real stdio with the default warm-on-startup setting. The bug was that the
    embedder (torch/sentence-transformers) loaded for the first time on a
    spawned worker thread inside the server, which hangs — so the tool timed
    out at the MCP deadline. The fix warms the semantic layer on the main thread
    before serving; this test fails (times out) if that regresses.

    Requires sentence-transformers; skipped on installs without it.
    """
    pytest.importorskip("sentence_transformers")

    paths = run_cli_json(sample_repo, ["paths"])
    db_path = Path(paths["db_path"])

    # Index WITH embeddings (the real default — not --skip-embeddings), so the
    # semantic index actually has vectors to search.
    run_cli(sample_repo, ["init", "--quiet"])
    indexed = run_cli(sample_repo, ["index", "--full", "."], timeout=300.0)
    assert indexed.exit_code == 0, indexed.stderr

    # Default real-user MCP settings: warm semantic on startup.
    hits = _call_mcp_tool(
        sample_repo,
        db_path,
        "semantic_search",
        {"query": "validate authentication token", "k": 5},
        warm_semantic=True,
        # Generous: covers the one-time main-thread model load + the call.
        tool_timeout=90.0,
    )
    # The query is semantically related to authenticate/verify; with a populated
    # vector index it must return at least one hit (and crucially: not hang).
    assert isinstance(hits, list), f"semantic_search should return a list, got {hits!r}"
    names = {hit.get("name") for hit in hits}
    assert names & {"authenticate", "verify"}, (
        f"semantic_search should surface auth-related symbols, got {names}"
    )


def test_status_file_is_consumable_by_extension_normalizer(sample_repo: Path) -> None:
    """The live status JSON carries every field the extension's normalizeIndexStatus reads."""
    paths = run_cli_json(sample_repo, ["paths"])
    db_path = Path(paths["db_path"])
    status_path = Path(paths["indexd_status_path"])
    run_cli(sample_repo, ["init", "--quiet"])

    # The extension's IndexStatusReport normalizer reads these snake_case keys.
    expected_keys = {
        "pid",
        "active",
        "phase",
        "message",
        "progress_percent",
        "pending_count",
        "pending_files",
        "inflight_count",
        "inflight_files",
        "recent_files",
        "updated_at",
    }

    with IndexdProcess(sample_repo, db_path, status_path, full_rebuild=True) as daemon:
        snapshot = daemon.wait_for_phase("watching", timeout=90.0)

    missing = expected_keys - snapshot.keys()
    assert not missing, f"status file missing keys the extension reads: {missing}"
    assert isinstance(snapshot["pending_files"], list)
    assert isinstance(snapshot["recent_files"], list)


def test_clear_reindex_flow_rebuilds_db(sample_repo: Path) -> None:
    """`cognis-cli index --clear` (the Clear & Re-index command) rebuilds synchronously."""
    run_cli(sample_repo, ["init", "--quiet"])
    paths = run_cli_json(sample_repo, ["paths"])
    db_path = Path(paths["db_path"])

    first = run_cli(sample_repo, ["index", "--clear", "."])
    assert first.exit_code == 0, first.stderr
    assert db_path.exists()

    # Re-running clear is idempotent and keeps the workspace healthy.
    second = run_cli(sample_repo, ["index", "--clear", "."])
    assert second.exit_code == 0, second.stderr

    health = run_cli_json(sample_repo, ["health", "--json"])
    assert health["checks"]["index"]["status"] == "ok", health["checks"]["index"]


# ---------------------------------------------------------------------------
# MCP stdio helper
# ---------------------------------------------------------------------------


def _call_mcp_tool(
    repo_root: Path,
    db_path: Path,
    tool: str,
    arguments: dict,
    *,
    warm_semantic: bool = False,
    tool_timeout: float = 30.0,
) -> list[dict]:
    """Spawn the real cognis-mcpd over stdio and call *tool*, like an AI host would.

    By default uses lexical tools so the round-trip exercises the full MCP stdio
    contract (handshake → tools/call → result) without a heavy embedder model.
    Set ``warm_semantic=True`` to run with the real warm-on-startup default for
    semantic tools (which loads the embedder on the server's main thread).

    A ``tool_timeout`` guards against a hung tool call so a regression surfaces
    as a clear failure instead of stalling the whole suite. Skips if fastmcp's
    client stack is unavailable.
    """
    import os
    import sys

    import anyio

    fastmcp = pytest.importorskip("fastmcp")
    Client = getattr(fastmcp, "Client", None)
    if Client is None:
        pytest.skip("fastmcp.Client unavailable")

    try:
        from fastmcp.client.transports import StdioTransport
    except Exception:  # pragma: no cover - older fastmcp layouts
        pytest.skip("fastmcp StdioTransport unavailable")

    env = dict(os.environ)
    env["COGNIS_DB_PATH"] = str(db_path)
    env["COGNIS_REPO_ROOT"] = str(repo_root)
    env["COGNIS_MCP_WARM_SEMANTIC_ON_STARTUP"] = "1" if warm_semantic else "0"

    transport = StdioTransport(
        command=sys.executable,
        args=["-m", "cognis_mcpd.main"],
        env=env,
    )

    async def _call() -> list[dict]:
        client = Client(transport)
        async with client:
            with anyio.fail_after(tool_timeout):
                result = await client.call_tool(tool, arguments)
        return _extract_hits(result)

    return anyio.run(_call)


def _extract_hits(result: object) -> list[dict]:
    """Normalize a fastmcp CallToolResult into a list of hit dicts."""
    # Newer fastmcp returns an object with `.data` / `.structured_content`;
    # fall back to parsing the text content block.
    data = getattr(result, "data", None)
    if isinstance(data, list):
        return [h for h in data if isinstance(h, dict)]
    structured = getattr(result, "structured_content", None)
    if isinstance(structured, dict):
        inner = structured.get("result", structured)
        if isinstance(inner, list):
            return [h for h in inner if isinstance(h, dict)]
    content = getattr(result, "content", None)
    if content:
        block = content[0]
        text = getattr(block, "text", None)
        if text:
            parsed = json.loads(text)
            if isinstance(parsed, list):
                return [h for h in parsed if isinstance(h, dict)]
    return []
