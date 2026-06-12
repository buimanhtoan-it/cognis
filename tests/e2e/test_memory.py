"""Real-backend resource-leak regression for cognis-mcpd.

Motivation: a long-running ``cognis-mcpd`` stays resident in a developer's
editor for hours. If each tool call leaks a resource (an unclosed sqlite
connection, a file handle, an unbounded cache) RAM and OS handles climb until
the editor is sluggish or the process is OOM-killed — a symptom unit tests with
a mocked backend can never see.

Both tests spin up the **real** server over a real process boundary and drive a
sustained stream of real MCP tool calls through one session, asserting the
process does not accumulate OS handles or grow RSS without bound:

- ``test_mcpd_lexical_load_is_resource_bound`` — model-free (fast, stable CI
  gate). Exercises the request/response + caching path under heavy load.
- ``test_mcpd_semantic_load_releases_worker_connections`` — drives the semantic
  path, where each call runs on a fresh worker thread that opens its own sqlite
  connection. This is the per-call connection leak ``_run_with_deadline`` must
  release; a near-linear handle climb here is the leak's fingerprint. Requires a
  local embedder, so it skips cleanly on a model-less CI runner.

The server's stdout/stderr go to DEVNULL: over hundreds of calls an undrained
pipe fills (~64 KB) and blocks the server mid-response, which would surface as a
spurious client read-timeout rather than a resource assertion.
"""

from __future__ import annotations

import asyncio
import os
import socket
import subprocess
import sys
import time
from contextlib import closing
from pathlib import Path

import pytest

from tests.e2e.harness import IndexdProcess, run_cli, write_sample_repo

pytestmark = pytest.mark.e2e

_MAX_HANDLE_GROWTH = 60
_MAX_RSS_GROWTH_MB = 80.0


def _free_port() -> int:
    with closing(socket.socket(socket.AF_INET, socket.SOCK_STREAM)) as s:
        s.bind(("127.0.0.1", 0))
        return int(s.getsockname()[1])


def _wait_for_port(host: str, port: int, timeout: float) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            with socket.create_connection((host, port), timeout=1.0):
                return
        except OSError:
            time.sleep(0.2)
    raise TimeoutError(f"cognis-mcpd never bound {host}:{port}")


def _handle_count(proc_info: object) -> int:
    """Open OS handles for a psutil.Process (Windows: handles, POSIX: fds)."""
    num_handles = getattr(proc_info, "num_handles", None)
    if callable(num_handles):
        return int(num_handles())
    num_fds = getattr(proc_info, "num_fds", None)
    if callable(num_fds):
        return int(num_fds())
    return -1


def _structured(result: object) -> object:
    """Extract a tool's return value (list or dict) from an MCP CallToolResult."""
    import json

    structured = getattr(result, "structuredContent", None)
    if isinstance(structured, dict):
        if set(structured.keys()) == {"result"}:
            return structured["result"]
        return structured
    content = getattr(result, "content", None)
    if content:
        text = getattr(content[0], "text", None)
        if text:
            return json.loads(text)
    return None


def _measure_under_load(
    repo: Path,
    db_path: Path,
    *,
    tool: str,
    arguments: dict,
    warm_semantic: bool,
    warmup_calls: int,
    load_calls: int,
    bind_timeout: float,
) -> dict[str, float]:
    """Spawn a real mcpd over HTTP, drive *load_calls* tool calls, sample resources."""
    import psutil
    from mcp import ClientSession
    from mcp.client.streamable_http import streamable_http_client

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
            "COGNIS_MCP_WARM_SEMANTIC_ON_STARTUP": "1" if warm_semantic else "0",
            "PYTHONUTF8": "1",
            "PYTHONUNBUFFERED": "1",
        },
        # DEVNULL: an undrained log pipe would fill and block the server.
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )

    samples: dict[str, float] = {}
    try:
        _wait_for_port("127.0.0.1", port, timeout=bind_timeout)
        ps = psutil.Process(proc.pid)
        url = f"http://127.0.0.1:{port}/mcp"

        async def _drive() -> None:
            async with streamable_http_client(url) as (read, write, _):
                async with ClientSession(read, write) as session:
                    await session.initialize()

                    for _ in range(warmup_calls):
                        await session.call_tool(tool, arguments)
                    await asyncio.sleep(0.5)
                    samples["handles_baseline"] = _handle_count(ps)
                    samples["rss_baseline_mb"] = ps.memory_info().rss / 1024 / 1024

                    for _ in range(load_calls):
                        await session.call_tool(tool, arguments)
                    await asyncio.sleep(0.5)
                    samples["handles_after"] = _handle_count(ps)
                    samples["rss_after_mb"] = ps.memory_info().rss / 1024 / 1024

        asyncio.run(_drive())
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(timeout=5)

    samples["load_calls"] = float(load_calls)
    return samples


