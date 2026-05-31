"""Base protocol and data model for all language parsers.

Defines:
- :class:`ParsedSymbol` — intermediate representation produced by each parser.
- :class:`LanguageParser` — structural-subtyping protocol; any object with
  ``language: str`` and ``parse(source, file_path) -> list[ParsedSymbol]``
  satisfies it.

Design notes (from design.md *Indexer Pipeline → Parser*):

- ``id`` follows the convention ``<lang>:<file_path>:<qualified_name>@<short_hash>``
  where ``<short_hash>`` is ``sha256(normalized_body)[:16]``.
- ``content_hash`` is over the **normalized** AST text (whitespace + comments
  stripped) so cosmetic edits do NOT churn IDs (CP-1, CP-2).
- ``body_excerpt`` is truncated to 1 500 chars as per design.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Protocol, runtime_checkable

from cognis.models import SymbolKind


@dataclass
class ParsedSymbol:
    """Intermediate symbol representation produced by a language parser.

    This is the output of the *Parser* stage of the indexer pipeline.  The
    *Writer* stage converts these into :class:`cognis.models.SymbolNode` rows
    after the Resolver and Enricher stages have run.
    """

    # --- stable identity fields ---
    id: str
    """``<lang>:<file_path>:<qualified_name>@<short_hash>``"""

    kind: SymbolKind
    name: str
    qualified_name: str
    language: str
    module: str
    """Nearest ancestor module path (e.g. ``src/auth/jwt``). Repo-relative, forward slashes."""

    file_path: str
    """Repo-relative path with forward slashes."""

    line_start: int
    line_end: int

    # --- optional enrichment fields ---
    signature: str | None = None
    docstring: str | None = None

    content_hash: str = ""
    """``sha256(normalize(body_text))[:16]``."""

    body_excerpt: str | None = None
    """Raw body text truncated to 1 500 chars; used by the embedder."""

    # Runtime defaults use field() to avoid mutable default pitfalls
    untrusted_flags: list[str] = field(default_factory=list)

    @property
    def line_range(self) -> tuple[int, int]:
        """Convenience accessor ``(line_start, line_end)``."""
        return (self.line_start, self.line_end)


@runtime_checkable
class LanguageParser(Protocol):
    """Structural protocol for all language parsers.

    Any class with a ``language`` attribute and a ``parse`` method satisfies
    this protocol — no inheritance required.
    """

    language: str
    """Lower-case language identifier, e.g. ``"typescript"``, ``"python"``, ``"go"``."""

    def parse(self, source: str, file_path: str) -> list[ParsedSymbol]:
        """Parse *source* text for *file_path* and return extracted symbols.

        Args:
            source: Full UTF-8 source text of the file.
            file_path: Repo-relative path (forward slashes) used in symbol IDs
                and ``ParsedSymbol.file_path``.

        Returns:
            List of :class:`ParsedSymbol` instances.  Empty list on parse
            failure (never raises).
        """
        ...
