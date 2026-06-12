"""Pin the AI-facing MCP tool output contract — against the real server.

The cross-language *plumbing* contract (CLI/daemon JSON the extension parses) is
covered by ``test_contract_snapshots.py``. The other half — the JSON the MCP
tools return to the **AI agent** — had no guard at all, even though it is the
surface the product is sold on. A refactor that silently drops ``qualified_name``
or ``score`` from a search hit, or renames ``hits`` in a dependency trace, would
pass every unit test and only surface as degraded answers in a user's editor.

These tests drive the **real** ``cognis-mcpd`` over a real MCP session (one
shared HTTP server per module) and assert the stable keys each tool must return.
Driving the real server — rather than importing the tools in-process — keeps the
test faithful to what an AI host actually receives and avoids loading the
backend's native extensions into the pytest process (which leaks file handles
that the repo's ``filterwarnings = error`` guard turns into spurious failures).

Required-key assertions (not full golden snapshots) keep this robust to
optional/enrichment fields while still failing loudly on a dropped/renamed field
the agent depends on. The tool set is pinned in ``cognis.contract.MCP_TOOLS``.
"""

from __future__ import annotations

import asyncio
import json
import os
import re
import socket
import subprocess
import sys
import time
from collections.abc import Callable
from contextlib import closing
from pathlib import Path
from typing import Any

import pytest

from tests.e2e.harness import run_cli, run_cli_json, write_sample_repo

pytestmark = pytest.mark.e2e


# ---------------------------------------------------------------------------
# Real-server fixture + MCP call helper
# ---------------------------------------------------------------------------


def _free_port() -> int:
    with closing(socket.socket(socket.AF_INET, socket.SOCK_STREAM)) as s:
        s.bind(("127.0.0.1", 0))
        return int(s.getsockname()[1])


def _wait_for_port(host: str, port: int, timeout: float = 60.0) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            with socket.create_connection((host, port), timeout=1.0):
                return
        except OSError:
            time.sleep(0.2)
    raise TimeoutError(f"cognis-mcpd never bound {host}:{port}")


def _structured(result: object) -> Any:
    """Extract a tool's return value (list or dict) from an MCP CallToolResult."""
    structured = getattr(result, "structuredContent", None)
    if isinstance(structured, dict):
        # fastmcp wraps a non-dict return (e.g. a list) under a "result" key.
        if set(structured.keys()) == {"result"}:
            return structured["result"]
        return structured
    content = getattr(result, "content", None)
    if content:
        text = getattr(content[0], "text", None)
        if text:
            return json.loads(text)
    return None


