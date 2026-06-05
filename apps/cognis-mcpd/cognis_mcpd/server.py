"""FastMCP server setup and tool registrations for cognis-mcpd.

Implements task 15.1: Stand up FastMCP server with stdio transport.

The four MVP tools are registered as ``@mcp.tool()`` decorated functions that
delegate to :mod:`cognis_mcpd.tools`.  All error handling, audit logging,
and hard limits live in the tools module; server.py is a thin wiring layer.

Design reference: design.md §Components and Interfaces → MCP Server.
"""

from __future__ import annotations

import logging

logger = logging.getLogger(__name__)

try:
    from fastmcp import FastMCP

    _FASTMCP_AVAILABLE = True
except ImportError:
    _FASTMCP_AVAILABLE = False
    FastMCP = None  # type: ignore[assignment,misc]


def build_server() -> FastMCP | None:
    """Create and return the configured FastMCP server instance.

    Returns ``None`` if ``fastmcp`` is not installed, so that the module can
    still be imported (unit tests stub the tools without needing fastmcp).
    """
    if not _FASTMCP_AVAILABLE or FastMCP is None:
        logger.warning(
            "fastmcp is not installed; install it via `pip install 'cognis-engine[mcp]'`. "
            "The MCP server will not be available."
        )
        return None

    from cognis_mcpd.tools import (
        dependency_trace as _dependency_trace,
    )
    from cognis_mcpd.tools import (
        diffuse_context as _diffuse_context,
    )
    from cognis_mcpd.tools import (
        discover_symbols as _discover_symbols,
    )
    from cognis_mcpd.tools import (
        resolve_symbols as _resolve_symbols,
    )
    from cognis_mcpd.tools import (
        retrieve_context_capsule as _retrieve_context_capsule,
    )
    from cognis_mcpd.tools import (
        semantic_search as _semantic_search,
    )
    from cognis_mcpd.tools import (
        symbol_lookup as _symbol_lookup,
    )
    from cognis_mcpd.tools import (
        symbol_search as _symbol_search,
    )

    mcp = FastMCP("cognis")

    @mcp.tool()
    def diffuse_context(  # type: ignore[return]
        query: str,
        k: int = 10,
        alpha: float | None = None,
        eps: float | None = None,
        kind: str | None = None,
        path_prefix: str | None = None,
        exclude_path_prefixes: list[str] | None = None,
        file_path: str | None = None,
    ) -> list | dict:
        """Flagship retrieval: spreading-activation over the code graph (CSAR).

        Seeds from cheap lexical + semantic matches, then diffuses relevance
        across the code knowledge graph with Personalized PageRank. Recovers
        symbols on the call/flow path between matches that independent ranking
        misses — replacing separate discover_symbols + dependency_trace calls in
        one round trip. Prefer this for "understand/trace this flow" intents.

        Args:
            query: Natural-language intent or keywords.
            k: Maximum ranked results (clamped to 50, default 10).
            alpha: Restart probability in (0, 1]; lower spreads farther along
                code flow (structural), higher stays near seeds (semantic).
                Defaults to 0.15.
            eps: Forward-push residual threshold (smaller = more thorough).
                Defaults to 1e-5.
            kind: Optional kind filter applied to seeds.
            path_prefix: Optional file-path prefix filter applied to seeds.
            exclude_path_prefixes: Optional path prefixes to exclude from seeds.
            file_path: Alias for path_prefix.

        Returns:
            List of ranked hit dicts (each with on_path flag and match_sources),
            or {"error": {...}} on failure.
        """
        return _diffuse_context(
            query, k, alpha, eps, kind, path_prefix, exclude_path_prefixes, file_path
        )

    @mcp.tool()
    def symbol_lookup(name_or_id: str, kind: str | None = None) -> dict:  # type: ignore[return]
        """Resolve a single symbol by exact id, qualified_name, or fuzzy name.

        Use when you already know the symbol id or qualified name. For discovery
        or ambiguous names, prefer ``symbol_search`` which returns ranked results.

        Args:
            name_or_id: Symbol id, qualified_name, or partial name to search for.
            kind: Optional kind filter (e.g. "function", "class").

        Returns:
            Serialized SymbolNode dict, or {"error": {...}} if not found.
        """
        return _symbol_lookup(name_or_id, kind)

    @mcp.tool()
    def symbol_search(  # type: ignore[return]
        query: str,
        k: int = 8,
        kind: str | None = None,
        path_prefix: str | None = None,
        exclude_path_prefixes: list[str] | None = None,
        file_path: str | None = None,
    ) -> list | dict:
        """Discover symbols with ranked multi-result search (recommended for exploration).

        Returns multiple hits with id, qualified_name, kind, file location, score,
        and snippet so agents can pick a target without extra lookups.

        Args:
            query: Name fragment, qualified name, or symbol id.
            k: Maximum results (clamped to 50, default 8).
            kind: Optional kind filter (e.g. "function", "class").
            path_prefix: Optional file-path prefix filter.
            exclude_path_prefixes: Optional path prefixes to exclude.
            file_path: Alias for path_prefix.

        Returns:
            List of ranked hit dicts, or {"error": {...}} on failure.
        """
        return _symbol_search(query, k, kind, path_prefix, exclude_path_prefixes, file_path)

    @mcp.tool()
    def discover_symbols(  # type: ignore[return]
        query: str,
        k: int = 10,
        kind: str | None = None,
        path_prefix: str | None = None,
        exclude_path_prefixes: list[str] | None = None,
        file_path: str | None = None,
    ) -> list | dict:
        """Hybrid lexical + semantic discovery in one ranked shortlist.

        Preferred when intent is unclear: merges name/keyword matches with
        embedding similarity using reciprocal-rank fusion. Each hit includes file
        location, snippet, and match_sources so agents can choose without follow-up
        search calls.

        Args:
            query: Name fragment, keyword, or natural-language intent.
            k: Maximum fused results (clamped to 50, default 10).
            kind: Optional kind filter.
            path_prefix: Optional file-path prefix filter.
            exclude_path_prefixes: Optional path prefixes to exclude.
            file_path: Alias for path_prefix.

        Returns:
            List of fused hit dicts, or {"error": {...}} on failure.
        """
        return _discover_symbols(query, k, kind, path_prefix, exclude_path_prefixes, file_path)

    @mcp.tool()
    def semantic_search(  # type: ignore[return]
        query: str,
        k: int = 10,
        mode: str | None = None,
        kind: str | None = None,
        path_prefix: str | None = None,
        exclude_path_prefixes: list[str] | None = None,
        file_path: str | None = None,
    ) -> list | dict:
        """Semantic search with actionable symbol payloads.

        Returns file location, signature, docstring, and snippet alongside
        similarity score so agents can act without follow-up lookups.

        Args:
            query: Natural-language query string.
            k: Maximum number of results (clamped to 50).
            mode: Deprecated alias for kind.
            kind: Optional kind filter (e.g. "function").
            path_prefix: Optional file-path prefix filter.
            exclude_path_prefixes: Optional path prefixes to exclude.
            file_path: Alias for path_prefix.

        Returns:
            List of enriched hit dicts or {"error": {...}} on failure.
        """
        return _semantic_search(
            query,
            k,
            mode,
            kind,
            path_prefix,
            exclude_path_prefixes,
            file_path,
        )

    @mcp.tool()
    def resolve_symbols(symbol_ids: list[str], include_body: bool = True) -> dict:  # type: ignore[return]
        """Hydrate up to 50 symbols in one call.

        Use after discovery when you need full symbol records for several ids
        without repeated symbol_lookup round trips.

        Args:
            symbol_ids: Symbol ids to resolve (max 50, deduplicated).
            include_body: When False, omit body_excerpt to save tokens.

        Returns:
            {"symbols": [...], "missing": [...], "requested_count", "resolved_count"}
            or {"error": {...}} on failure.
        """
        return _resolve_symbols(symbol_ids, include_body)

    @mcp.tool()
    def dependency_trace(  # type: ignore[return]
        symbol_id: str, direction: str = "out", depth: int = 3
    ) -> dict:
        """Trace symbol dependencies via the call graph.

        Each hit includes qualified_name, kind, file_path, and line range when
        resolvable, so follow-up symbol lookups are usually unnecessary.

        Args:
            symbol_id: Starting symbol id.
            direction: "out" (callees), "in" (callers), or "both".
            depth: Traversal depth (clamped to 8).

        Returns:
            {"start": ..., "direction": ..., "depth": ..., "hits": [...]}
            or {"error": {...}} on failure.
        """
        return _dependency_trace(symbol_id, direction, depth)

    @mcp.tool()
    def retrieve_context_capsule(  # type: ignore[return]
        task: str, max_tokens: int = 8000, include_runtime: bool = False
    ) -> dict:
        """End-to-end: classify, plan, retrieve, and compose a Context Capsule.

        Args:
            task: User task / query string.
            max_tokens: Token budget for the capsule (clamped to 32000).
            include_runtime: If True, include runtime evidence (Phase 3).

        Returns:
            Serialized ContextCapsule dict, or {"error": {...}} on failure.
        """
        return _retrieve_context_capsule(task, max_tokens, include_runtime)

    return mcp


__all__ = ["build_server"]
