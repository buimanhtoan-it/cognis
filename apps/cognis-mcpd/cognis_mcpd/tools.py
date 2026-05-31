"""MCP tool implementations for the cognis server.

Each function:
- Validates inputs and applies hard limits (CP-10, design §Error Handling).
- Calls into the retrieval / planner / capsule layers.
- Returns a typed result dict or a standard error envelope (never raises).
- Appends an audit log entry (args_hash only, never raw args).
- Enforces a 10s hard wall time via ``signal`` on POSIX (best-effort on Windows).

Tool signatures (from design.md §MCP Server):

    symbol_lookup(name_or_id, kind=None)  -> dict
    symbol_search(query, k=8, kind=None, path_prefix=None,
                  exclude_path_prefixes=None) -> list[dict]
    semantic_search(query, k=10, mode=None, kind=None, path_prefix=None,
                    exclude_path_prefixes=None) -> list[dict]
    discover_symbols(query, k=10, ...) -> list[dict]   # hybrid lexical + semantic
    diffuse_context(query, k=10, alpha=None, eps=None, ...) -> list[dict]  # CSAR (flagship)
    resolve_symbols(symbol_ids, include_body=True) -> dict
    dependency_trace(symbol_id, direction="out", depth=3) -> dict
    retrieve_context_capsule(task, max_tokens=8000, include_runtime=False) -> dict

Hard limits (design §Error Handling → Hard limits):
    dependency_trace depth  ≤ 8
    semantic_search k       ≤ 50
    symbol_search k       ≤ 50
    discover_symbols k      ≤ 50
    diffuse_context k       ≤ 50
    resolve_symbols ids     ≤ 50
    retrieve_context_capsule max_tokens ≤ 32000
    Per-tool wall time: 5s soft / 10s hard kill
"""

from __future__ import annotations

import logging
import os
import queue
import threading
import time
from collections.abc import Callable
from functools import lru_cache
from pathlib import Path
from typing import Any, TypeVar, cast

from cognis.db import Database, get_symbol
from cognis.planner import Planner

from cognis_mcpd.audit import audit_log_entry
from cognis_mcpd.errors import (
    EMBEDDER_UNAVAILABLE,
    INDEX_NOT_READY,
    INTERNAL_ERROR,
    INVALID_ARGUMENT,
    SYMBOL_NOT_FOUND,
    TIMEOUT,
    McpError,
    error_envelope,
)
from cognis_mcpd.metrics import METRICS
from cognis_mcpd.result_cache import cache_get, cache_set

logger = logging.getLogger(__name__)

# ---------------------------------------------------------------------------
# Hard limits (design §Error Handling → Hard limits)
# ---------------------------------------------------------------------------

_MAX_DEPTH: int = 8
_MAX_K: int = 50
_MAX_SYMBOL_SEARCH_K: int = 50
_MAX_RESOLVE_IDS: int = 50
_MAX_TOKENS: int = 32_000
_RRF_K: int = 60
_SOFT_TIMEOUT_S: float = float(os.environ.get("COGNIS_MCP_SOFT_TIMEOUT_S", "5.0"))
_HARD_TIMEOUT_S: float = float(os.environ.get("COGNIS_MCP_HARD_TIMEOUT_S", "10.0"))
_DISCOVER_SEMANTIC_TIMEOUT_S: float = float(
    os.environ.get("COGNIS_MCP_DISCOVER_SEMANTIC_TIMEOUT_S", "8.0")
)
_SEMANTIC_COOLDOWN_S: float = float(os.environ.get("COGNIS_MCP_SEMANTIC_COOLDOWN_S", "15.0"))
# CSAR diffusion defaults (see docs/csar.md). ``alpha`` interpolates semantic
# (->1) and structural (->0); ``eps`` bounds forward-push work by 1/(alpha*eps).
_CSAR_DEFAULT_ALPHA: float = float(os.environ.get("COGNIS_MCP_CSAR_ALPHA", "0.15"))
_CSAR_DEFAULT_EPS: float = float(os.environ.get("COGNIS_MCP_CSAR_EPS", "1e-5"))
_CSAR_SEED_K: int = int(os.environ.get("COGNIS_MCP_CSAR_SEED_K", "25"))
_DB_CACHE_LOCK = threading.Lock()
_DB_CACHE: dict[str, Database] = {}
_SEMANTIC_STAGE_LOCK = threading.Lock()
_SEMANTIC_STATE_LOCK = threading.Lock()
_SEMANTIC_DISABLED_UNTIL: float = 0.0
_T = TypeVar("_T")

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _get_db() -> Database:
    """Return a cached ``Database`` for the current ``COGNIS_DB_PATH``."""
    db_path = os.path.abspath(os.environ.get("COGNIS_DB_PATH", ".cognis/uckg.db"))
    cached = _DB_CACHE.get(db_path)
    if cached is not None:
        return cached

    with _DB_CACHE_LOCK:
        cached = _DB_CACHE.get(db_path)
        if cached is not None:
            return cached
        cached = Database(db_path)
        _DB_CACHE[db_path] = cached
        return cached


def _get_audit_path() -> Path:
    """Return the audit log path from env var or beside the active UCKG."""
    override = os.environ.get("COGNIS_AUDIT_LOG")
    if override:
        return Path(override)
    db_path = Path(os.environ.get("COGNIS_DB_PATH", ".cognis/uckg.db"))
    candidate = db_path.resolve().parent / "audit.log"
    if candidate.parent.name == ".cognis":
        return candidate
    return Path(".cognis/audit.log")


def _symbol_to_dict(sym: Any) -> dict[str, Any]:
    """Serialize a SymbolNode to a plain dict for MCP tool output."""
    return {
        "id": sym.id,
        "kind": sym.kind,
        "name": sym.name,
        "qualified_name": sym.qualified_name,
        "language": sym.language,
        "module": sym.module,
        "file_path": sym.file_path,
        "line_start": sym.line_start,
        "line_end": sym.line_end,
        "signature": sym.signature,
        "docstring": sym.docstring,
        "content_hash": sym.content_hash,
        "body_excerpt": sym.body_excerpt,
        "risk_score": sym.risk_score,
    }


def _row_to_symbol_dict(row: Any, *, include_body: bool = True) -> dict[str, Any]:
    """Serialize a symbol table row to MCP output."""
    payload = {
        "id": str(row["id"]),
        "kind": str(row["kind"]),
        "name": str(row["name"]),
        "qualified_name": str(row["qualified_name"]),
        "language": str(row["language"]),
        "module": str(row["module"]),
        "file_path": str(row["file_path"]),
        "line_start": row["line_start"],
        "line_end": row["line_end"],
        "signature": row["signature"],
        "docstring": row["docstring"],
        "content_hash": row["content_hash"],
        "body_excerpt": row["body_excerpt"],
        "risk_score": row["risk_score"],
    }
    if not include_body:
        payload.pop("body_excerpt", None)
    return payload


def _effective_path_prefix(path_prefix: str | None, file_path: str | None) -> str | None:
    """Resolve path filter aliases (``file_path`` is a synonym for ``path_prefix``)."""
    if path_prefix is not None:
        return path_prefix
    return file_path


def _repo_root_for_filters() -> str | None:
    """Best-effort repository root used for config and ignore-based filtering."""
    raw_root = os.environ.get("COGNIS_REPO_ROOT")
    if raw_root:
        return os.path.abspath(raw_root)

    db_path = os.path.abspath(os.environ.get("COGNIS_DB_PATH", ".cognis/uckg.db"))
    candidate = Path(db_path).parent
    if candidate.name == ".cognis":
        candidate = candidate.parent
    if candidate.exists():
        return str(candidate)
    return None


