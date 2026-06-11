"""E2E: cognis-mcpd HTTP transport speaks real MCP and serves real tools.

The panel-managed standalone server in the extension launches
``cognis-mcpd --transport http --port <p>``; this test exercises the same
wire path end-to-end:

  1. Cold-index a fixture repo with cognis-cli (so MCP has data).
  2. Spawn cognis-mcpd over HTTP on a free port.
  3. Wait for the bound URL to accept connections.
  4. Open a streamable-http MCP session, list tools, call one.
  5. Stop the daemon.

If cognis-cli (init/index) takes a different shape on a given platform we
skip rather than guess. The MCP client lib is part of the project's runtime
extras, so this needs no extra deps in CI.
"""

from __future__ import annotations

import asyncio
import os
import shutil
import socket
import subprocess
import sys
import time
from contextlib import closing
from pathlib import Path

import pytest

pytestmark = pytest.mark.e2e

REPO_ROOT = Path(__file__).resolve().parent.parent.parent
FIXTURE = REPO_ROOT / "tests" / "fixtures" / "repos" / "mini-py-svc"


def _free_port() -> int:
    """Reserve a free TCP port and immediately release it (kernel race-window)."""
    with closing(socket.socket(socket.AF_INET, socket.SOCK_STREAM)) as s:
        s.bind(("127.0.0.1", 0))
        return int(s.getsockname()[1])


def _wait_for_port(host: str, port: int, timeout: float = 30.0) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            with socket.create_connection((host, port), timeout=1.0):
                return
        except OSError:
            time.sleep(0.2)
    raise TimeoutError(f"cognis-mcpd never bound {host}:{port}")


def _run_cli(repo: Path, args: list[str]) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, "-m", "cognis.cli.main", *args],
        cwd=repo,
        capture_output=True,
        text=True,
        timeout=180,
        env={**os.environ, "PYTHONUTF8": "1", "PYTHONUNBUFFERED": "1"},
    )


def test_http_transport_lists_and_calls_tools(tmp_path: Path) -> None:
    pytest.importorskip("mcp")
    pytest.importorskip("fastmcp")

    repo = tmp_path / "mini-py-svc"
    shutil.copytree(FIXTURE, repo)
    init = _run_cli(repo, ["init", "--quiet"])
    if init.returncode != 0:
        pytest.skip(f"cognis-cli init unavailable: {init.stderr[-300:]}")
    idx = _run_cli(repo, ["index", "--skip-embeddings", "--quiet"])
    if idx.returncode != 0:
        pytest.skip(f"cognis-cli index unavailable: {idx.stderr[-300:]}")

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
            "COGNIS_MCP_WARM_SEMANTIC_ON_STARTUP": "0",
            "PYTHONUTF8": "1",
            "PYTHONUNBUFFERED": "1",
        },
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    try:
        _wait_for_port("127.0.0.1", port, timeout=60)

        async def _drive() -> tuple[list[str], int]:
            from mcp import ClientSession
            from mcp.client.streamable_http import streamable_http_client

            url = f"http://127.0.0.1:{port}/mcp"
            async with streamable_http_client(url) as (read, write, _):
                async with ClientSession(read, write) as session:
                    await session.initialize()
                    listed = await session.list_tools()
                    names = sorted(t.name for t in listed.tools)
                    res = await session.call_tool("discover_symbols", {"query": "create user"})
                    text = "".join(getattr(c, "text", "") for c in res.content)
                    return names, len(text)

        names, body_len = asyncio.run(_drive())

        assert "discover_symbols" in names, f"expected MCP tools, got {names}"
        assert body_len > 0, "discover_symbols over HTTP returned an empty body"
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(timeout=5)
