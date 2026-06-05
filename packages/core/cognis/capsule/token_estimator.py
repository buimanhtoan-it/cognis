"""Token estimation utilities for Context Capsule v1.

Implements Task 14.3 of ``.kiro/specs/cognis/tasks.md``.

Design reference
----------------
"Token estimate honest: use ``tiktoken`` cl100k_base + 10% safety margin.
Defer Claude tokenizer to Phase 2."

Two public functions:

- :func:`estimate_tokens` — estimate token count for any text string.
- :func:`estimate_capsule_tokens` — estimate the total token count of a
  :class:`~cognis.capsule.models.ContextCapsule` (serialise to JSON first).

Fallback
--------
If ``tiktoken`` is not installed (it's in the ``tokenizers`` optional extra),
we fall back to ``len(text.split()) * 1.3`` as an approximation.  The
``TIKTOKEN_AVAILABLE`` module-level flag signals which path was taken; tests
may check it but MUST NOT skip on the fallback — the fallback is a valid
implementation.
"""

from __future__ import annotations

import logging
from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:
    from cognis.capsule.models import ContextCapsule

logger = logging.getLogger(__name__)

# ---------------------------------------------------------------------------
# tiktoken availability probe
# ---------------------------------------------------------------------------

# Typed as ``Any``: tiktoken ships no type stubs, so the encoding object is
# opaque to mypy. ``Any`` lets us call ``.encode(...)`` without an
# ``attr-defined`` error and without a per-line ignore that mypy would flag as
# unused on environments where tiktoken *is* importable.
_TIKTOKEN_ENCODING: Any = None
TIKTOKEN_AVAILABLE: bool = False

try:
    import tiktoken as _tiktoken

    _TIKTOKEN_ENCODING = _tiktoken.get_encoding("cl100k_base")
    TIKTOKEN_AVAILABLE = True
except Exception:
    # ImportError if not installed; any other error (network, corrupt cache)
    # should also fall back gracefully.
    logger.debug("tiktoken not available; using word-count fallback for token estimation")

# ---------------------------------------------------------------------------
# Safety margin
# ---------------------------------------------------------------------------

SAFETY_MARGIN: float = 0.10
"""Add 10% on top of the raw token estimate to account for serialisation
overhead and tokenizer discrepancies (design §Resolved Open Questions)."""

# ---------------------------------------------------------------------------
# Public API
# ---------------------------------------------------------------------------


def estimate_tokens(text: str) -> int:
    """Estimate the token count of *text* using tiktoken cl100k_base.

    Applies the 10% safety margin defined in the design doc.

    Args:
        text: The string to estimate.

    Returns:
        Estimated token count with a 10% safety margin applied
        (i.e. ``ceil(raw_count * 1.1)``).
    """
    if not text:
        return 0

    raw: int
    if TIKTOKEN_AVAILABLE and _TIKTOKEN_ENCODING is not None:
        # tiktoken's Encoding.encode() returns a list of ints; we need len().
        encode = _TIKTOKEN_ENCODING.encode
        raw = len(encode(text))
    else:
        # Fallback: word count * 1.3 (rough approximation for code + prose).
        raw = int(len(text.split()) * 1.3)

    return int(raw * (1.0 + SAFETY_MARGIN))


def estimate_capsule_tokens(capsule: ContextCapsule) -> int:
    """Estimate the total token count of a complete capsule.

    Serialises the capsule to JSON (the wire format clients receive) and
    runs :func:`estimate_tokens` on the resulting string.

    Args:
        capsule: A :class:`~cognis.capsule.models.ContextCapsule` instance.

    Returns:
        Estimated token count with the 10% safety margin.
    """
    serialised = capsule.model_dump_json(by_alias=True)
    return estimate_tokens(serialised)


def tokens_for_text(text: str | None) -> int:
    """Estimate tokens for an optional text field, returning 0 for None/empty.

    Convenience helper used internally by the composer to budget individual
    symbol snippets and summaries without explicit None checks at every call
    site.

    Args:
        text: Text to estimate, or ``None``.

    Returns:
        Estimated token count (0 for None or empty string).
    """
    if not text:
        return 0
    return estimate_tokens(text)


__all__ = [
    "SAFETY_MARGIN",
    "TIKTOKEN_AVAILABLE",
    "estimate_capsule_tokens",
    "estimate_tokens",
    "tokens_for_text",
]