@lru_cache(maxsize=8)
def _default_ignore_filter(repo_root: str | None) -> Any:
    """Load repo-aware ignore patterns for retrieval-time path filtering."""
    if repo_root is None:
        return None

    try:
        from cognis.config import Config
        from cognis_indexer.watcher.gitignore import GitignoreFilter

        cfg = Config.load(repo_root)
        return GitignoreFilter.from_repo(repo_root, extra_patterns=list(cfg.repo.ignore))
    except Exception:
        logger.debug("Default ignore filter unavailable", exc_info=True)
        return None


def _matches_path_filters(
    file_path_value: str,
    path_prefix: str | None,
    exclude_path_prefixes: list[str] | None,
) -> bool:
    normalized_path = file_path_value.replace("\\", "/")
    normalized_prefix = path_prefix.replace("\\", "/") if path_prefix is not None else None
    default_filter = _default_ignore_filter(_repo_root_for_filters())
    if default_filter is not None:
        try:
            if default_filter.is_ignored(normalized_path):
                return False
        except Exception:
            logger.debug("Default ignore filter check failed", exc_info=True)
    if normalized_prefix is not None and not normalized_path.startswith(normalized_prefix):
        return False
    for prefix in exclude_path_prefixes or []:
        normalized_exclude = prefix.replace("\\", "/")
        if normalized_exclude and normalized_path.startswith(normalized_exclude):
            return False
    return True


def _symbol_row_to_search_hit(
    row: Any,
    *,
    score: float,
    match_reason: str,
    match_sources: list[str] | None = None,
    lexical_score: float | None = None,
    semantic_score: float | None = None,
) -> dict[str, Any]:
    """Build a compact symbol-search hit from a DB row."""
    body_excerpt = row["body_excerpt"]
    symbol_id = str(row["id"])
    hit: dict[str, Any] = {
        "symbol_id": symbol_id,
        "id": symbol_id,
        "name": str(row["name"]),
        "qualified_name": str(row["qualified_name"]),
        "kind": str(row["kind"]),
        "file_path": str(row["file_path"]),
        "line_start": row["line_start"],
        "line_end": row["line_end"],
        "score": score,
        "match_reason": match_reason,
        "snippet": body_excerpt,
        "body_excerpt": body_excerpt,
    }
    row_keys = row.keys()
    signature = row["signature"] if "signature" in row_keys else None
    docstring = row["docstring"] if "docstring" in row_keys else None
    if signature:
        hit["signature"] = str(signature)
    if docstring:
        hit["docstring"] = str(docstring)
    if match_sources:
        hit["match_sources"] = match_sources
    if lexical_score is not None:
        hit["lexical_score"] = lexical_score
    if semantic_score is not None:
        hit["semantic_score"] = semantic_score
    return hit


def _batch_fetch_symbol_rows(db: Database, symbol_ids: list[str]) -> dict[str, Any]:
    """Fetch symbol rows for *symbol_ids* in one query."""
    if not symbol_ids:
        return {}
    conn = db.connect()
    placeholders = ", ".join("?" * len(symbol_ids))
    rows = conn.execute(
        "SELECT id, kind, name, qualified_name, language, module, file_path, "
        "line_start, line_end, signature, docstring, content_hash, body_excerpt, "
        f"risk_score FROM symbol WHERE id IN ({placeholders})",
        symbol_ids,
    ).fetchall()
    return {str(row["id"]): row for row in rows}


def _filter_retrieval_hits(db: Database, hits: list[Any]) -> list[Any]:
    """Drop retrieval hits whose symbol paths are excluded by repo filters."""
    if not hits:
        return hits

    rows_by_id = _batch_fetch_symbol_rows(db, [str(hit.symbol_id) for hit in hits])
    filtered: list[Any] = []
    for hit in hits:
        row = rows_by_id.get(str(hit.symbol_id))
        if row is None:
            filtered.append(hit)
            continue
        if _matches_path_filters(str(row["file_path"]), None, None):
            filtered.append(hit)
    return filtered


def _discover_query_variants(query: str, *, max_variants: int = 8) -> list[str]:
    """Expand natural-language discovery queries into lexical sub-queries."""
    base = query.strip()
    if not base:
        return []

    variants = [base]
    seen = {base.lower()}

    try:
        from cognis_retrieval.query_rewriter import rewrite_query

        rewritten = rewrite_query(query)
    except Exception:
        logger.debug("Query rewriting unavailable for discover_symbols", exc_info=True)
        return variants

    for token in rewritten.split(" OR "):
        normalized = token.strip()
        if len(normalized) < 3:
            continue
        lowered = normalized.lower()
        if lowered in seen:
            continue
        seen.add(lowered)
        variants.append(normalized)
        if len(variants) >= max_variants:
            break
    return variants


def _fts_search_core(
    query: str,
    k: int,
    *,
    kind: str | None = None,
    path_prefix: str | None = None,
    exclude_path_prefixes: list[str] | None = None,
) -> list[dict[str, Any]]:
    """FTS-backed lexical retrieval for discovery queries."""
    from cognis_retrieval.lexical import LexicalLayer

    db = _get_db()
    hits = LexicalLayer().search(query, k, db)
    if not hits:
        return []

    rows_by_id = _batch_fetch_symbol_rows(db, [str(hit.symbol_id) for hit in hits])
    results: list[dict[str, Any]] = []
    for hit in hits:
        row = rows_by_id.get(str(hit.symbol_id))
        if row is None:
            continue

        file_path_value = str(row["file_path"])
        if kind is not None and str(row["kind"]) != kind:
            continue
        if not _matches_path_filters(file_path_value, path_prefix, exclude_path_prefixes):
            continue

        result = _symbol_row_to_search_hit(
            row,
            score=hit.score,
            match_reason="fts_bm25",
            match_sources=["lexical"],
            lexical_score=hit.score,
        )
        snippet = hit.evidence.get("snippet") if isinstance(hit.evidence, dict) else None
        if snippet:
            result["snippet"] = snippet
        results.append(result)
        if len(results) >= k:
            break
    return results


def _cacheable_result(result: Any) -> bool:
    """Return True when *result* is safe to store in the short-lived cache."""
    if isinstance(result, list):
        return True
    return isinstance(result, dict) and "error" not in result


def _rrf_fuse(
    ranked_lists: list[list[tuple[str, float, str]]],
    *,
    k: int,
) -> list[tuple[str, float, list[str], dict[str, float]]]:
    """Reciprocal-rank fuse multiple ranked symbol-id lists.

    Each inner list item is ``(symbol_id, raw_score, source_name)``.
    """
    fused: dict[str, dict[str, Any]] = {}
    for source_list in ranked_lists:
        for rank, (symbol_id, raw_score, source) in enumerate(source_list, start=1):
            entry = fused.setdefault(
                symbol_id,
                {"score": 0.0, "sources": set(), "raw_scores": {}},
            )
            entry["score"] += 1.0 / (_RRF_K + rank)
            entry["sources"].add(source)
            entry["raw_scores"][source] = raw_score

    ordered = sorted(
        fused.items(),
        key=lambda item: (-item[1]["score"], item[0]),
    )
    return [
        (
            symbol_id,
            float(meta["score"]),
            sorted(meta["sources"]),
            dict(meta["raw_scores"]),
        )
        for symbol_id, meta in ordered[:k]
    ]