@pytest.fixture(scope="module")
def mcp_call(tmp_path_factory: pytest.TempPathFactory) -> Callable[[str, dict], Any]:
    """Bootstrap+index a repo once, run one real mcpd over HTTP, return a caller."""
    pytest.importorskip("mcp")
    pytest.importorskip("fastmcp")

    repo = tmp_path_factory.mktemp("mcp_contract_repo")
    write_sample_repo(repo)
    assert run_cli(repo, ["init", "--quiet"]).exit_code == 0
    indexed = run_cli(repo, ["index", "--skip-embeddings", "."], timeout=180.0)
    assert indexed.exit_code == 0, indexed.stderr
    db_path = Path(run_cli(repo, ["paths"]).json()["db_path"])

    port = _free_port()
    proc = subprocess.Popen(
        [
            sys.executable,
            "-m",
            "cognis_mcpd.main",
            "--transport",
            "http",
            "--host",
            "127.0.0.1",
            "--port",
            str(port),
        ],
        cwd=repo,
        env={
            **os.environ,
            "COGNIS_DB_PATH": str(db_path),
            "COGNIS_REPO_ROOT": str(repo),
            "COGNIS_MCP_WARM_SEMANTIC_ON_STARTUP": "0",
            "PYTHONUTF8": "1",
            "PYTHONUNBUFFERED": "1",
        },
        # DEVNULL: an undrained log pipe would fill and block the server.
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    url = f"http://127.0.0.1:{port}/mcp"

    def call(tool: str, arguments: dict) -> Any:
        from mcp import ClientSession
        from mcp.client.streamable_http import streamable_http_client

        async def go() -> Any:
            async with streamable_http_client(url) as (read, write, _):
                async with ClientSession(read, write) as session:
                    await session.initialize()
                    result = await session.call_tool(tool, arguments)
                    return _structured(result)

        return asyncio.run(go())

    call.url = url  # type: ignore[attr-defined]  # let the tool-list test reuse it

    try:
        _wait_for_port("127.0.0.1", port, timeout=60.0)
        yield call
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(timeout=5)


def _require_keys(obj: Any, keys: list[str], label: str) -> None:
    assert isinstance(obj, dict), f"{label}: expected a dict, got {type(obj).__name__}"
    missing = [k for k in keys if k not in obj]
    assert not missing, (
        f"{label}: MCP tool output is missing keys the AI agent relies on: {missing}. "
        f"The tool output contract drifted."
    )


# ---------------------------------------------------------------------------
# Tool output contracts (real server)
# ---------------------------------------------------------------------------


def test_server_advertises_the_pinned_tool_set(mcp_call: Callable[[str, dict], Any]) -> None:
    """The live server exposes exactly the tools the contract advertises."""
    from cognis.contract import MCP_TOOLS
    from mcp import ClientSession
    from mcp.client.streamable_http import streamable_http_client

    url = mcp_call.url  # type: ignore[attr-defined]

    async def go() -> set[str]:
        async with streamable_http_client(url) as (read, write, _):
            async with ClientSession(read, write) as session:
                await session.initialize()
                listed = await session.list_tools()
                return {t.name for t in listed.tools}

    names = asyncio.run(go())
    missing = [t for t in MCP_TOOLS if t not in names]
    assert not missing, (
        f"server is missing pinned tools {missing}; advertised={sorted(names)}. "
        f"cognis.contract.MCP_TOOLS drifted from the server."
    )


def test_discover_symbols_hit_contract(mcp_call: Callable[[str, dict], Any]) -> None:
    """A discover_symbols (hybrid) hit carries identity, location, score, sources."""
    results = mcp_call("discover_symbols", {"query": "verify", "k": 10})
    assert isinstance(results, list), f"discover_symbols returned {results!r}, expected a list"
    assert results, "discover_symbols found no hits for a symbol in the sample repo"
    _require_keys(
        results[0],
        [
            "symbol_id",
            "id",
            "name",
            "qualified_name",
            "kind",
            "file_path",
            "score",
            "match_reason",
            "match_sources",
            "snippet",
        ],
        "discover_symbols hit",
    )


def test_diffuse_context_hit_contract(mcp_call: Callable[[str, dict], Any]) -> None:
    """A diffuse_context (flagship CSAR) hit carries on_path + ppr_score + sources."""
    results = mcp_call("diffuse_context", {"query": "verify", "k": 10})
    assert isinstance(results, list), f"diffuse_context returned {results!r}, expected a list"
    assert results, "diffuse_context found no hits for a symbol in the sample repo"
    _require_keys(
        results[0],
        [
            "symbol_id",
            "id",
            "name",
            "qualified_name",
            "kind",
            "file_path",
            "score",
            "match_sources",
            "on_path",
            "ppr_score",
        ],
        "diffuse_context hit",
    )


def test_retrieve_context_capsule_contract(mcp_call: Callable[[str, dict], Any]) -> None:
    """retrieve_context_capsule returns the composed Context Capsule schema."""
    result = mcp_call(
        "retrieve_context_capsule",
        {"task": "how is authentication verified", "max_tokens": 2000},
    )
    _require_keys(
        result,
        [
            "goal",
            "task_mode",
            "confidence",
            "relevant_symbols",
            "compressed_context",
            "sources",
            "token_estimate",
            "version",
        ],
        "retrieve_context_capsule result",
    )


def test_symbol_search_hit_contract(mcp_call: Callable[[str, dict], Any]) -> None:
    """A symbol_search hit carries the identity + location + score fields agents use."""
    results = mcp_call("symbol_search", {"query": "authenticate", "k": 5})
    assert isinstance(results, list), f"symbol_search returned {results!r}, expected a list"
    assert results, "symbol_search found no hits for a symbol that exists in the sample repo"
    _require_keys(
        results[0],
        [
            "symbol_id",
            "id",
            "name",
            "qualified_name",
            "kind",
            "file_path",
            "line_start",
            "line_end",
            "score",
            "match_reason",
        ],
        "symbol_search hit",
    )


def test_symbol_lookup_contract(mcp_call: Callable[[str, dict], Any]) -> None:
    """symbol_lookup returns a full serialized symbol record."""
    result = mcp_call("symbol_lookup", {"name_or_id": "authenticate"})
    _require_keys(
        result,
        [
            "id",
            "kind",
            "name",
            "qualified_name",
            "language",
            "file_path",
            "line_start",
            "line_end",
        ],
        "symbol_lookup result",
    )


def test_dependency_trace_contract(mcp_call: Callable[[str, dict], Any]) -> None:
    """dependency_trace returns the start/direction/depth envelope + hits list."""
    hits = mcp_call("symbol_search", {"query": "authenticate", "k": 1})
    symbol_id = hits[0]["symbol_id"]
    result = mcp_call("dependency_trace", {"symbol_id": symbol_id, "direction": "out", "depth": 2})
    _require_keys(result, ["start", "direction", "depth", "hits"], "dependency_trace result")
    assert isinstance(result["hits"], list), "dependency_trace.hits must be a list"


def test_resolve_symbols_contract(mcp_call: Callable[[str, dict], Any]) -> None:
    """resolve_symbols hydrates ids and reports requested/resolved counts."""
    hits = mcp_call("symbol_search", {"query": "authenticate", "k": 1})
    symbol_id = hits[0]["symbol_id"]
    result = mcp_call("resolve_symbols", {"symbol_ids": [symbol_id]})
    _require_keys(
        result,
        ["symbols", "missing", "requested_count", "resolved_count"],
        "resolve_symbols result",
    )
    assert isinstance(result["symbols"], list), "resolve_symbols.symbols must be a list"


def test_error_envelope_contract(mcp_call: Callable[[str, dict], Any]) -> None:
    """A not-found lookup returns the {error:{code,message,retryable}} envelope."""
    result = mcp_call("symbol_lookup", {"name_or_id": "no_such_symbol_xyz_123"})
    _require_keys(result, ["error"], "error envelope")
    _require_keys(result["error"], ["code", "message", "retryable"], "error envelope body")


# ---------------------------------------------------------------------------
# Contract version lockstep + handshake (CLI subprocess — no in-process import)
# ---------------------------------------------------------------------------


def test_contract_version_is_in_lockstep_across_languages() -> None:
    """The backend and extension must agree on the contract version.

    The version-skew guard for our *own* releases: if someone bumps the Python
    ``CONTRACT_VERSION`` (a breaking shape change) without bumping the
    extension's ``EXPECTED_CONTRACT_VERSION`` — or vice versa — every matched
    e2e run would still pass while a real install would warn/degrade. Pinning
    them together here fails the build the moment they drift.
    """
    from cognis.contract import CONTRACT_VERSION

    repo_root = Path(__file__).resolve().parents[2]
    contract_ts = repo_root / "apps" / "cognis-vscode" / "src" / "contract.ts"
    text = contract_ts.read_text(encoding="utf-8")
    match = re.search(r"EXPECTED_CONTRACT_VERSION\s*=\s*(\d+)", text)
    assert match, "could not find EXPECTED_CONTRACT_VERSION in contract.ts"
    ts_version = int(match.group(1))
    assert ts_version == CONTRACT_VERSION, (
        f"contract version skew: backend CONTRACT_VERSION={CONTRACT_VERSION} but the "
        f"extension's EXPECTED_CONTRACT_VERSION={ts_version}. Bump both together when the "
        f"cross-process JSON contract changes."
    )


def test_handshake_command_emits_the_contract(tmp_path: Path) -> None:
    """`cognis-cli handshake` returns the negotiation payload the extension reads."""
    repo = tmp_path / "ws"
    repo.mkdir()
    payload = run_cli_json(repo, ["handshake"])
    _require_keys(
        payload,
        ["contract_version", "engine_version", "cli_commands", "mcp_tools"],
        "handshake payload",
    )
    from cognis.contract import CONTRACT_VERSION, MCP_TOOLS

    assert payload["contract_version"] == CONTRACT_VERSION
    assert set(MCP_TOOLS).issubset(set(payload["mcp_tools"]))