def _assert_bounded(samples: dict[str, float], tool: str) -> None:
    handle_growth = samples["handles_after"] - samples["handles_baseline"]
    rss_growth = samples["rss_after_mb"] - samples["rss_baseline_mb"]
    detail = (
        f"over {samples['load_calls']:.0f} {tool} calls: "
        f"handles {samples['handles_baseline']:.0f} -> {samples['handles_after']:.0f} "
        f"(+{handle_growth:.0f}), "
        f"rss {samples['rss_baseline_mb']:.1f}MB -> {samples['rss_after_mb']:.1f}MB "
        f"(+{rss_growth:.1f}MB)"
    )
    print(f"[memory] {detail}")  # visible with -s for trend tracking
    assert handle_growth <= _MAX_HANDLE_GROWTH, (
        f"cognis-mcpd leaked OS handles under sustained load ({detail}). "
        f"A near-linear climb is the fingerprint of a per-call connection/file leak."
    )
    assert rss_growth <= _MAX_RSS_GROWTH_MB, (
        f"cognis-mcpd RSS grew unbounded under sustained load ({detail})."
    )


def test_mcpd_lexical_load_is_resource_bound(tmp_path: Path) -> None:
    """Real server under heavy lexical load stays resource-bounded (model-free)."""
    pytest.importorskip("psutil")
    pytest.importorskip("mcp")
    pytest.importorskip("fastmcp")

    repo = tmp_path / "workspace"
    repo.mkdir()
    write_sample_repo(repo)
    assert run_cli(repo, ["init", "--quiet"]).exit_code == 0
    indexed = run_cli(repo, ["index", "--skip-embeddings", "."], timeout=180.0)
    assert indexed.exit_code == 0, indexed.stderr
    db_path = Path(run_cli(repo, ["paths"]).json()["db_path"])

    samples = _measure_under_load(
        repo,
        db_path,
        tool="symbol_search",
        arguments={"query": "verify", "k": 8},
        warm_semantic=False,
        warmup_calls=20,
        load_calls=250,
        bind_timeout=60.0,
    )
    _assert_bounded(samples, "symbol_search")


def test_mcpd_semantic_load_releases_worker_connections(tmp_path: Path) -> None:
    """Real server under semantic load must not leak per-call worker connections.

    semantic_search runs each call on a fresh worker thread that opens its own
    sqlite connection; ``_run_with_deadline`` must release it. Without that, OS
    handles climb ~linearly with call count. Requires a local embedder.
    """
    pytest.importorskip("psutil")
    pytest.importorskip("mcp")
    pytest.importorskip("fastmcp")
    pytest.importorskip("sentence_transformers")

    repo = tmp_path / "workspace"
    repo.mkdir()
    write_sample_repo(repo)
    assert run_cli(repo, ["init", "--quiet"]).exit_code == 0
    # Index WITH embeddings so the semantic path has vectors to search.
    indexed = run_cli(repo, ["index", "--full", "."], timeout=300.0)
    assert indexed.exit_code == 0, indexed.stderr
    db_path = Path(run_cli(repo, ["paths"]).json()["db_path"])

    samples = _measure_under_load(
        repo,
        db_path,
        tool="semantic_search",
        arguments={"query": "validate authentication token", "k": 5},
        warm_semantic=True,
        warmup_calls=8,
        load_calls=120,
        bind_timeout=150.0,
    )
    _assert_bounded(samples, "semantic_search")