def _score_symbol_match(query: str, row: Any) -> tuple[float, str]:
    """Rank a symbol row against *query* (higher score = better match)."""
    q = query.strip()
    q_lower = q.lower()
    sym_id = str(row["id"])
    name = str(row["name"])
    qname = str(row["qualified_name"])
    name_lower = name.lower()
    qname_lower = qname.lower()

    if sym_id == q:
        return 1000.0, "exact_id"
    if name == q:
        return 950.0, "exact_name"
    if qname == q:
        return 900.0, "exact_qualified_name"
    if name_lower == q_lower:
        return 850.0, "exact_name_insensitive"
    if qname_lower == q_lower:
        return 800.0, "exact_qualified_name_insensitive"
    if name_lower.startswith(q_lower):
        return 700.0, "prefix_name"
    if qname_lower.startswith(q_lower):
        return 650.0, "prefix_qualified_name"
    if q_lower in name_lower:
        return 500.0, "substring_name"
    if q_lower in qname_lower:
        return 450.0, "substring_qualified_name"
    if q_lower in sym_id.lower():
        return 300.0, "substring_id"
    return 100.0, "fuzzy"


def _enrich_trace_hits(db: Database, hit_dicts: list[dict[str, Any]]) -> list[dict[str, Any]]:
    """Attach basic symbol metadata to dependency-trace hits."""
    if not hit_dicts:
        return hit_dicts

    conn = db.connect()
    symbol_ids = [str(h["symbol_id"]) for h in hit_dicts]
    placeholders = ", ".join("?" * len(symbol_ids))
    rows = conn.execute(
        f"SELECT id, kind, qualified_name, file_path, line_start, line_end "
        f"FROM symbol WHERE id IN ({placeholders})",
        symbol_ids,
    ).fetchall()
    meta_by_id = {str(row["id"]): row for row in rows}

    enriched: list[dict[str, Any]] = []
    for hit in hit_dicts:
        entry = dict(hit)
        row = meta_by_id.get(str(hit["symbol_id"]))
        if row is not None:
            entry["qualified_name"] = str(row["qualified_name"])
            entry["kind"] = str(row["kind"])
            entry["file_path"] = str(row["file_path"])
            entry["line_start"] = row["line_start"]
            entry["line_end"] = row["line_end"]
        enriched.append(entry)
    return enriched


def _record_tool_metrics(tool: str, start: float, ok: bool) -> None:
    """Update in-process counters/histograms for *tool*."""
    elapsed = time.perf_counter() - start
    METRICS.tool_calls.inc(tool)
    METRICS.tool_latency.observe(elapsed, tool)
    if not ok:
        METRICS.tool_errors.inc(tool)


def _check_elapsed(start: float, tool: str, *, enforce_soft: bool = True) -> None:
    """Raise McpError(TIMEOUT) if soft or hard wall time exceeded."""
    elapsed = time.perf_counter() - start
    if elapsed > _HARD_TIMEOUT_S:
        raise McpError(
            TIMEOUT,
            f"Tool '{tool}' exceeded hard wall time ({elapsed:.1f}s > {_HARD_TIMEOUT_S}s)",
            retryable=True,
        )
    if enforce_soft and elapsed > _SOFT_TIMEOUT_S:
        raise McpError(
            TIMEOUT,
            f"Tool '{tool}' exceeded soft wall time ({elapsed:.1f}s > {_SOFT_TIMEOUT_S}s)",
            retryable=True,
        )


def _remaining_timeout_s(start: float, timeout_s: float | None = None) -> float:
    """Return a positive timeout budget relative to *start*."""
    budget = _HARD_TIMEOUT_S if timeout_s is None else timeout_s
    return max(0.001, budget - (time.perf_counter() - start))


def _semantic_cooldown_remaining() -> float:
    """Return seconds left before semantic work may run again."""
    with _SEMANTIC_STATE_LOCK:
        return max(0.0, _SEMANTIC_DISABLED_UNTIL - time.monotonic())


def _mark_semantic_cooldown() -> None:
    """Temporarily stop semantic work after a timeout or stuck model load."""
    global _SEMANTIC_DISABLED_UNTIL
    with _SEMANTIC_STATE_LOCK:
        _SEMANTIC_DISABLED_UNTIL = max(
            _SEMANTIC_DISABLED_UNTIL,
            time.monotonic() + _SEMANTIC_COOLDOWN_S,
        )


def _run_with_deadline(
    tool: str,
    stage: str,
    start: float,
    fn: Callable[[], _T],
    *,
    timeout_s: float | None = None,
    exclusive_lock: threading.Lock | None = None,
    on_timeout: Callable[[], None] | None = None,
) -> _T:
    """Run a blocking stage on a daemon thread and fail fast on timeout.

    This is the Windows-safe guard for operations that can block inside native
    code or network-bound model loading, where POSIX ``signal`` timeouts do not
    help. When an exclusive lock is provided, wait only within the remaining
    deadline budget instead of failing immediately; this keeps short overlaps
    from turning into needless retry storms.
    """
    lock_acquired = False
    deadline_s = _remaining_timeout_s(start, timeout_s)
    if exclusive_lock is not None:
        lock_acquired = exclusive_lock.acquire(timeout=deadline_s)
        if not lock_acquired:
            raise McpError(
                TIMEOUT,
                f"Tool '{tool}' stage '{stage}' remained busy for {deadline_s:.1f}s",
                retryable=True,
            )

    result_queue: queue.Queue[tuple[bool, object]] = queue.Queue(maxsize=1)
    deadline_s = _remaining_timeout_s(start, timeout_s)

    def _worker() -> None:
        try:
            result_queue.put((True, fn()))
        except Exception as exc:
            result_queue.put((False, exc))
        finally:
            if lock_acquired and exclusive_lock is not None:
                exclusive_lock.release()

    thread = threading.Thread(
        target=_worker,
        name=f"cognis-mcp-{tool}-{stage}",
        daemon=True,
    )
    thread.start()

    try:
        ok, payload = result_queue.get(timeout=deadline_s)
    except queue.Empty as exc:
        if on_timeout is not None:
            on_timeout()
        raise McpError(
            TIMEOUT,
            f"Tool '{tool}' stage '{stage}' exceeded wall time ({deadline_s:.1f}s)",
            retryable=True,
        ) from exc

    if ok:
        return cast(_T, payload)
    if isinstance(payload, Exception):
        raise payload
    raise McpError(INTERNAL_ERROR, f"Tool '{tool}' stage '{stage}' failed")


def _run_semantic_with_deadline(
    tool: str,
    stage: str,
    start: float,
    fn: Callable[[], _T],
    *,
    timeout_s: float | None = None,
    cooldown_on_timeout: bool = True,
) -> _T:
    """Run semantic work with single-flight protection and timeout cooldown."""
    cooldown_remaining = _semantic_cooldown_remaining()
    if cooldown_remaining > 0:
        raise McpError(
            TIMEOUT,
            f"Semantic retrieval is cooling down after a timeout ({cooldown_remaining:.1f}s left)",
            retryable=True,
        )

    return _run_with_deadline(
        tool,
        stage,
        start,
        fn,
        timeout_s=timeout_s,
        exclusive_lock=_SEMANTIC_STAGE_LOCK,
        on_timeout=_mark_semantic_cooldown if cooldown_on_timeout else None,
    )


def _semantic_index_available(db: Database) -> bool:
    """Return True only when a usable vec0 table has indexed rows."""
    if not db.vec_enabled:
        return False

    try:
        conn = db.connect()
        row = conn.execute(
            "SELECT sql FROM sqlite_master WHERE type IN ('table','shadow') AND name='symbol_vec'"
        ).fetchone()
        if row is None or "USING vec0" not in str(row["sql"] or ""):
            return False
        return conn.execute("SELECT 1 FROM symbol_vec LIMIT 1").fetchone() is not None
    except Exception:
        logger.debug("Semantic index availability check failed", exc_info=True)
        return False


