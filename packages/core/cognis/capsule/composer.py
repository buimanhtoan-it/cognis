"""Capsule Composer — Task 14.2, 14.4 of ``.kiro/specs/cognis/tasks.md``.

Implements the composition pipeline that takes retrieval hits from the three
MVP layers (lexical, semantic, structural), merges them, hydrates symbol rows
from the DB, fills per-mode sections, attaches source entries, wraps untrusted
content, and enforces the token budget.

Design reference
----------------
- Context Capsule schema (v1, MVP) and Composition rules — design.md §Data Models.
- Cognitive Context Planner pipeline — design.md §Components and Interfaces.
- Error Handling → Untrusted content handling — design.md.

Correctness properties
-----------------------
CP-8: ``token_estimate ≤ max_tokens`` for any ``max_tokens ∈ [500, 32000]``.
CP-9: ``sources[]`` non-empty for every populated section.
CP-11: Same query + same DB state → same capsule (modulo wall-clock fields).
"""

from __future__ import annotations

import json
import logging
from typing import TYPE_CHECKING, Any

from cognis.capsule.models import (
    CapsuleSource,
    CompressedContext,
    ContextCapsule,
    RelevantSymbol,
    RiskArea,
    RootCauseCandidate,
)
from cognis.capsule.token_estimator import estimate_capsule_tokens
from cognis.db import Database, get_symbol
from cognis.planner import TaskMode

if TYPE_CHECKING:
    from cognis_retrieval.base import Hit

logger = logging.getLogger(__name__)

# ---------------------------------------------------------------------------
# Untrusted content markers (design §Error Handling → Untrusted content)
# ---------------------------------------------------------------------------

_UNTRUSTED_OPEN = '<<<UNTRUSTED type="{kind}" symbol="{symbol}">>>'
_UNTRUSTED_CLOSE = "<<<END UNTRUSTED>>>"


def _wrap_untrusted(text: str, kind: str, symbol: str) -> str:
    """Wrap *text* with the ``<<<UNTRUSTED ...>>>`` marker pair.

    Args:
        text: The raw content to wrap.
        kind: Content kind (e.g. ``"docstring"``, ``"comment"``).
        symbol: Qualified name or symbol_id of the originating symbol.

    Returns:
        The wrapped string ready for inclusion in the capsule.
    """
    open_tag = _UNTRUSTED_OPEN.format(kind=kind, symbol=symbol)
    return f"{open_tag}\n{text}\n{_UNTRUSTED_CLOSE}"


# ---------------------------------------------------------------------------
# ComposeError
# ---------------------------------------------------------------------------


class ComposeError(Exception):
    """Raised when the capsule composition pipeline detects a fatal violation.

    Currently the only fatal case is a populated section (root_cause_candidates
    or relevant_symbols) that has no backing source entry — per the design's
    "Sources mandatory" composition rule.
    """


# ---------------------------------------------------------------------------
# Internal helpers
# ---------------------------------------------------------------------------


def _dedupe_hits(hits: list[Hit]) -> list[Hit]:
    """Deduplicate hits by ``symbol_id``, keeping the highest score per symbol.

    The output is sorted by descending score (deterministic tie-breaking by
    symbol_id ensures CP-11 determinism).

    Args:
        hits: Raw hits from one or more retrieval layers.

    Returns:
        Deduplicated, score-sorted list of hits.
    """
    best: dict[str, Hit] = {}
    for hit in hits:
        existing = best.get(hit.symbol_id)
        if existing is None or hit.score > existing.score:
            best[hit.symbol_id] = hit
    # Stable sort: descending score, then ascending symbol_id for tie-break.
    return sorted(best.values(), key=lambda h: (-h.score, h.symbol_id))


def _make_source(symbol_id: str) -> CapsuleSource:
    """Create a ``CapsuleSource`` of type ``"symbol"`` for *symbol_id*."""
    return CapsuleSource(type="symbol", id=symbol_id, uri=None)


