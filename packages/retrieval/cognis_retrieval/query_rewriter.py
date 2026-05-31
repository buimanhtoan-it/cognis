"""Query rewriter for the lexical retrieval layer.

Extracts searchable tokens from a natural-language query so FTS5 BM25 can
match against the ``symbol_fts`` index.

Extracted token categories
--------------------------
1. **Identifiers** — ``\\b[A-Za-z_][A-Za-z0-9_]+\\b`` after stop-word filtering.
2. **Error / backtick tokens** — content inside backtick spans or quoted strings.
3. **File-glob hints** — patterns like ``*.ts``, ``src/*.py``.
4. **TODO markers** — the words TODO, FIXME, HACK.

The rewriter returns an FTS5 query string with tokens joined by ``OR``, e.g.::

    "validate OR jwt OR auth OR timeout"

Design reference: *Retrieval Mesh → Lexical* (design.md).
Requirements: REQ-RET-1.
"""

from __future__ import annotations

import re

__all__ = ["rewrite_query"]

# ---------------------------------------------------------------------------
# Stop words (common English words that clutter FTS results)
# ---------------------------------------------------------------------------

_STOP_WORDS: frozenset[str] = frozenset(
    {
        "a",
        "an",
        "and",
        "at",
        "be",
        "by",
        "do",
        "does",
        "for",
        "from",
        "how",
        "in",
        "is",
        "it",
        "of",
        "or",
        "the",
        "to",
        "what",
        "why",
        "with",
        "not",
        "no",
        "on",
        "as",
        "are",
        "was",
        "that",
        "this",
        "its",
        "can",
        "could",
        "will",
        "would",
        "should",
        "may",
        "might",
        "i",
        "me",
        "my",
        "we",
        "our",
        "you",
        "your",
        "he",
        "she",
        "they",
        "them",
        "their",
    }
)

# ---------------------------------------------------------------------------
# Regex patterns
# ---------------------------------------------------------------------------

# Backtick spans: `some_token`
_BACKTICK_RE = re.compile(r"`([^`]+)`")

# Double-quoted strings: "some token"
_DQUOTE_RE = re.compile(r'"([^"]+)"')

# Single-quoted strings: 'some token'
_SQUOTE_RE = re.compile(r"'([^']+)'")

# File-glob hints: *.ts  src/*.py  tests/**/*.go
_GLOB_RE = re.compile(r"\b[\w*/]+\.\w+\b")

# Identifier pattern: starts with letter or _, followed by alphanumeric/_
_IDENT_RE = re.compile(r"\b([A-Za-z_][A-Za-z0-9_]+)\b")

# TODO markers (exact words)
_TODO_RE = re.compile(r"\b(TODO|FIXME|HACK)\b")

# Characters we want to keep in FTS5 queries (alphanumeric + _)
_SAFE_TOKEN_RE = re.compile(r"[A-Za-z0-9_]+")


def _safe_token(text: str) -> str | None:
    """Return the first safe identifier fragment from *text*, or None."""
    m = _SAFE_TOKEN_RE.search(text)
    return m.group(0) if m else None


def rewrite_query(query: str) -> str:
    """Rewrite a natural-language *query* into an FTS5 query string.

    Extraction order:
    1. Backtick spans → treated as high-priority tokens.
    2. Quoted strings → same.
    3. TODO/FIXME/HACK markers.
    4. File-glob hints.
    5. Identifier tokens after stop-word filtering.

    Tokens are deduplicated (order-preserving) and joined with `` OR ``.

    Args:
        query: Free-form natural-language query.

    Returns:
        FTS5 query string such as ``"validate OR jwt OR timeout"`` or an empty
        string if no usable tokens were found.
    """
    seen: set[str] = set()
    tokens: list[str] = []

    def _add(tok: str) -> None:
        """Add *tok* to *tokens* if it's non-empty and not a duplicate."""
        t = tok.strip()
        if t and t.lower() not in seen and t.lower() not in _STOP_WORDS:
            seen.add(t.lower())
            tokens.append(t)

    # 1. Backtick spans
    for m in _BACKTICK_RE.finditer(query):
        content = m.group(1).strip()
        # Backtick content may contain spaces (e.g. `auth.jwt`); split on
        # non-identifier characters and add each part.
        for part in re.split(r"[^A-Za-z0-9_]", content):
            if part:
                _add(part)

    # 2. Quoted strings (double and single)
    # Strip them from query for subsequent steps so identifiers inside aren't
    # double-counted.
    clean_query = query
    for pattern in (_DQUOTE_RE, _SQUOTE_RE, _BACKTICK_RE):
        for m in pattern.finditer(query):
            content = m.group(1).strip()
            for part in re.split(r"[^A-Za-z0-9_]", content):
                if part:
                    _add(part)
        clean_query = pattern.sub(" ", clean_query)

    # 3. TODO markers
    for m in _TODO_RE.finditer(clean_query):
        _add(m.group(1))

    # 4. File-glob hints — e.g. *.ts  src/*.py
    for m in _GLOB_RE.finditer(clean_query):
        tok = m.group(0)
        # Extract the extension and base name as tokens.
        safe = _safe_token(tok)
        if safe:
            _add(safe)
        # Also add the extension without the dot.
        parts = tok.split(".")
        if len(parts) >= 2 and parts[-1]:
            _add(parts[-1])

    # 5. Identifier tokens from remaining query text
    for m in _IDENT_RE.finditer(clean_query):
        ident = m.group(1)
        if ident.upper() not in ("TODO", "FIXME", "HACK"):
            _add(ident)

    if not tokens:
        return ""
    return " OR ".join(tokens)