# ---------------------------------------------------------------------------
# Tool 1: symbol_lookup
# ---------------------------------------------------------------------------


def symbol_lookup(name_or_id: str, kind: str | None = None) -> dict[str, Any]:
    """Resolve a single symbol by exact id, qualified_name, or fuzzy name.

    Prefer :func:`symbol_search` when you need multiple ranked candidates or
    are exploring unknown symbol names. Use ``symbol_lookup`` when you already
    have a specific id or qualified name and want the full symbol record.

    Args:
        name_or_id: Symbol id, qualified_name, or partial name to search for.
        kind: Optional kind filter (e.g. ``"function"``, ``"class"``).

    Returns:
        Serialized SymbolNode dict, or error envelope if not found.
    """
    start = time.perf_counter()
    args = {"name_or_id": name_or_id, "kind": kind}
    ok = False
    try:
        if not name_or_id or not name_or_id.strip():
            raise McpError(INVALID_ARGUMENT, "name_or_id must be a non-empty string")

        db = _get_db()
        conn = db.connect()

        # Step 1: exact id match.
        sym = get_symbol(db, name_or_id)
        if sym is not None:
            if kind is None or sym.kind == kind:
                ok = True
                return _symbol_to_dict(sym)
            # Found by ID but wrong kind — try other lookups.
            sym = None

        _check_elapsed(start, "symbol_lookup")

        # Step 2: qualified_name exact match.
        row = conn.execute(
            "SELECT id FROM symbol WHERE qualified_name = ? LIMIT 1",
            (name_or_id,),
        ).fetchone()
        if row is not None:
            sym = get_symbol(db, str(row["id"]))
            if sym is not None and (kind is None or sym.kind == kind):
                ok = True
                return _symbol_to_dict(sym)

        _check_elapsed(start, "symbol_lookup")

        # Step 3: fuzzy name LIKE search.
        like_pattern = f"%{name_or_id}%"
        rows = conn.execute(
            "SELECT id FROM symbol WHERE name LIKE ? LIMIT 10",
            (like_pattern,),
        ).fetchall()

        candidates = []
        for r in rows:
            s = get_symbol(db, str(r["id"]))
            if s is not None and (kind is None or s.kind == kind):
                candidates.append(s)

        if candidates:
            # Return the first (best) candidate.
            ok = True
            return _symbol_to_dict(candidates[0])

        # Nothing found.
        raise McpError(
            SYMBOL_NOT_FOUND,
            f"Symbol not found: {name_or_id!r}" + (f" (kind={kind!r})" if kind else ""),
            retryable=False,
        )

    except McpError as exc:
        return exc.to_envelope()
    except Exception as exc:
        logger.exception("symbol_lookup unexpected error")
        return error_envelope(INTERNAL_ERROR, str(exc))
    finally:
        _record_tool_metrics("symbol_lookup", start, ok)
        audit_log_entry("symbol_lookup", args, ok, _get_audit_path())


# ---------------------------------------------------------------------------
# Tool 1b: symbol_search
# ---------------------------------------------------------------------------


def symbol_search(
    query: str,
    k: int = 8,
    kind: str | None = None,
    path_prefix: str | None = None,
    exclude_path_prefixes: list[str] | None = None,
    file_path: str | None = None,
) -> list[dict[str, Any]] | dict[str, Any]:
    """Discover symbols with ranked multi-result search.

    Recommended for fuzzy or exploratory lookups. Returns up to *k* hits with
    enough metadata to act without follow-up lookups.

    Args:
        query: Name fragment, qualified name, or symbol id to search for.
        k: Maximum results (clamped to 50, default 8).
        kind: Optional kind filter (e.g. ``"function"``, ``"class"``).
        path_prefix: Optional file-path prefix filter.
        file_path: Alias for ``path_prefix`` (exact file or directory prefix).
        exclude_path_prefixes: Optional list of file-path prefixes to exclude.

    Returns:
        List of ranked hit dicts, or error envelope on failure.
    """
    path_prefix = _effective_path_prefix(path_prefix, file_path)
    start = time.perf_counter()
    args = {
        "query": query,
        "k": k,
        "kind": kind,
        "path_prefix": path_prefix,
        "exclude_path_prefixes": exclude_path_prefixes,
    }
    ok = False
    try:
        cached = cache_get("symbol_search", args)
        if cached is not None:
            ok = True
            return cached

        if not query or not query.strip():
            raise McpError(INVALID_ARGUMENT, "query must be a non-empty string")

        k = max(1, min(k, _MAX_SYMBOL_SEARCH_K))
        _check_elapsed(start, "symbol_search")
        results = _symbol_search_core(
            query,
            k,
            kind=kind,
            path_prefix=path_prefix,
            exclude_path_prefixes=exclude_path_prefixes,
        )
        _check_elapsed(start, "symbol_search")

        ok = True
        if _cacheable_result(results):
            cache_set("symbol_search", args, results)
        return results

    except McpError as exc:
        return exc.to_envelope()
    except Exception as exc:
        logger.exception("symbol_search unexpected error")
        return error_envelope(INTERNAL_ERROR, str(exc))
    finally:
        _record_tool_metrics("symbol_search", start, ok)
        audit_log_entry("symbol_search", args, ok, _get_audit_path())


def _symbol_search_core(
    query: str,
    k: int,
    *,
    kind: str | None = None,
    path_prefix: str | None = None,
    exclude_path_prefixes: list[str] | None = None,
) -> list[dict[str, Any]]:
    """Internal symbol search without metrics, audit, or cache."""
    path_prefix = _effective_path_prefix(path_prefix, None)
    q = query.strip()
    db = _get_db()
    conn = db.connect()

    ranked: dict[str, tuple[float, str, Any]] = {}
    exact_sym = get_symbol(db, q)
    if (
        exact_sym is not None
        and (kind is None or exact_sym.kind == kind)
        and _matches_path_filters(str(exact_sym.file_path), path_prefix, exclude_path_prefixes)
    ):
        ranked[exact_sym.id] = (1000.0, "exact_id", exact_sym)

    candidate_limit = min(max(k * 10, 50), 250)
    conditions = ["(name LIKE ? OR qualified_name LIKE ? OR id LIKE ?)"]
    params: list[Any] = [f"%{q}%", f"%{q}%", f"%{q}%"]

    if kind is not None:
        conditions.append("kind = ?")
        params.append(kind)
    if path_prefix is not None:
        conditions.append("file_path LIKE ?")
        params.append(f"{path_prefix}%")
    if exclude_path_prefixes:
        for prefix in exclude_path_prefixes:
            if prefix:
                conditions.append("file_path NOT LIKE ?")
                params.append(f"{prefix}%")

    sql = (
        "SELECT id, kind, name, qualified_name, file_path, line_start, line_end, "
        "signature, docstring, body_excerpt FROM symbol WHERE "
        + " AND ".join(conditions)
        + " LIMIT ?"
    )
    params.append(candidate_limit)
    rows = conn.execute(sql, params).fetchall()

    for row in rows:
        sym_id = str(row["id"])
        score, reason = _score_symbol_match(q, row)
        prev = ranked.get(sym_id)
        if prev is None or score > prev[0]:
            ranked[sym_id] = (score, reason, row)

    ordered = sorted(ranked.values(), key=lambda item: (-item[0], str(item[2]["id"])))
    results: list[dict[str, Any]] = []
    for score, reason, item in ordered[:k]:
        if hasattr(item, "id"):
            sym = item
            results.append(
                {
                    "symbol_id": sym.id,
                    "id": sym.id,
                    "name": sym.name,
                    "qualified_name": sym.qualified_name,
                    "kind": sym.kind,
                    "file_path": sym.file_path,
                    "line_start": sym.line_start,
                    "line_end": sym.line_end,
                    "score": score,
                    "match_reason": reason,
                    "match_sources": ["lexical"],
                    "lexical_score": score,
                    "snippet": sym.body_excerpt,
                    "body_excerpt": sym.body_excerpt,
                    "signature": sym.signature,
                    "docstring": sym.docstring,
                }
            )
        else:
            results.append(
                _symbol_row_to_search_hit(
                    item,
                    score=score,
                    match_reason=reason,
                    match_sources=["lexical"],
                    lexical_score=score,
                )
            )
    return results


