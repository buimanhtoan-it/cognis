"""AST text normalizer and content hash utilities.

The normalizer is the foundation of CP-1 (index idempotency) and CP-2 (symbol
id stability under cosmetic edits).

Design contract:

- Cosmetic edits — whitespace-only or comment-only changes — MUST produce the
  same ``content_hash``.
- Structural edits — rename, signature change, body change — MUST produce a
  different ``content_hash``.

Implementation strategy:

1. Remove single-line comments (``//``, ``#``).
2. Remove multi-line / block comments (``/* … */``, triple-quoted docstrings,
   Go doc comments).
3. Collapse all remaining whitespace sequences (spaces, tabs, newlines) to a
   single space.
4. Strip leading/trailing space.
5. SHA-256 the result; return the first 16 hex chars.

This approach is language-agnostic and intentionally simple.  It operates on
*raw text*, not on tree-sitter AST nodes, so it can also be used in tests
without a live tree-sitter installation.

The trade-off: over-zealous stripping of string literals that look like
comments is acceptable for the purpose of stable hashing — it errs on the side
of fewer spurious re-index events.
"""

from __future__ import annotations

import hashlib
import re

# ---------------------------------------------------------------------------
# Comment stripping patterns
# ---------------------------------------------------------------------------

# Single-line comments: // … (TypeScript/Go) and # … (Python)
_SINGLE_LINE_COMMENT = re.compile(r"(//|#)[^\n]*")

# Block comments: /* … */  (TypeScript/Go)
_BLOCK_COMMENT = re.compile(r"/\*.*?\*/", re.DOTALL)

# Python triple-quoted strings used as docstrings.
# We strip them when they appear as standalone expression statements (first
# non-whitespace on a line), which covers the docstring pattern.
# This is *not* a full Python parser; it handles the common case.
_TRIPLE_DOUBLE = re.compile(r'""".*?"""', re.DOTALL)
_TRIPLE_SINGLE = re.compile(r"'''.*?'''", re.DOTALL)

# Collapse whitespace
_WHITESPACE = re.compile(r"\s+")


def normalize_body(text: str) -> str:
    """Return a whitespace- and comment-stripped version of *text*.

    Suitable for hashing to detect structural (non-cosmetic) changes.
    """
    # Strip block comments first (they can span lines)
    text = _BLOCK_COMMENT.sub(" ", text)
    # Strip Python triple-quoted docstrings
    text = _TRIPLE_DOUBLE.sub(" ", text)
    text = _TRIPLE_SINGLE.sub(" ", text)
    # Strip single-line comments
    text = _SINGLE_LINE_COMMENT.sub(" ", text)
    # Collapse whitespace
    text = _WHITESPACE.sub(" ", text)
    return text.strip()


def content_hash(body_text: str) -> str:
    """Return ``sha256(normalize(body_text))[:16]``.

    This is the ``short_hash`` component of a symbol's stable ID per the design
    *ID conventions* section::

        symbol.id = "<lang>:<file_path>:<qualified_name>@<short_hash>"
        content_hash = sha256(normalized_body)[:16]
    """
    normalized = normalize_body(body_text)
    digest = hashlib.sha256(normalized.encode("utf-8")).hexdigest()
    return digest[:16]


def make_symbol_id(lang: str, file_path: str, qualified_name: str, body_text: str) -> str:
    """Construct the stable symbol ID per design conventions.

    Args:
        lang: Language prefix, e.g. ``"ts"``, ``"py"``, ``"go"``.
        file_path: Repo-relative path with forward slashes.
        qualified_name: Fully qualified name, e.g. ``"src/auth/jwt.validate"``.
        body_text: Raw body text of the symbol (before normalization).

    Returns:
        ``"<lang>:<file_path>:<qualified_name>@<short_hash>"``
    """
    short_hash = content_hash(body_text)
    return f"{lang}:{file_path}:{qualified_name}@{short_hash}"
