"""Enricher — ties together attribute extraction and secret redaction.

The :class:`Enricher` is the *Enricher* stage of the cognis indexer pipeline
(design.md *Indexer Pipeline*).  It receives a :class:`~cognis_indexer.parsers.base.ParsedSymbol`
from the Resolver stage and returns an :class:`EnrichedSymbol` with:

1. ``attributes`` — :class:`~cognis.models.SymbolAttribute` rows for
   ``db_table``, ``http_route``, ``env_var``, ``external_call``.
2. ``untrusted_flags`` — taint reasons including ``"secret_redacted"`` (when
   any secret was found) and ``"untrusted_doc"`` (when a docstring is present).
3. Redacted ``body_excerpt``, ``signature``, and ``docstring`` — originals are
   never persisted (design.md NFR Security, CP-7).

Usage::

    enricher = Enricher()
    enriched = enricher.enrich(parsed_symbol)
    # enriched.symbol has secrets redacted in body_excerpt/signature/docstring
    # enriched.attributes contains extracted db_table, http_route, etc.
    # enriched.untrusted_flags contains taint reasons

Design reference: design.md *Indexer Pipeline → Enricher*.
"""

from __future__ import annotations

import copy
import re
from dataclasses import dataclass, field

from cognis.models import SymbolAttribute

from cognis_indexer.enricher.attributes import AttributeExtractor, ExtractedAttribute
from cognis_indexer.enricher.secrets import SecretDetector
from cognis_indexer.parsers.base import ParsedSymbol

# Prompt-injection markers (docs/security.md).
_PROMPT_INJECTION_RE = re.compile(
    r"(ignore previous|disregard above|you are now)",
    re.IGNORECASE,
)

# ---------------------------------------------------------------------------
# EnrichedSymbol
# ---------------------------------------------------------------------------


@dataclass
class EnrichedSymbol:
    """Result of running the :class:`Enricher` over a :class:`ParsedSymbol`.

    The :attr:`symbol` field contains a *copy* of the original symbol with any
    secret-shaped strings in ``body_excerpt``, ``signature``, and ``docstring``
    replaced by ``[REDACTED:<type>]``.  The original is discarded — it is never
    accessible through this object.

    Attributes:
        symbol: A (potentially redacted) copy of the parsed symbol.  Mutation
            of ``untrusted_flags`` on this copy is safe; it does not affect the
            original.
        attributes: Extracted side-effect / contract attributes as
            :class:`~cognis.models.SymbolAttribute` instances ready for DB
            insertion.
        untrusted_flags: List of taint reasons.  Canonical values:

            - ``"secret_redacted"`` — at least one secret was detected and
              replaced in one or more fields.
            - ``"untrusted_doc"`` — the symbol has a non-empty docstring;
              consumers should wrap its content in ``<<<UNTRUSTED>>>`` markers
              (design.md Error Handling → Untrusted content handling).
    """

    symbol: ParsedSymbol
    """Redacted symbol (originals never stored)."""

    attributes: list[SymbolAttribute]
    """Extracted db_table / http_route / env_var / external_call rows."""

    untrusted_flags: list[str] = field(default_factory=list)
    """Taint reasons accumulator."""


# ---------------------------------------------------------------------------
# Enricher
# ---------------------------------------------------------------------------