# ---------------------------------------------------------------------------
# Tool 2: semantic_search
# ---------------------------------------------------------------------------


def _semantic_search_core(
    query: str,
    k: int,
    *,
    kind: str | None = None,
    path_prefix: str | None = None,
    exclude_path_prefixes: list[str] | None = None,
) -> list[dict[str, Any]]:
    """Internal semantic search without metrics, audit, or cache."""
    from cognis_mcpd.embedder_pool import get_shared_semantic_layer

    db = _get_db()
    if not _semantic_index_available(db):
        return []

    layer = get_shared_semantic_layer()

    fetch_k = k
    if kind is not None or path_prefix is not None or exclude_path_prefixes:
        fetch_k = min(k * 5, _MAX_K)

    hits = layer.search(query, fetch_k, db)
    if not hits:
        return []

    symbol_ids = [str(hit.symbol_id) for hit in hits]
    rows_by_id = _batch_fetch_symbol_rows(db, symbol_ids)

    results: list[dict[str, Any]] = []
    for hit in hits:
        row = rows_by_id.get(str(hit.symbol_id))
        if row is None:
            results.append(
                {
                    "symbol_id": hit.symbol_id,
                    "id": hit.symbol_id,
                    "score": hit.score,
                    "kind": "unknown",
                    "name": hit.symbol_id,
                    "qualified_name": hit.symbol_id,
                    "file_path": None,
                    "line_start": None,
                    "line_end": None,
                    "match_reason": "semantic",
                    "match_sources": ["semantic"],
                    "semantic_score": hit.score,
                    "snippet": None,
                    "body_excerpt": None,
                }
            )
            continue

        file_path_value = str(row["file_path"])
        if kind is not None and str(row["kind"]) != kind:
            continue
        if not _matches_path_filters(file_path_value, path_prefix, exclude_path_prefixes):
            continue

        results.append(
            {
                "symbol_id": str(row["id"]),
                "id": str(row["id"]),
                "name": str(row["name"]),
                "qualified_name": str(row["qualified_name"]),
                "kind": str(row["kind"]),
                "file_path": file_path_value,
                "line_start": row["line_start"],
                "line_end": row["line_end"],
                "score": hit.score,
                "match_reason": "semantic",
                "match_sources": ["semantic"],
                "semantic_score": hit.score,
                "snippet": row["body_excerpt"],
                "body_excerpt": row["body_excerpt"],
                "signature": row["signature"],
                "docstring": row["docstring"],
            }
        )
        if len(results) >= k:
            break
    return results


def semantic_search(
    query: str,
    k: int = 10,
    mode: str | None = None,
    kind: str | None = None,
    path_prefix: str | None = None,
    exclude_path_prefixes: list[str] | None = None,
    file_path: str | None = None,
) -> list[dict[str, Any]] | dict[str, Any]:
    """Semantic search with actionable symbol payloads.

    Args:
        query: Natural-language query string.
        k: Maximum number of results (clamped to 50).
        mode: Deprecated alias for ``kind``.
        kind: Optional kind filter (e.g. ``"function"``).
        path_prefix: Optional file-path prefix filter.
        file_path: Alias for ``path_prefix``.
        exclude_path_prefixes: Optional path prefixes to exclude.

    Returns:
        List of enriched hit dicts (location, signature, snippet), or error envelope.
    """
    path_prefix = _effective_path_prefix(path_prefix, file_path)
    effective_kind = kind if kind is not None else mode
    start = time.perf_counter()
    args = {
        "query": query,
        "k": k,
        "kind": effective_kind,
        "path_prefix": path_prefix,
        "exclude_path_prefixes": exclude_path_prefixes,
    }
    ok = False
    try:
        cached = cache_get("semantic_search", args)
        if cached is not None:
            ok = True
            return cached

        if not query or not query.strip():
            raise McpError(INVALID_ARGUMENT, "query must be a non-empty string")

        k = max(1, min(k, _MAX_K))

        db = _get_db()
        if not _semantic_index_available(db):
            ok = True
            results: list[dict[str, Any]] = []
            cache_set("semantic_search", args, results)
            return results

        try:
            results = _run_semantic_with_deadline(
                "semantic_search",
                "semantic_retrieval",
                start,
                lambda: _semantic_search_core(
                    query,
                    k,
                    kind=effective_kind,
                    path_prefix=path_prefix,
                    exclude_path_prefixes=exclude_path_prefixes,
                ),
            )
        except McpError:
            raise
        except ImportError as exc:
            raise McpError(
                EMBEDDER_UNAVAILABLE,
                "sentence-transformers or cognis_indexer is not installed; "
                "semantic search is unavailable.",
                retryable=False,
            ) from exc
        except Exception as exc:
            raise McpError(
                EMBEDDER_UNAVAILABLE,
                f"Semantic retrieval unavailable: {exc}",
                retryable=False,
            ) from exc

        _check_elapsed(start, "semantic_search", enforce_soft=False)

        ok = True
        if _cacheable_result(results):
            cache_set("semantic_search", args, results)
        return results

    except McpError as exc:
        return exc.to_envelope()
    except Exception as exc:
        logger.exception("semantic_search unexpected error")
        return error_envelope(INTERNAL_ERROR, str(exc))
    finally:
        _record_tool_metrics("semantic_search", start, ok)
        audit_log_entry("semantic_search", args, ok, _get_audit_path())


# ---------------------------------------------------------------------------
# Tool 2b: discover_symbols (hybrid lexical + semantic)
# ---------------------------------------------------------------------------


