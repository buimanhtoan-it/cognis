"""Error types and error envelope for the cognis MCP server.

Design reference: design.md §MCP Server — Error envelope::

    {"error": {"code": "...", "message": "...", "retryable": true | false}}

All MCP tool handlers catch exceptions and return this envelope rather than
letting unhandled exceptions propagate (CP-10 / REQ-MCP-1).
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any

__all__ = [
    "EMBEDDER_UNAVAILABLE",
    "INDEX_NOT_READY",
    "INTERNAL_ERROR",
    "INVALID_ARGUMENT",
    "SYMBOL_NOT_FOUND",
    "TIMEOUT",
    "McpError",
    "error_envelope",
]

# ---------------------------------------------------------------------------
# Error code constants
# ---------------------------------------------------------------------------

SYMBOL_NOT_FOUND = "SYMBOL_NOT_FOUND"
INDEX_NOT_READY = "INDEX_NOT_READY"
TIMEOUT = "TIMEOUT"
EMBEDDER_UNAVAILABLE = "EMBEDDER_UNAVAILABLE"
INVALID_ARGUMENT = "INVALID_ARGUMENT"
INTERNAL_ERROR = "INTERNAL_ERROR"

# Codes that are considered retryable by default.
_RETRYABLE_CODES = {TIMEOUT, INDEX_NOT_READY}


# ---------------------------------------------------------------------------
# McpError — typed exception for tool handlers
# ---------------------------------------------------------------------------


@dataclass
class McpError(Exception):
    """Typed error raised inside tool implementations.

    Tool handlers ``except McpError`` and convert to the error envelope dict.
    All other exceptions are caught at the tool boundary and converted to
    ``INTERNAL_ERROR`` envelopes (CP-10: no unhandled exception escapes).

    Attributes:
        code: One of the ``*`` constants in this module (e.g. ``TIMEOUT``).
        message: Human-readable description.
        retryable: Whether the caller should retry the request.  Defaults
            to ``True`` for timeout/index-not-ready codes, ``False`` otherwise.
    """

    code: str
    message: str
    retryable: bool | None = None

    def __post_init__(self) -> None:
        if self.retryable is None:
            self.retryable = self.code in _RETRYABLE_CODES
        # Ensure Exception.__init__ is called with a meaningful message.
        super().__init__(self.message)

    def to_envelope(self) -> dict[str, Any]:
        """Return the standard error envelope dict."""
        return error_envelope(self.code, self.message, bool(self.retryable))


# ---------------------------------------------------------------------------
# Envelope builder
# ---------------------------------------------------------------------------


def error_envelope(code: str, message: str, retryable: bool = False) -> dict[str, Any]:
    """Build the standard MCP error envelope.

    Args:
        code: Machine-readable error code (e.g. ``"SYMBOL_NOT_FOUND"``).
        message: Human-readable explanation.
        retryable: Whether the caller should retry the request.

    Returns:
        ``{"error": {"code": ..., "message": ..., "retryable": ...}}``
    """
    return {"error": {"code": code, "message": message, "retryable": retryable}}