class Enricher:
    """Orchestrate attribute extraction and secret redaction.

    This class is stateless once constructed.  A single instance may be safely
    shared across threads.

    Args:
        attribute_extractor: Optional custom :class:`~cognis_indexer.enricher.attributes.AttributeExtractor`.
            Defaults to a freshly constructed instance.
        secret_detector: Optional custom :class:`~cognis_indexer.enricher.secrets.SecretDetector`.
            Defaults to a freshly constructed instance.
    """

    def __init__(
        self,
        attribute_extractor: AttributeExtractor | None = None,
        secret_detector: SecretDetector | None = None,
    ) -> None:
        self._attr_extractor = attribute_extractor or AttributeExtractor()
        self._secret_detector = secret_detector or SecretDetector()

    def enrich(self, symbol: ParsedSymbol) -> EnrichedSymbol:
        """Enrich *symbol* and return an :class:`EnrichedSymbol`.

        Steps performed (in order):

        1. **Attribute extraction** — run :class:`~cognis_indexer.enricher.attributes.AttributeExtractor`
           on ``symbol.body_excerpt`` and build :class:`~cognis.models.SymbolAttribute` rows.
        2. **Secret redaction** — run :class:`~cognis_indexer.enricher.secrets.SecretDetector`
           on ``body_excerpt``, ``signature``, and ``docstring`` (independently).
           Replace secrets with ``[REDACTED:<type>]``.  If any secrets found,
           add ``"secret_redacted"`` to ``untrusted_flags``.
        3. **Untrusted-doc tagging** — if ``docstring`` is non-empty (after any
           redaction), add ``"untrusted_doc"`` to ``untrusted_flags``.
        4. Return :class:`EnrichedSymbol` with the redacted symbol copy,
           attributes, and untrusted flags.

        The *original* ``symbol`` is never mutated.  The returned
        :class:`EnrichedSymbol` holds a deep-copied :class:`ParsedSymbol` whose
        ``body_excerpt``, ``signature``, ``docstring``, and
        ``untrusted_flags`` may differ from the original.

        Args:
            symbol: The :class:`~cognis_indexer.parsers.base.ParsedSymbol` to
                enrich.

        Returns:
            An :class:`EnrichedSymbol` instance.
        """
        # Deep-copy so we never mutate the caller's symbol.
        sym = copy.deepcopy(symbol)

        untrusted_flags: list[str] = list(sym.untrusted_flags)
        any_secret_found = False

        # ------------------------------------------------------------------
        # Step 1: Extract attributes from body_excerpt
        # ------------------------------------------------------------------
        raw_attrs: list[ExtractedAttribute] = self._attr_extractor.extract(sym.body_excerpt or "")
        attributes: list[SymbolAttribute] = [
            SymbolAttribute(symbol_id=sym.id, key=attr.key, value=attr.value)  # type: ignore[arg-type]
            for attr in raw_attrs
        ]

        # ------------------------------------------------------------------
        # Step 2: Redact secrets in body_excerpt, signature, docstring
        # ------------------------------------------------------------------
        if sym.body_excerpt:
            redacted_body, types_body = self._secret_detector.redact(sym.body_excerpt)
            sym.body_excerpt = redacted_body
            if types_body:
                any_secret_found = True

        if sym.signature:
            redacted_sig, types_sig = self._secret_detector.redact(sym.signature)
            sym.signature = redacted_sig
            if types_sig:
                any_secret_found = True

        if sym.docstring:
            redacted_doc, types_doc = self._secret_detector.redact(sym.docstring)
            sym.docstring = redacted_doc
            if types_doc:
                any_secret_found = True

        if any_secret_found and "secret_redacted" not in untrusted_flags:
            untrusted_flags.append("secret_redacted")

        # ------------------------------------------------------------------
        # Step 3: Untrusted-doc tagging (task 9.4)
        # ------------------------------------------------------------------
        # Any non-empty docstring (after redaction) is considered untrusted
        # content — it may contain user-controlled text (prompt injection).
        if sym.docstring and sym.docstring.strip() and "untrusted_doc" not in untrusted_flags:
            untrusted_flags.append("untrusted_doc")

        # ------------------------------------------------------------------
        # Step 4: High-risk prompt-injection pattern tagging
        # ------------------------------------------------------------------
        for field_text in (sym.body_excerpt, sym.docstring, sym.signature):
            if field_text and _PROMPT_INJECTION_RE.search(field_text):
                if "prompt_injection_high" not in untrusted_flags:
                    untrusted_flags.append("prompt_injection_high")
                break

        sym.untrusted_flags = untrusted_flags

        return EnrichedSymbol(
            symbol=sym,
            attributes=attributes,
            untrusted_flags=untrusted_flags,
        )
