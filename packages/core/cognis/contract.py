"""Single source of truth for the extension ↔ backend contract version.

The VS Code extension and the Python backend (``cognis-cli``, ``cognis-indexd``,
``cognis-mcpd``) exchange JSON across process and language boundaries. Those
shapes are a cross-language contract: if one side renames or drops a field, or
the set of commands / MCP tools changes, the other side breaks — and because
the extension updates independently of the backend (marketplace vs PyPI), a
version-skewed install is the single most common production state and the one
the matched-version e2e suite never exercises.

``CONTRACT_VERSION`` is the explicit handshake token. Bump it whenever the shape
of the cross-process exchange changes in a way the other side must know about
(a renamed/removed field, a new required command, a changed MCP tool surface).
The extension ships the version it was built against, calls ``cognis-cli
handshake`` at startup, and degrades with a clear, actionable message instead of
failing silently when the versions disagree.
"""

from __future__ import annotations

from typing import Any, Final

from cognis import __version__

#: Bump on any breaking change to the extension ↔ backend JSON contract.
CONTRACT_VERSION: Final[int] = 1

#: CLI commands the extension relies on (see cli.ts / workspace.ts). The
#: handshake advertises these so the extension can detect a backend that lacks a
#: command it needs.
CLI_COMMANDS: Final[tuple[str, ...]] = (
    "init",
    "bootstrap",
    "health",
    "paths",
    "doctor",
    "mcp-config",
    "index",
    "handshake",
)

#: MCP tools the server exposes (see cognis_mcpd/server.py). The AI agent calls
#: these; pinned here so a dropped/renamed tool is caught by a contract test and
#: surfaced in the handshake capability list.
MCP_TOOLS: Final[tuple[str, ...]] = (
    "diffuse_context",
    "symbol_lookup",
    "symbol_search",
    "discover_symbols",
    "semantic_search",
    "resolve_symbols",
    "dependency_trace",
    "retrieve_context_capsule",
)


def handshake_payload() -> dict[str, Any]:
    """Build the ``cognis-cli handshake`` JSON the extension reads at startup."""
    return {
        "contract_version": CONTRACT_VERSION,
        "engine_version": __version__,
        "cli_commands": list(CLI_COMMANDS),
        "mcp_tools": list(MCP_TOOLS),
    }
