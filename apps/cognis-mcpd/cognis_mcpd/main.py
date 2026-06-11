"""Entry point for ``cognis-mcpd``.

Starts the FastMCP server with stdio transport (MVP). SSE transport is deferred
to Phase 2 per the design's resolved open questions.

The console script ``cognis-mcpd`` in pyproject.toml points here.
"""

from __future__ import annotations

import argparse
import logging
import os
import sys


def _warm_db_on_startup(logger: logging.Logger) -> None:
    """Open the UCKG ``Database`` on the main thread before serving.

    Why this matters: ``Database.__init__`` probes for the optional
    ``sqlite-vec`` extension, which lazily imports ``sqlite_vec`` → ``numpy``.
    If that import first happens inside a tool call — which FastMCP runs on an
    anyio *worker thread* — it can deadlock on the CPython import lock against
    the main serve loop (observed on Python 3.14 + Windows: the tool call hangs
    forever and the MCP client times out).

    Touching the DB here forces every heavy, import-locking dependency to load
    on the main thread up front, so the first ``symbol_lookup``/``symbol_search``
    over stdio responds immediately instead of hanging.

    Best-effort: a missing DB or probe failure must never stop the server from
    starting, so all errors are swallowed with a debug log.
    """
    try:
        import os

        from cognis.db import Database

        db_path = os.environ.get("COGNIS_DB_PATH", ".cognis/uckg.db")
        # Construction runs _probe_vec_support → imports sqlite_vec/numpy now.
        Database(db_path)
    except Exception:
        logger.debug("db warm-up skipped", exc_info=True)


def _warm_semantic_layer_on_startup(logger: logging.Logger) -> None:
    """Warm the shared semantic layer **on the main thread** before serving.

    This must run on the main thread, synchronously, for a critical reason:
    importing/initializing ``torch`` (via ``sentence-transformers``) for the
    first time on a non-main thread can hang indefinitely inside the MCP server
    process. FastMCP runs each tool on an anyio worker thread, and our tools
    further offload semantic work to a spawned daemon thread via
    ``_run_with_deadline``. If the embedder's first load happens there, torch's
    one-time global initialization deadlocks while the main thread sits in the
    asyncio/anyio serve loop — the tool call then blocks until the MCP deadline
    fires and ``semantic_search`` returns a TIMEOUT to the agent.

    Doing the first (and only) cold load here, on the main thread, means every
    subsequent tool call reuses the cached singleton and returns immediately.

    Disabled by ``COGNIS_MCP_WARM_SEMANTIC_ON_STARTUP=0`` for environments that
    never use semantic tools (lexical/structural retrieval still work), but note
    that disabling it re-exposes the off-main-thread first-load hang the moment
    a semantic tool is called. Best-effort: a load failure is logged and never
    blocks startup.
    """
    if os.environ.get("COGNIS_MCP_WARM_SEMANTIC_ON_STARTUP", "1").lower() in {
        "0",
        "false",
        "no",
    }:
        return

    try:
        from cognis_mcpd.embedder_pool import get_shared_semantic_layer

        get_shared_semantic_layer()
    except Exception:
        logger.debug("semantic warm-up skipped", exc_info=True)


def _parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    """Parse cognis-mcpd CLI args.

    Defaults preserve the original behaviour (stdio), so an editor's existing
    stdio ``mcp.json`` keeps working with no arguments. The HTTP transport is
    opt-in for the panel-managed, per-workspace server.
    """
    parser = argparse.ArgumentParser(prog="cognis-mcpd", description="Cognis MCP server.")
    parser.add_argument(
        "--transport",
        choices=["stdio", "http"],
        default=os.environ.get("COGNIS_MCP_TRANSPORT", "stdio"),
        help="stdio (editor-launched, default) or http (standalone server with a URL).",
    )
    parser.add_argument(
        "--host",
        default=os.environ.get("COGNIS_MCP_HOST", "127.0.0.1"),
        help="Host to bind for http transport. Localhost only by default for safety.",
    )
    parser.add_argument(
        "--port",
        type=int,
        default=int(os.environ.get("COGNIS_MCP_PORT", "0") or "0"),
        help="Port to bind for http transport (required when --transport http).",
    )
    return parser.parse_args(argv)


def _serve(mcp: object, args: argparse.Namespace, logger: logging.Logger) -> None:
    """Run the server on the selected transport.

    Security: http binds to ``127.0.0.1`` by default and we refuse to bind a
    non-loopback host unless ``COGNIS_MCP_ALLOW_REMOTE=1`` is set explicitly —
    the MCP server exposes code-search over the local index and must not be
    reachable off the machine by accident.
    """
    run = mcp.run  # type: ignore[attr-defined]
    if args.transport == "http":
        if args.port <= 0:
            raise SystemExit("cognis-mcpd: --port is required for --transport http")
        loopback = args.host in {"127.0.0.1", "::1", "localhost"}
        if not loopback and os.environ.get("COGNIS_MCP_ALLOW_REMOTE", "").lower() not in {
            "1",
            "true",
            "yes",
        }:
            raise SystemExit(
                f"cognis-mcpd: refusing to bind non-loopback host {args.host!r}. "
                "Set COGNIS_MCP_ALLOW_REMOTE=1 to override (not recommended)."
            )
        logger.info("serving MCP over http at http://%s:%d/mcp", args.host, args.port)
        run(transport="http", host=args.host, port=args.port)
    else:
        run(transport="stdio")


def main(argv: list[str] | None = None) -> int:
    """Start the cognis MCP server.

    Returns:
        Exit code (0 on clean exit, 1 on startup error).
    """
    args = _parse_args(argv)
    logging.basicConfig(
        level=logging.WARNING,
        format="%(asctime)s %(name)s %(levelname)s %(message)s",
        stream=sys.stderr,
    )
    from cognis.branding import echo_banner

    echo_banner(prog="cognis-mcpd")
    logger = logging.getLogger("cognis_mcpd")

    try:
        from cognis_mcpd.server import build_server

        mcp = build_server()
        if mcp is None:
            sys.stderr.write(
                "cognis-mcpd: fastmcp is not installed.\n"
                "Install it with: pip install 'cognis-engine[mcp]'\n"
            )
            return 1

        # Force import-locking, heavy deps (sqlite_vec/numpy via the DB probe)
        # to load on the main thread before serving. Otherwise the first tool
        # call triggers that import on an anyio worker thread and can deadlock
        # against the serve loop's import lock (Python 3.14 / Windows).
        _warm_db_on_startup(logger)
        _warm_semantic_layer_on_startup(logger)

        _serve(mcp, args, logger)
        return 0
    except KeyboardInterrupt:
        logger.info("cognis-mcpd received KeyboardInterrupt, shutting down.")
        return 0
    except Exception:
        logger.exception("cognis-mcpd startup failed")
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