def _is_untrusted(untrusted_flags: list[str]) -> bool:
    """Return True if the symbol's flags indicate untrusted document content."""
    return "untrusted_doc" in untrusted_flags


# ---------------------------------------------------------------------------
# CapsuleComposer
# ---------------------------------------------------------------------------


class CapsuleComposer:
    """Compose a :class:`~cognis.capsule.models.ContextCapsule` from retrieval hits.

    The composer is **stateless** — all state is passed in via :meth:`compose`.
    This makes it straightforward to test and keeps CP-11 (determinism) easy
    to satisfy: given the same inputs, the pipeline always produces the same
    output.

    Usage
    -----
    .. code-block:: python

        composer = CapsuleComposer()
        capsule = composer.compose(
            task="Why is /login timing out?",
            mode="bugfix",
            confidence=0.85,
            hits=hits,
            max_tokens=8000,
            db=db,
        )
    """

    # ------------------------------------------------------------------
    # Public interface
    # ------------------------------------------------------------------

    def compose(
        self,
        task: str,
        mode: TaskMode,
        confidence: float,
        hits: list[Hit],
        max_tokens: int,
        db: Database,
        include_runtime: bool = False,
    ) -> ContextCapsule:
        """Run the full composition pipeline.

        Pipeline steps (design §Cognitive Context Planner):

        1. Score-merge hits: deduplicate by ``symbol_id``, keep highest score.
        2. Sort by score descending (tie-break by symbol_id for determinism).
        3. Hydrate symbol rows from the DB for top-N deduplicated hits.
        4. Fill sections based on *mode*:
           - ``bugfix`` → ``root_cause_candidates`` from structural hits;
             ``relevant_symbols`` from semantic + lexical.
           - all other modes → ``relevant_symbols`` with appropriate scoring.
        5. Attach sources: every entry in ``root_cause_candidates`` and
           ``relevant_symbols`` must have a backing ``CapsuleSource``.
        6. Untrusted wrapping: symbols with ``"untrusted_doc"`` in
           ``untrusted_flags`` get their snippet/docstring wrapped in
           ``<<<UNTRUSTED ...>>>`` markers; section id added to
           ``untrusted_sections``.
        7. Estimate tokens (tiktoken cl100k_base + 10% margin).
        8. Drop sections (not truncate) to fit within *max_tokens* budget.
        9. Final validation: every populated section must have ≥ 1 source;
           raise :class:`ComposeError` on violation.

        Args:
            task: The original user task string (stored as ``goal``).
            mode: Task mode from the planner classifier.
            confidence: Planner classifier confidence (0.0-1.0).
            hits: Raw retrieval hits from all layers (may be duplicated
                across layers; composer deduplicates).
            max_tokens: Hard upper bound on ``token_estimate``.
            db: Database handle for hydrating symbol rows.
            include_runtime: Include ``runtime_evidence`` section if hits
                provide runtime signals (Phase 3; currently always empty).

        Returns:
            A validated :class:`~cognis.capsule.models.ContextCapsule`.

        Raises:
            ComposeError: If any populated section lacks a source entry.
        """
        # Step 1+2: deduplicate and sort hits.
        deduped = _dedupe_hits(hits)

        # Step 3: hydrate symbol rows.  We limit to the top-100 hits to avoid
        # runaway DB queries; the token budget will further trim sections later.
        top_hits = deduped[:100]
        symbol_rows = self._hydrate_symbols(top_hits, db)

        # Step 4+5+6: fill sections, attach sources, wrap untrusted content.
        sources: list[CapsuleSource] = []
        untrusted_sections: list[str] = []

        root_cause_candidates: list[RootCauseCandidate] = []
        relevant_symbols: list[RelevantSymbol] = []
        risk_areas: list[RiskArea] = []
        compressed_context: list[CompressedContext] = []

        if mode == "bugfix":
            root_cause_candidates, relevant_symbols = self._fill_bugfix_sections(
                top_hits, symbol_rows, sources, untrusted_sections
            )
        else:
            relevant_symbols = self._fill_generic_sections(
                top_hits, symbol_rows, sources, untrusted_sections
            )

        # Risk areas: symbols with risk_score > 0 (from hydrated rows).
        for hit in top_hits:
            sym = symbol_rows.get(hit.symbol_id)
            if sym is not None and sym.risk_score > 0.0:
                risk_areas.append(
                    RiskArea(
                        symbol_id=hit.symbol_id,
                        reason=f"risk_score={sym.risk_score:.2f}",
                    )
                )

        # Step 7: assemble a draft capsule and estimate tokens.
        draft = ContextCapsule(
            version="1",
            goal=task,
            task_mode=mode,
            confidence=confidence,
            root_cause_candidates=root_cause_candidates,
            relevant_symbols=relevant_symbols,
            call_chain=[],
            runtime_evidence=[],
            neighbor_patterns=[],
            risk_areas=risk_areas,
            compressed_context=compressed_context,
            token_estimate=0,  # placeholder; computed below
            sources=sources,
            untrusted_sections=sorted(set(untrusted_sections)),
        )

        # Step 8: drop sections to fit within max_tokens budget.
        draft = self._enforce_budget(draft, max_tokens)

        # Step 9: validate sources completeness.
        self._validate_sources(draft)

        return draft

    # ------------------------------------------------------------------
    # Section filling helpers
    # ------------------------------------------------------------------

    def _hydrate_symbols(
        self,
        hits: list[Hit],
        db: Database,
    ) -> dict[str, Any]:
        """Fetch symbol rows from the DB for every hit.

        Missing rows (symbol was deleted after indexing) are silently skipped —
        the hit will still appear in the capsule but with minimal metadata.

        Returns:
            ``{symbol_id: SymbolNode}`` mapping (may be a subset of *hits*).
        """
        result: dict[str, Any] = {}
        for hit in hits:
            sym = get_symbol(db, hit.symbol_id)
            if sym is not None:
                result[hit.symbol_id] = sym
        return result

    def _build_relevant_symbol(
        self,
        hit: Hit,
        symbol_rows: dict[str, Any],
        untrusted_sections: list[str],
        section_id: str,
    ) -> RelevantSymbol:
        """Build a :class:`RelevantSymbol` for a single hit.

        Applies the untrusted-content wrapping rule:  if the symbol has
        ``"untrusted_doc"`` in its ``untrusted_flags``, the snippet is wrapped
        with ``<<<UNTRUSTED>>>`` markers and *section_id* is appended to
        *untrusted_sections*.

        Args:
            hit: The retrieval hit.
            symbol_rows: Hydrated symbol rows from the DB.
            untrusted_sections: Mutable list to which the section id is
                appended if the content is untrusted.
            section_id: The capsule section id (e.g. ``"relevant_symbols"``).

        Returns:
            A :class:`RelevantSymbol` instance.
        """
        sym = symbol_rows.get(hit.symbol_id)
        kind = sym.kind if sym is not None else "unknown"

        snippet: str | None = None
        if sym is not None:
            raw_snippet = sym.body_excerpt or sym.docstring
            if raw_snippet:
                if _is_untrusted(list(sym.untrusted_flags)):
                    snippet = _wrap_untrusted(raw_snippet, "docstring", sym.qualified_name)
                    if section_id not in untrusted_sections:
                        untrusted_sections.append(section_id)
                else:
                    snippet = raw_snippet

        return RelevantSymbol(
            symbol_id=hit.symbol_id,
            kind=kind,
            snippet=snippet,
            summary=sym.semantic_summary if sym is not None else None,
            score=hit.score,
        )

    def _fill_bugfix_sections(
        self,
        hits: list[Hit],
        symbol_rows: dict[str, Any],
        sources: list[CapsuleSource],
        untrusted_sections: list[str],
    ) -> tuple[list[RootCauseCandidate], list[RelevantSymbol]]:
        """Fill ``root_cause_candidates`` and ``relevant_symbols`` for bugfix mode.

        Bugfix strategy (design §Cognitive Context Planner layer plan table):
        - ``root_cause_candidates``: top structural hits (layer == "structural"),
          sorted by score descending.  Max 5.
        - ``relevant_symbols``: remaining hits (semantic + lexical), up to 20.
        """
        structural_hits = [h for h in hits if h.layer == "structural"]
        other_hits = [h for h in hits if h.layer != "structural"]

        root_causes: list[RootCauseCandidate] = []
        relevant: list[RelevantSymbol] = []

        # Root cause candidates from structural hits (up to 5).
        for hit in structural_hits[:5]:
            sym = symbol_rows.get(hit.symbol_id)
            evidence: list[str] = []
            if hit.evidence:
                ev_str = json.dumps(hit.evidence, sort_keys=True)
                evidence.append(ev_str)
            rationale = hit.reason or f"structural relevance (score={hit.score:.3f})"

            # Untrusted rationale wrapping.
            if sym is not None and _is_untrusted(list(sym.untrusted_flags)):
                rationale = _wrap_untrusted(rationale, "rationale", sym.qualified_name)
                if "root_cause_candidates" not in untrusted_sections:
                    untrusted_sections.append("root_cause_candidates")

            root_causes.append(
                RootCauseCandidate(
                    symbol_id=hit.symbol_id,
                    rationale=rationale,
                    evidence=evidence,
                )
            )
            sources.append(_make_source(hit.symbol_id))

        # Relevant symbols from non-structural hits (up to 20).
        for hit in other_hits[:20]:
            rs = self._build_relevant_symbol(
                hit, symbol_rows, untrusted_sections, "relevant_symbols"
            )
            relevant.append(rs)
            sources.append(_make_source(hit.symbol_id))

        return root_causes, relevant

    def _fill_generic_sections(
        self,
        hits: list[Hit],
        symbol_rows: dict[str, Any],
        sources: list[CapsuleSource],
        untrusted_sections: list[str],
    ) -> list[RelevantSymbol]:
        """Fill ``relevant_symbols`` for all non-bugfix modes.

        Takes up to 25 hits, builds :class:`RelevantSymbol` entries with
        source attachments.
        """
        relevant: list[RelevantSymbol] = []
        for hit in hits[:25]:
            rs = self._build_relevant_symbol(
                hit, symbol_rows, untrusted_sections, "relevant_symbols"
            )
            relevant.append(rs)
            sources.append(_make_source(hit.symbol_id))
        return relevant

    # ------------------------------------------------------------------
    # Budget enforcement (CP-8)
    # ------------------------------------------------------------------

    def _enforce_budget(self, capsule: ContextCapsule, max_tokens: int) -> ContextCapsule:
        """Drop sections until ``token_estimate ≤ max_tokens`` (CP-8).

        The design mandates "drop sections (not truncate)" so we remove entire
        sections in priority order (least important first) until the estimate
        fits.  Wall-clock fields (``generated_at`` if present) are excluded
        from the hash comparison in the determinism test (CP-11) but are not
        present in the v1 schema at MVP.

        Section drop priority (lowest value = dropped first):

        1. ``neighbor_patterns``
        2. ``compressed_context``
        3. ``risk_areas``
        4. ``runtime_evidence``
        5. ``relevant_symbols`` (trimmed from the end, not entirely dropped)
        6. ``root_cause_candidates`` (trimmed from the end)

        Args:
            capsule: Draft capsule (``token_estimate`` may be 0 placeholder).
            max_tokens: Hard upper bound.

        Returns:
            A new capsule instance with ``token_estimate`` set and sections
            potentially trimmed to fit.
        """
        # Section drop order: cheapest to lose first.
        _DROP_ORDER = [
            "neighbor_patterns",
            "compressed_context",
            "risk_areas",
            "runtime_evidence",
        ]

        # Build a mutable dict so we can iteratively drop sections.
        fields: dict[str, Any] = capsule.model_dump(by_alias=True)

        def _current_estimate(f: dict[str, Any]) -> int:
            tmp = ContextCapsule(
                **{k: v for k, v in f.items() if k not in ("token_estimate",)}, token_estimate=0
            )
            return estimate_capsule_tokens(tmp)

        estimate = _current_estimate(fields)

        # Phase 1: drop whole sections in priority order.
        for section in _DROP_ORDER:
            if estimate <= max_tokens:
                break
            if fields.get(section):
                fields[section] = []
                estimate = _current_estimate(fields)

        # Phase 2: trim relevant_symbols from the end (one-at-a-time).
        while estimate > max_tokens and fields.get("relevant_symbols"):
            fields["relevant_symbols"] = fields["relevant_symbols"][:-1]
            estimate = _current_estimate(fields)

        # Phase 3: trim root_cause_candidates from the end.
        while estimate > max_tokens and fields.get("root_cause_candidates"):
            fields["root_cause_candidates"] = fields["root_cause_candidates"][:-1]
            estimate = _current_estimate(fields)

        # After trimming, recalculate the final sources list to only include
        # sources for symbols that still appear in the capsule.
        remaining_symbol_ids: set[str] = set()
        for rs in fields.get("relevant_symbols", []):
            sid: str | None = rs.get("symbol_id") if isinstance(rs, dict) else rs.symbol_id
            if sid is not None:
                remaining_symbol_ids.add(sid)
        for rcc in fields.get("root_cause_candidates", []):
            sid2: str | None = rcc.get("symbol_id") if isinstance(rcc, dict) else rcc.symbol_id
            if sid2 is not None:
                remaining_symbol_ids.add(sid2)

        # Filter sources to only those backing remaining symbols.
        filtered_sources = []
        seen_source_ids: set[str] = set()
        for src in fields.get("sources", []):
            src_id: str | None = src.get("id") if isinstance(src, dict) else src.id
            if (
                src_id is not None
                and src_id in remaining_symbol_ids
                and src_id not in seen_source_ids
            ):
                filtered_sources.append(src)
                seen_source_ids.add(src_id)
        fields["sources"] = filtered_sources

        # Also trim untrusted_sections to only those relevant to surviving content.
        # (simplified: keep all declared untrusted sections; they reference section
        # names not individual symbols, so we keep them as-is)

        # Set the final token estimate.
        fields["token_estimate"] = estimate

        # Reconstruct the capsule with corrected fields.
        return ContextCapsule(**{k: v for k, v in fields.items()})

    # ------------------------------------------------------------------
    # Source validation (CP-9 / "Sources mandatory")
    # ------------------------------------------------------------------

    def _validate_sources(self, capsule: ContextCapsule) -> None:
        """Verify every populated section has at least one ``sources[]`` entry.

        Design: "Sources mandatory. Every claim has a sources[] entry. Compose
        fails if violation detected."

        Args:
            capsule: The composed capsule to validate.

        Raises:
            ComposeError: If ``root_cause_candidates`` or ``relevant_symbols``
                are populated but ``sources`` is empty.
        """
        has_claims = bool(capsule.root_cause_candidates or capsule.relevant_symbols)
        if has_claims and not capsule.sources:
            raise ComposeError(
                "Capsule has populated sections (root_cause_candidates / relevant_symbols) "
                "but sources[] is empty. Composition rule 1 requires every claim to have "
                "at least one source entry."
            )

        # Per-symbol source check: every symbol_id in claims must have a source.
        sourced_ids = {s.id for s in capsule.sources}
        for rcc in capsule.root_cause_candidates:
            if rcc.symbol_id not in sourced_ids:
                raise ComposeError(
                    f"root_cause_candidate symbol '{rcc.symbol_id}' has no source entry. "
                    "Compose fails: sources mandatory per design composition rule 1."
                )
        for rs in capsule.relevant_symbols:
            if rs.symbol_id not in sourced_ids:
                raise ComposeError(
                    f"relevant_symbol '{rs.symbol_id}' has no source entry. "
                    "Compose fails: sources mandatory per design composition rule 1."
                )


__all__ = [
    "CapsuleComposer",
    "ComposeError",
]