def discover_symbols(
    query: str,
    k: int = 10,
    kind: str | None = None,
    path_prefix: str | None = None,
    exclude_path_prefixes: list[str] | None = None,
    file_path: str | None = None,
) -> list[dict[str, Any]] | dict[str, Any]:
    """Hybrid discovery merging lexical and semantic evidence in one call.

    Uses reciprocal-rank fusion so agents get a single ranked shortlist without
    separate ``symbol_search`` + ``semantic_search`` round trips.

    Args:
        query: Name fragment, keyword, or natural-language intent.
        k: Maximum fused results (clamped to 50, default 10).
        kind: Optional kind filter.
        path_prefix: Optional file-path prefix filter.
        file_path: Alias for ``path_prefix``.
        exclude_path_prefixes: Optional path prefixes to exclude.

    Returns:
        List of fused hit dicts with ``match_sources``, or error envelope.
    """
    path_prefix = _effective_path_prefix(path_prefix, file_path)
    start = time.perf_counter()
    args = {
        "query": query,
        "k": k,
        "kind": kind,
        "path_prefix": path_prefix,
        "exclude_path_prefixes": exclude_path_prefixes,
    }
    ok = False
    try:
        cached = cache_get("discover_symbols", args)
        if cached is not None:
            ok = True
            return cached

        if not query or not query.strip():
            raise McpError(INVALID_ARGUMENT, "query must be a non-empty string")

        k = max(1, min(k, _MAX_K))
        fetch_k = min(max(k * 2, k), _MAX_K)
        db = _get_db()

        _check_elapsed(start, "discover_symbols")
        lexical_ranked_lists: list[list[tuple[str, float, str]]] = []
        for variant in _discover_query_variants(query):
            variant_hits = _symbol_search_core(
                variant,
                fetch_k,
                kind=kind,
                path_prefix=path_prefix,
                exclude_path_prefixes=exclude_path_prefixes,
            )
            if variant_hits:
                lexical_ranked_lists.append(
                    [
                        (str(hit["symbol_id"]), float(hit["score"]), "lexical")
                        for hit in variant_hits
                    ]
                )

        fts_hits = _fts_search_core(
            query,
            fetch_k,
            kind=kind,
            path_prefix=path_prefix,
            exclude_path_prefixes=exclude_path_prefixes,
        )
        if fts_hits:
            lexical_ranked_lists.append(
                [(str(hit["symbol_id"]), float(hit["score"]), "lexical") for hit in fts_hits]
            )

        semantic_hits: list[dict[str, Any]] = []
        semantic_attempted = False
        semantic_failed = False
        if _semantic_index_available(db):
            semantic_attempted = True
            try:
                semantic_hits = _run_semantic_with_deadline(
                    "discover_symbols",
                    "semantic_leg",
                    start,
                    lambda: _semantic_search_core(
                        query,
                        fetch_k,
                        kind=kind,
                        path_prefix=path_prefix,
                        exclude_path_prefixes=exclude_path_prefixes,
                    ),
                    timeout_s=_DISCOVER_SEMANTIC_TIMEOUT_S,
                    cooldown_on_timeout=False,
                )
            except Exception:
                semantic_failed = True
                logger.debug("Semantic leg unavailable for discover_symbols", exc_info=True)

        _check_elapsed(start, "discover_symbols", enforce_soft=not semantic_attempted)

        ranked_lists: list[list[tuple[str, float, str]]] = list(lexical_ranked_lists)
        if semantic_hits:
            ranked_lists.append(
                [(str(hit["symbol_id"]), float(hit["score"]), "semantic") for hit in semantic_hits]
            )

        if not ranked_lists:
            ok = True
            if not semantic_failed:
                cache_set("discover_symbols", args, [])
            return []

        fused = _rrf_fuse(ranked_lists, k=k)
        symbol_ids = [symbol_id for symbol_id, _, _, _ in fused]
        rows_by_id = _batch_fetch_symbol_rows(_get_db(), symbol_ids)

        results: list[dict[str, Any]] = []
        for symbol_id, fused_score, sources, raw_scores in fused:
            row = rows_by_id.get(symbol_id)
            if row is None:
                continue
            results.append(
                _symbol_row_to_search_hit(
                    row,
                    score=fused_score,
                    match_reason="hybrid_rrf",
                    match_sources=sources,
                    lexical_score=raw_scores.get("lexical"),
                    semantic_score=raw_scores.get("semantic"),
                )
            )

        ok = True
        if _cacheable_result(results):
            cache_set("discover_symbols", args, results)
        return results

    except McpError as exc:
        return exc.to_envelope()
    except Exception as exc:
        logger.exception("discover_symbols unexpected error")
        return error_envelope(INTERNAL_ERROR, str(exc))
    finally:
        _record_tool_metrics("discover_symbols", start, ok)
        audit_log_entry("discover_symbols", args, ok, _get_audit_path())


# ---------------------------------------------------------------------------
# Tool 2c: diffuse_context (CSAR — flagship retrieval)
# ---------------------------------------------------------------------------


def _csar_seed_hits(
    query: str,
    seed_k: int,
    start: float,
    *,
    kind: str | None,
    path_prefix: str | None,
    exclude_path_prefixes: list[str] | None,
) -> list[list[Any]]:
    """Build per-layer seed Hit lists (lexical + semantic) for CSAR diffusion."""
    from cognis_retrieval.base import Hit

    db = _get_db()
    seed_layers: list[list[Hit]] = []

    # Lexical leg: tokenized FTS over the query (cheap, always available).
    fts_hits = _fts_search_core(
        query,
        seed_k,
        kind=kind,
        path_prefix=path_prefix,
        exclude_path_prefixes=exclude_path_prefixes,
    )
    if fts_hits:
        seed_layers.append(
            [
                Hit(
                    symbol_id=str(h["symbol_id"]),
                    score=float(h["score"]),
                    layer="lexical",
                    reason="fts_bm25",
                )
                for h in fts_hits
            ]
        )

    # Semantic leg: embedding KNN, guarded by the shared deadline/cooldown.
    if _semantic_index_available(db):
        try:
            sem_hits = _run_semantic_with_deadline(
                "diffuse_context",
                "semantic_seed",
                start,
                lambda: _semantic_search_core(
                    query,
                    seed_k,
                    kind=kind,
                    path_prefix=path_prefix,
                    exclude_path_prefixes=exclude_path_prefixes,
                ),
                timeout_s=_DISCOVER_SEMANTIC_TIMEOUT_S,
                cooldown_on_timeout=False,
            )
        except Exception:
            sem_hits = []
            logger.debug("Semantic seed leg unavailable for diffuse_context", exc_info=True)
        if sem_hits:
            seed_layers.append(
                [
                    Hit(
                        symbol_id=str(h["symbol_id"]),
                        score=float(h["score"]),
                        layer="semantic",
                        reason="semantic_knn",
                    )
                    for h in sem_hits
                ]
            )

    return seed_layers


