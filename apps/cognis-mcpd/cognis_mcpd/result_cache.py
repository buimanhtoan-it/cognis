"""Short-lived in-process cache for MCP search tool results.

Repeated identical search queries within a session reuse hydrated payloads,
avoiding redundant embedder calls and SQL round trips.
"""

from __future__ import annotations

import hashlib
import json
import os
import threading
import time
from collections import OrderedDict
from typing import Any

_DEFAULT_TTL_S = float(os.environ.get("COGNIS_MCP_CACHE_TTL_S", "60"))
_MAX_ENTRIES = int(os.environ.get("COGNIS_MCP_CACHE_MAX", "128"))


class _ResultCache:
    """Thread-safe TTL cache keyed by tool name + normalized arguments."""

    def __init__(self, *, ttl_s: float = _DEFAULT_TTL_S, max_entries: int = _MAX_ENTRIES) -> None:
        self._ttl_s = ttl_s
        self._max_entries = max_entries
        self._lock = threading.Lock()
        self._entries: OrderedDict[str, tuple[float, Any]] = OrderedDict()

    @staticmethod
    def _make_key(tool: str, args: dict[str, Any]) -> str:
        payload = json.dumps({"tool": tool, "args": args}, sort_keys=True, default=str)
        return hashlib.sha256(payload.encode("utf-8")).hexdigest()

    def get(self, tool: str, args: dict[str, Any]) -> Any | None:
        key = self._make_key(tool, args)
        now = time.monotonic()
        with self._lock:
            entry = self._entries.get(key)
            if entry is None:
                return None
            expires_at, value = entry
            if expires_at <= now:
                self._entries.pop(key, None)
                return None
            self._entries.move_to_end(key)
            return value

    def set(self, tool: str, args: dict[str, Any], value: Any) -> None:
        key = self._make_key(tool, args)
        expires_at = time.monotonic() + self._ttl_s
        with self._lock:
            self._entries[key] = (expires_at, value)
            self._entries.move_to_end(key)
            while len(self._entries) > self._max_entries:
                self._entries.popitem(last=False)

    def clear(self) -> None:
        with self._lock:
            self._entries.clear()


CACHE = _ResultCache()


def cache_get(tool: str, args: dict[str, Any]) -> Any | None:
    """Return a cached result for *tool*/*args*, or ``None`` when absent/expired."""
    return CACHE.get(tool, args)


def cache_set(tool: str, args: dict[str, Any], value: Any) -> None:
    """Store *value* for *tool*/*args*."""
    CACHE.set(tool, args, value)


def reset_cache_for_tests() -> None:
    """Clear cached entries (test helper only)."""
    CACHE.clear()


__all__ = ["CACHE", "cache_get", "cache_set", "reset_cache_for_tests"]