def test_every_mcp_tool_is_resource_bound(tmp_path: Path) -> None:
    """Every user-facing MCP tool, under repeated calls, stays resource-bounded.

    Coverage: the AI agent can invoke any of the 8 tools; a leak in *any* of them
    accumulates over a session. This drives all 8 against one real server
    (model-free: a skip-embeddings index keeps semantic legs empty so no model
    loads) and asserts per-tool OS-handle growth is flat. A tool that leaks a
    connection/handle per call shows a near-linear climb here.
    """
    psutil = pytest.importorskip("psutil")
    pytest.importorskip("mcp")
    pytest.importorskip("fastmcp")

    from mcp import ClientSession
    from mcp.client.streamable_http import streamable_http_client

    repo = tmp_path / "workspace"
    repo.mkdir()
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
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )

    per_tool: dict[str, int] = {}
    rss_start = 0.0
    rss_end = 0.0
    calls_each = 40
    try:
        _wait_for_port("127.0.0.1", port, timeout=60.0)
        ps = psutil.Process(proc.pid)
        url = f"http://127.0.0.1:{port}/mcp"

        async def _drive() -> None:
            nonlocal rss_start, rss_end
            async with streamable_http_client(url) as (read, write, _):
                async with ClientSession(read, write) as session:
                    await session.initialize()

                    # Resolve a real symbol id for the id-taking tools.
                    res = await session.call_tool("symbol_search", {"query": "verify", "k": 1})
                    hits = _structured(res)
                    symbol_id = hits[0]["symbol_id"] if hits else "x"

                    tools: dict[str, dict] = {
                        "symbol_search": {"query": "verify", "k": 8},
                        "symbol_lookup": {"name_or_id": "verify"},
                        "discover_symbols": {"query": "verify", "k": 10},
                        "semantic_search": {"query": "verify token", "k": 5},
                        "diffuse_context": {"query": "verify", "k": 10},
                        "resolve_symbols": {"symbol_ids": [symbol_id]},
                        "dependency_trace": {
                            "symbol_id": symbol_id,
                            "direction": "out",
                            "depth": 2,
                        },
                        "retrieve_context_capsule": {
                            "task": "how is authentication verified",
                            "max_tokens": 2000,
                        },
                    }

                    rss_start = ps.memory_info().rss / 1024 / 1024
                    for name, args in tools.items():
                        for _ in range(3):  # warmup
                            await session.call_tool(name, args)
                        await asyncio.sleep(0.2)
                        base = _handle_count(ps)
                        for _ in range(calls_each):
                            await session.call_tool(name, args)
                        await asyncio.sleep(0.2)
                        per_tool[name] = _handle_count(ps) - base
                    rss_end = ps.memory_info().rss / 1024 / 1024

        asyncio.run(_drive())
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(timeout=5)

    summary = ", ".join(f"{name}:+{delta}" for name, delta in per_tool.items())
    print(f"[memory] per-tool handle growth over {calls_each} calls each: {summary}")
    print(f"[memory] rss {rss_start:.1f}MB -> {rss_end:.1f}MB (+{rss_end - rss_start:.1f}MB)")
    leaky = {name: delta for name, delta in per_tool.items() if delta > 40}
    assert not leaky, (
        f"MCP tool(s) leaked OS handles under repeated calls: {leaky}. "
        f"A near-linear climb is the fingerprint of a per-call connection/file leak."
    )
    assert (rss_end - rss_start) <= _MAX_RSS_GROWTH_MB, (
        f"mcpd RSS grew unbounded across all tools: +{rss_end - rss_start:.1f}MB"
    )


def test_indexd_watcher_is_resource_bound(tmp_path: Path) -> None:
    """The long-running indexd watcher must not leak under repeated file edits.

    The watcher is the other process that stays resident while the user works;
    every file save triggers an incremental index cycle. This drives many edit
    cycles against the real daemon and asserts its RSS / OS handles stay bounded
    (a per-event leak would climb with edit count).
    """
    psutil = pytest.importorskip("psutil")

    repo = tmp_path / "workspace"
    repo.mkdir()
    write_sample_repo(repo)
    assert run_cli(repo, ["init", "--quiet"]).exit_code == 0
    paths = run_cli(repo, ["paths"]).json()
    db_path = Path(paths["db_path"])
    status_path = Path(paths["indexd_status_path"])
    edit_file = repo / "src" / "churn.py"

    samples: dict[str, float] = {}
    with IndexdProcess(
        repo,
        db_path,
        status_path,
        full_rebuild=True,
        env={"COGNIS_MCP_WARM_SEMANTIC_ON_STARTUP": "0"},
    ) as daemon:
        daemon.wait_for_phase("watching", timeout=180.0)
        assert daemon.proc is not None
        ps = psutil.Process(daemon.proc.pid)

        # Warm: a few edits so the first-touch allocations settle.
        for i in range(5):
            edit_file.write_text(f"def warm_{i}():\n    return {i}\n", encoding="utf-8")
            time.sleep(0.25)
        time.sleep(1.5)
        samples["handles_baseline"] = _handle_count(ps)
        samples["rss_baseline_mb"] = ps.memory_info().rss / 1024 / 1024

        for i in range(40):
            edit_file.write_text(
                f"def churn_{i}(x):\n    return verify(x) or {i}\n", encoding="utf-8"
            )
            time.sleep(0.2)
        time.sleep(2.0)
        samples["handles_after"] = _handle_count(ps)
        samples["rss_after_mb"] = ps.memory_info().rss / 1024 / 1024

    handle_growth = samples["handles_after"] - samples["handles_baseline"]
    rss_growth = samples["rss_after_mb"] - samples["rss_baseline_mb"]
    detail = (
        f"over 40 edit cycles: handles {samples['handles_baseline']:.0f} -> "
        f"{samples['handles_after']:.0f} (+{handle_growth:.0f}), "
        f"rss {samples['rss_baseline_mb']:.1f}MB -> {samples['rss_after_mb']:.1f}MB "
        f"(+{rss_growth:.1f}MB)"
    )
    print(f"[memory] indexd {detail}")
    assert handle_growth <= _MAX_HANDLE_GROWTH, (
        f"cognis-indexd leaked OS handles under repeated edits ({detail})."
    )
    assert rss_growth <= _MAX_RSS_GROWTH_MB, (
        f"cognis-indexd RSS grew unbounded under repeated edits ({detail})."
    )