def diffuse_context(
    query: str,
    k: int = 10,
    alpha: float | None = None,
    eps: float | None = None,
    kind: str | None = None,
    path_prefix: str | None = None,
    exclude_path_prefixes: list[str] | None = None,
    file_path: str | None = None,
) -> list[dict[str, Any]] | dict[str, Any]:
    """Flagship retrieval: spreading-activation over the code graph (CSAR).

    Seeds a relevance distribution from cheap lexical + semantic matches, then
    diffuses it across the Unified Code Knowledge Graph using Personalized
    PageRank (random walk with restart). Unlike independent ranking, this
    recovers symbols that sit on the *call/flow path* between matches even when
    they have no direct lexical or semantic hit -- in one round trip, replacing
    separate ``discover_symbols`` + ``dependency_trace`` calls.

    The diffusion uses forward-push, whose cost is bounded by ``1/(alpha*eps)``
    independent of repository size (see ``docs/csar.md``).

    Args:
        query: Natural-language intent or keywords.
        k: Maximum ranked results (clamped to 50, default 10).
        alpha: Restart probability in ``(0, 1]``; lower spreads farther along
            code flow (more structural), higher stays near seeds (more
            semantic). Defaults to ``0.15``.
        eps: Forward-push residual threshold; smaller is more thorough but does
            more work. Defaults to ``1e-5``.
        kind: Optional symbol-kind filter applied to seeds.
        path_prefix: Optional file-path prefix filter applied to seeds.
        exclude_path_prefixes: Optional path prefixes to exclude from seeds.
        file_path: Alias for ``path_prefix``.

    Returns:
        List of ranked hit dicts. Each carries ``match_reason="csar_diffusion"``,
        an ``on_path`` flag (True when reached via code flow rather than a direct
        seed match), and ``match_sources``. Returns an error envelope on failure.
    """
    path_prefix = _effective_path_prefix(path_prefix, file_path)
    eff_alpha = _CSAR_DEFAULT_ALPHA if alpha is None else alpha
    eff_eps = _CSAR_DEFAULT_EPS if eps is None else eps
    start = time.perf_counter()
    args = {
        "query": query,
        "k": k,
        "alpha": eff_alpha,
        "eps": eff_eps,
        "kind": kind,
        "path_prefix": path_prefix,
        "exclude_path_prefixes": exclude_path_prefixes,
    }
    ok = False
    try:
        cached = cache_get("diffuse_context", args)
        if cached is not None:
            ok = True
            return cached

        if not query or not query.strip():
            raise McpError(INVALID_ARGUMENT, "query must be a non-empty string")
        if not 0.0 < eff_alpha <= 1.0:
            raise McpError(INVALID_ARGUMENT, f"alpha must be in (0, 1]; got {eff_alpha}")
        if eff_eps <= 0.0:
            raise McpError(INVALID_ARGUMENT, f"eps must be > 0; got {eff_eps}")

        k = max(1, min(k, _MAX_K))
        db = _get_db()

        try:
            from cognis_retrieval.csar import build_code_graph, diffuse_seed_hits
        except ImportError as exc:
            raise McpError(
                EMBEDDER_UNAVAILABLE,
                "numpy is required for CSAR diffusion; install cognis[embed-local].",
                retryable=False,
            ) from exc

        seed_layers = _csar_seed_hits(
            query,
            _CSAR_SEED_K,
            start,
            kind=kind,
            path_prefix=path_prefix,
            exclude_path_prefixes=exclude_path_prefixes,
        )
        _check_elapsed(start, "diffuse_context", enforce_soft=False)

        if not seed_layers:
            ok = True
            cache_set("diffuse_context", args, [])
            return []

        graph = build_code_graph(db)
        diffused = diffuse_seed_hits(graph, seed_layers, k=k, alpha=eff_alpha, eps=eff_eps)
        _check_elapsed(start, "diffuse_context", enforce_soft=False)

        if not diffused:
            ok = True
            cache_set("diffuse_context", args, [])
            return []

        rows_by_id = _batch_fetch_symbol_rows(db, [h.symbol_id for h in diffused])
        results: list[dict[str, Any]] = []
        for hit in diffused:
            row = rows_by_id.get(hit.symbol_id)
            if row is None:
                continue
            on_path = not bool(hit.evidence.get("seed", False))
            entry = _symbol_row_to_search_hit(
                row,
                score=hit.score,
                match_reason="csar_diffusion",
                match_sources=["csar"],
            )
            entry["on_path"] = on_path
            entry["ppr_score"] = float(hit.evidence.get("ppr", hit.score))
            results.append(entry)

        ok = True
        if _cacheable_result(results):
            cache_set("diffuse_context", args, results)
        return results

    except McpError as exc:
        return exc.to_envelope()
    except Exception as exc:
        logger.exception("diffuse_context unexpected error")
        return error_envelope(INTERNAL_ERROR, str(exc))
    finally:
        _record_tool_metrics("diffuse_context", start, ok)
        audit_log_entry("diffuse_context", args, ok, _get_audit_path())


# ---------------------------------------------------------------------------
# Tool 2d: resolve_symbols (batch hydration)
# ---------------------------------------------------------------------------


def resolve_symbols(
    symbol_ids: list[str],
    include_body: bool = True,
) -> dict[str, Any]:
    """Hydrate multiple symbols in one call.

    Use after discovery tools when you need full records for several ids without
    repeated ``symbol_lookup`` calls.

    Args:
        symbol_ids: Symbol ids to resolve (deduplicated, max 50).
        include_body: When False, omit ``body_excerpt`` to save tokens.

    Returns:
        ``{"symbols": [...], "missing": [...], "requested_count", "resolved_count"}``
        or error envelope.
    """
    start = time.perf_counter()
    args = {"symbol_ids": symbol_ids, "include_body": include_body}
    ok = False
    try:
        if not symbol_ids:
            raise McpError(INVALID_ARGUMENT, "symbol_ids must be a non-empty list")

        if len(symbol_ids) > _MAX_RESOLVE_IDS:
            raise McpError(
                INVALID_ARGUMENT,
                f"symbol_ids exceeds max {_MAX_RESOLVE_IDS} ids",
                retryable=False,
            )

        seen: set[str] = set()
        ordered_ids: list[str] = []
        for raw_id in symbol_ids:
            sym_id = str(raw_id).strip()
            if not sym_id or sym_id in seen:
                continue
            seen.add(sym_id)
            ordered_ids.append(sym_id)

        if not ordered_ids:
            raise McpError(INVALID_ARGUMENT, "symbol_ids must contain valid ids")

        db = _get_db()
        rows_by_id = _batch_fetch_symbol_rows(db, ordered_ids)

        symbols: list[dict[str, Any]] = []
        missing: list[str] = []
        for sym_id in ordered_ids:
            row = rows_by_id.get(sym_id)
            if row is None:
                missing.append(sym_id)
                continue
            symbols.append(_row_to_symbol_dict(row, include_body=include_body))

        ok = True
        return {
            "symbols": symbols,
            "missing": missing,
            "requested_count": len(ordered_ids),
            "resolved_count": len(symbols),
        }

    except McpError as exc:
        return exc.to_envelope()
    except Exception as exc:
        logger.exception("resolve_symbols unexpected error")
        return error_envelope(INTERNAL_ERROR, str(exc))
    finally:
        _record_tool_metrics("resolve_symbols", start, ok)
        audit_log_entry("resolve_symbols", args, ok, _get_audit_path())


# ---------------------------------------------------------------------------
# Tool 3: dependency_trace
# ---------------------------------------------------------------------------


def dependency_trace(symbol_id: str, direction: str = "out", depth: int = 3) -> dict[str, Any]:
    """Trace symbol dependencies via the call graph.

    Args:
        symbol_id: Starting symbol id.
        direction: ``"out"`` (callees), ``"in"`` (callers), or ``"both"``.
        depth: Traversal depth (clamped to 8).

    Returns:
        ``{"start": symbol_id, "direction": direction, "depth": depth,
        "hits": [...]}`` or error envelope.
    """
    start_time = time.perf_counter()
    args = {"symbol_id": symbol_id, "direction": direction, "depth": depth}
    ok = False
    try:
        if not symbol_id or not symbol_id.strip():
            raise McpError(INVALID_ARGUMENT, "symbol_id must be a non-empty string")

        if direction not in ("out", "in", "both"):
            raise McpError(
                INVALID_ARGUMENT,
                f"direction must be 'out', 'in', or 'both'; got {direction!r}",
                retryable=False,
            )

        # Clamp depth.
        depth = max(1, min(depth, _MAX_DEPTH))

        db = _get_db()

        _check_elapsed(start_time, "dependency_trace")

        # Import structural layer, stubbing out numpy-dependent semantic module
        # in case numpy is not installed (test environments without embed-local extra).
        import sys as _sys
        import types as _types

        _needs_semantic_stub = (
            "cognis_retrieval" not in _sys.modules
            and "cognis_retrieval.semantic" not in _sys.modules
        )
        _semantic_stub = None
        if _needs_semantic_stub:
            try:
                import numpy  # noqa: F401

                _needs_semantic_stub = False
            except ImportError:
                # Numpy absent: pre-populate a stub so cognis_retrieval.__init__
                # doesn't fail when it tries to import semantic.
                _semantic_stub = _types.ModuleType("cognis_retrieval.semantic")
                _semantic_stub.SemanticLayer = object  # type: ignore[attr-defined]
                _semantic_stub.populate_vec = lambda *a, **kw: None  # type: ignore[attr-defined]
                _sys.modules["cognis_retrieval.semantic"] = _semantic_stub

        from cognis_retrieval.structural import (
            StructuralLayer,  # type: ignore[import]
        )

        layer = StructuralLayer()
        hits = layer.dependency_trace(symbol_id, direction, depth, db)

        _check_elapsed(start_time, "dependency_trace")

        hit_dicts = [
            {
                "symbol_id": h.symbol_id,
                "score": h.score,
                "layer": h.layer,
                "reason": h.reason,
                "evidence": h.evidence,
            }
            for h in hits
        ]
        hit_dicts = _enrich_trace_hits(db, hit_dicts)
        hit_dicts = [
            hit
            for hit in hit_dicts
            if not hit.get("file_path") or _matches_path_filters(str(hit["file_path"]), None, None)
        ]

        ok = True
        return {
            "start": symbol_id,
            "direction": direction,
            "depth": depth,
            "hits": hit_dicts,
        }

    except McpError as exc:
        return exc.to_envelope()
    except Exception as exc:
        logger.exception("dependency_trace unexpected error")
        return error_envelope(INTERNAL_ERROR, str(exc))
    finally:
        _record_tool_metrics("dependency_trace", start_time, ok)
        audit_log_entry("dependency_trace", args, ok, _get_audit_path())


