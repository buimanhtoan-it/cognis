"""Entry point for ``cognis-mcpd``.

Starts the FastMCP server with stdio transport (MVP). SSE transport is deferred
to Phase 2 per the design's resolved open questions.

The console script ``cognis-mcpd`` in pyproject.toml points here.
"""

from __future__ import annotations

import logging
import os
import sys
import threading


def _warm_semantic_layer_on_startup(logger: logging.Logger) -> None:
    """Best-effort background warm-up for the shared semantic layer.

    Loading sentence-transformers can take several seconds on the first request,
    especially on Windows. Warm in the background so the MCP server can accept
    stdio traffic immediately while the model is loading.
    """
    if os.environ.get("COGNIS_MCP_WARM_SEMANTIC_ON_STARTUP", "1").lower() in {
        "0",
        "false",
        "no",
    }:
        return

    def _warm() -> None:
        try:
            from cognis_mcpd.embedder_pool import get_shared_semantic_layer

            get_shared_semantic_layer()
        except Exception:
            logger.debug("semantic warm-up skipped", exc_info=True)

    thread = threading.Thread(
        target=_warm,
        name="cognis-mcpd-semantic-warmup",
        daemon=True,
    )
    thread.start()


def main() -> int:
    """Start the cognis MCP server with stdio transport.

    Returns:
        Exit code (0 on clean exit, 1 on startup error).
    """
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
                "Install it with: pip install 'cognis[mcp]'\n"
            )
            return 1

        _warm_semantic_layer_on_startup(logger)

        # Run with stdio transport (MVP).
        mcp.run(transport="stdio")
        return 0
    except KeyboardInterrupt:
        logger.info("cognis-mcpd received KeyboardInterrupt, shutting down.")
        return 0
    except Exception:
        logger.exception("cognis-mcpd startup failed")
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