# ---------------------------------------------------------------------------
# Tool 4: retrieve_context_capsule
# ---------------------------------------------------------------------------


def retrieve_context_capsule(
    task: str,
    max_tokens: int = 8000,
    include_runtime: bool = False,
) -> dict[str, Any]:
    """End-to-end: classify, plan, retrieve, compose a Context Capsule.

    Args:
        task: User task / query string.
        max_tokens: Token budget for the capsule (clamped to 32000).
        include_runtime: If True, include runtime evidence (Phase 3; currently
            no-op since behavioral layer is not yet implemented).

    Returns:
        Serialized ContextCapsule dict, or error envelope on failure.
    """
    start_time = time.perf_counter()
    args = {"task": task, "max_tokens": max_tokens, "include_runtime": include_runtime}
    ok = False
    try:
        if not task or not task.strip():
            raise McpError(INVALID_ARGUMENT, "task must be a non-empty string")

        # Clamp max_tokens.
        max_tokens = max(500, min(max_tokens, _MAX_TOKENS))

        db = _get_db()

        # Check DB is accessible.
        try:
            db.connect()
        except Exception as exc:
            raise McpError(
                INDEX_NOT_READY,
                f"Database not accessible: {exc}",
                retryable=True,
            ) from exc

        planner = Planner()

        # Step 1: classify.
        mode, confidence = planner.classify(task)

        _check_elapsed(start_time, "retrieve_context_capsule")

        # Step 2: layer plan.
        plan = planner.layer_plan(mode)

        # Step 3: allocate budget (available layers at MVP).
        semantic_available = _semantic_index_available(db)
        available_layers = {"lexical", "structural"}
        if semantic_available:
            available_layers.add("semantic")
        quotas = planner.allocate_budget(max_tokens, plan, available_layers)

        _check_elapsed(start_time, "retrieve_context_capsule")

        # Step 4: run retrieval layers.
        from cognis_retrieval.base import Hit
        from cognis_retrieval.lexical import LexicalLayer

        all_hits: list[Hit] = []
        lex_hits: list[Hit] = []
        sem_hits: list[Hit] = []

        # Lexical retrieval.
        try:
            lex_layer = LexicalLayer()
            k_lex = max(1, quotas.lexical // 50)  # ~50 tokens per hit estimate
            lex_hits = _filter_retrieval_hits(db, lex_layer.search(task, k_lex, db))
            all_hits.extend(lex_hits)
        except Exception:
            logger.debug("Lexical retrieval failed", exc_info=True)

        _check_elapsed(start_time, "retrieve_context_capsule")

        # Semantic retrieval (skip if embedder is unavailable, busy, or cooling down).
        semantic_attempted = False
        if semantic_available:
            semantic_attempted = True
            try:
                from cognis_mcpd.embedder_pool import get_shared_semantic_layer

                k_sem = max(1, quotas.semantic // 100)
                sem_hits = _run_semantic_with_deadline(
                    "retrieve_context_capsule",
                    "semantic_leg",
                    start_time,
                    lambda: get_shared_semantic_layer().search(task, k_sem, db),
                    timeout_s=_DISCOVER_SEMANTIC_TIMEOUT_S,
                    cooldown_on_timeout=False,
                )
                sem_hits = _filter_retrieval_hits(db, sem_hits)
                all_hits.extend(sem_hits)
            except Exception:
                logger.debug(
                    "Semantic retrieval failed (embedder may be unavailable)", exc_info=True
                )

        _check_elapsed(
            start_time,
            "retrieve_context_capsule",
            enforce_soft=not semantic_attempted,
        )

        # Structural stage — CSAR spreading-activation (flagship engine).
        #
        # Instead of a single-hop BFS from the top RRF seeds, diffuse the
        # lexical + semantic hits over the whole code graph via Personalized
        # PageRank (docs/csar.md). This is the primary way cognis recovers the
        # full flow around a relevant region: multi-hop, weighted, and
        # bidirectional, with cost bounded by 1/(alpha*eps) regardless of repo
        # size. Diffused on-path symbols are tagged structural so the bugfix
        # composer surfaces them as root-cause candidates.
        seed_layers = [hit_list for hit_list in (lex_hits, sem_hits) if hit_list]
        if seed_layers:
            try:
                from cognis_retrieval.csar import build_code_graph, diffuse_seed_hits

                k_struct = max(5, quotas.structural // 80)
                graph = build_code_graph(db)
                diffused = diffuse_seed_hits(
                    graph,
                    seed_layers,
                    k=k_struct,
                    alpha=_CSAR_DEFAULT_ALPHA,
                    eps=_CSAR_DEFAULT_EPS,
                )
                seed_ids = {h.symbol_id for hit_list in seed_layers for h in hit_list}
                struct_hits: list[Hit] = []
                for hit in diffused:
                    # Only the *newly reached* (on-path) symbols add structural
                    # signal; direct seed matches are already in all_hits.
                    if hit.symbol_id in seed_ids:
                        continue
                    struct_hits.append(
                        Hit(
                            symbol_id=hit.symbol_id,
                            score=hit.score,
                            layer="structural",
                            reason=hit.reason,
                            evidence=hit.evidence,
                        )
                    )
                all_hits.extend(_filter_retrieval_hits(db, struct_hits))
            except Exception:
                logger.debug("CSAR diffusion stage failed", exc_info=True)

        _check_elapsed(
            start_time,
            "retrieve_context_capsule",
            enforce_soft=not semantic_attempted,
        )

        # Step 5: compose capsule.
        from cognis.capsule.composer import CapsuleComposer, ComposeError

        composer = CapsuleComposer()
        try:
            capsule = composer.compose(
                task=task,
                mode=mode,
                confidence=confidence,
                hits=all_hits,
                max_tokens=max_tokens,
                db=db,
                include_runtime=include_runtime,
            )
        except ComposeError as exc:
            raise McpError(
                INTERNAL_ERROR, f"Capsule composition failed: {exc}", retryable=False
            ) from exc

        ok = True
        return capsule.model_dump(by_alias=True)

    except McpError as exc:
        return exc.to_envelope()
    except Exception as exc:
        logger.exception("retrieve_context_capsule unexpected error")
        return error_envelope(INTERNAL_ERROR, str(exc))
    finally:
        _record_tool_metrics("retrieve_context_capsule", start_time, ok)
        audit_log_entry("retrieve_context_capsule", args, ok, _get_audit_path())


__all__ = [
    "dependency_trace",
    "diffuse_context",
    "discover_symbols",
    "resolve_symbols",
    "retrieve_context_capsule",
    "semantic_search",
    "symbol_lookup",
    "symbol_search",
]
