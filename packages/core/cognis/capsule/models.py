"""Pydantic v2 models for Context Capsule v1.

These models validate and serialise the capsule schema defined in the design
document's *Context Capsule schema (v1, MVP)* section.  The JSON Schema file
is derived from these models and shipped at
``packages/core/cognis/schemas/capsule.v1.json``.

Design reference
----------------
- Context Capsule schema (v1, MVP) — design.md §Data Models.
- Composition rules:
  1. Sources mandatory — every claim has a ``sources[]`` entry.
  2. Token estimate honest — ``token_estimate ≤ max_tokens``.
  3. No null arrays — empty array preferred so consumers don't branch on null.
  4. Section ordering deterministic — same query + same index state → same capsule.

Correctness property
--------------------
CP-9 (PBT): For any composed capsule, validates against ``capsule.v1.json``,
every populated section has ≥ 1 ``sources[]`` entry, ``token_estimate ≤ max_tokens``.
"""

from __future__ import annotations

from typing import Any, Final, Literal

from pydantic import BaseModel, ConfigDict, Field

# ---------------------------------------------------------------------------
# Shared model config
# ---------------------------------------------------------------------------

_MODEL_CONFIG: Final[ConfigDict] = ConfigDict(
    extra="forbid",
    validate_assignment=True,
    frozen=True,
)

# ---------------------------------------------------------------------------
# Sub-models
# ---------------------------------------------------------------------------


class CapsuleSource(BaseModel):
    """Audit trail entry — one source backing a claim in the capsule.

    Attributes:
        type: Kind of source: ``"symbol"``, ``"commit"``, ``"trace"``, ``"pr"``.
        id: Stable identifier for this source (symbol_id, commit sha, etc.).
        uri: Optional human-navigable URI to the source (e.g. GitHub permalink).
    """

    model_config = _MODEL_CONFIG

    type: Literal["symbol", "commit", "trace", "pr"]
    id: str = Field(min_length=1, max_length=512)
    uri: str | None = None


class RootCauseCandidate(BaseModel):
    """A ranked hypothesis for the root cause of a bugfix task.

    Attributes:
        symbol_id: The symbol implicated as the root cause.
        rationale: Short human-readable explanation.
        evidence: List of evidence strings (stack frames, log snippets, etc.).
    """

    model_config = _MODEL_CONFIG

    symbol_id: str = Field(min_length=1, max_length=512)
    rationale: str = Field(min_length=1)
    evidence: list[str] = Field(default_factory=list)


class RelevantSymbol(BaseModel):
    """A symbol that is relevant to the user's task.

    Attributes:
        symbol_id: Stable symbol identifier.
        kind: Symbol kind (function, class, method, …).
        snippet: Raw source snippet or ``None`` if budget exhausted.
        summary: Semantic summary or ``None`` if not yet generated (Phase 2).
        score: Retrieval relevance score (higher is better).
    """

    model_config = _MODEL_CONFIG

    symbol_id: str = Field(min_length=1, max_length=512)
    kind: str = Field(min_length=1, max_length=64)
    snippet: str | None = None
    summary: str | None = None
    score: float = Field(ge=0.0)


class CallChainEdge(BaseModel):
    """One directed edge in the call chain.

    Note: the design schema uses ``from``/``to`` which are Python keywords;
    we use ``from_id`` / ``to_id`` in the Pydantic model and alias them on
    serialisation so the JSON output matches the design schema.
    """

    model_config = ConfigDict(
        extra="forbid",
        validate_assignment=True,
        frozen=True,
        populate_by_name=True,
    )

    from_id: str = Field(alias="from", min_length=1, max_length=512)
    to_id: str = Field(alias="to", min_length=1, max_length=512)
    kind: str = Field(min_length=1, max_length=64)
    confidence: float = Field(default=1.0, ge=0.0, le=1.0)


class RuntimeEvidence(BaseModel):
    """Runtime signal attached to a symbol (OTel / Sentry; Phase 3 content).

    Attributes:
        kind: Signal kind: ``"error"``, ``"latency"``, or ``"trace"``.
        symbol_id: Symbol this evidence refers to.
        payload: Free-form JSON payload (trace_id, p95_ms, …).
    """

    model_config = _MODEL_CONFIG

    kind: Literal["error", "latency", "trace"]
    symbol_id: str = Field(min_length=1, max_length=512)
    payload: dict[str, Any] = Field(default_factory=dict)


class NeighborPattern(BaseModel):
    """A symbol that follows a similar structural or semantic pattern.

    Attributes:
        symbol_id: The neighbouring symbol.
        similarity: Cosine similarity (0-1).
        why: Short explanation of the similarity signal.
    """

    model_config = _MODEL_CONFIG

    symbol_id: str = Field(min_length=1, max_length=512)
    similarity: float = Field(ge=0.0, le=1.0)
    why: str = Field(min_length=1)


class RiskArea(BaseModel):
    """A symbol flagged as a risk area (high fan-in, recently changed, etc.).

    Attributes:
        symbol_id: The at-risk symbol.
        reason: Human-readable reason (e.g. ``"high fan-in"``).
    """

    model_config = _MODEL_CONFIG

    symbol_id: str = Field(min_length=1, max_length=512)
    reason: str = Field(min_length=1)


class CompressedContext(BaseModel):
    """A compressed bullet-point summary for a named context section.

    Attributes:
        section: Section identifier / name (e.g. ``"auth flow"``).
        bullets: List of concise bullet strings.
    """

    model_config = _MODEL_CONFIG

    section: str = Field(min_length=1, max_length=256)
    bullets: list[str] = Field(default_factory=list)


# ---------------------------------------------------------------------------
# ContextCapsule — top-level schema
# ---------------------------------------------------------------------------


class ContextCapsule(BaseModel):
    """Context Capsule v1 — task-optimised understanding object.

    This is the top-level output of :class:`~cognis.capsule.composer.CapsuleComposer`.
    It conforms to the JSON Schema shipped at
    ``packages/core/cognis/schemas/capsule.v1.json``.

    Composition rules (design §Context Capsule schema):

    1. **Sources mandatory**: every populated section has ≥ 1 ``sources[]`` entry.
    2. **Token estimate honest**: ``token_estimate ≤ max_tokens`` after 10% margin.
    3. **No null arrays**: empty lists not ``None`` so consumers don't branch.
    4. **Deterministic**: same query + same DB state → same capsule (modulo
       wall-clock fields such as ``generated_at``).

    CP-9 (PBT): composing with any ``max_tokens ∈ [500, 32000]`` always
    produces ``token_estimate ≤ max_tokens`` and non-empty ``sources[]`` for
    every populated section.
    """

    model_config = ConfigDict(
        extra="forbid",
        validate_assignment=True,
        frozen=True,
    )

    version: Literal["1"] = "1"
    """Schema version — always ``"1"`` at MVP."""

    goal: str
    """Original user task / query string."""

    task_mode: Literal["bugfix", "feature", "refactor", "explain", "migrate", "review"]
    """Task classification mode determined by the planner."""

    confidence: float = Field(ge=0.0, le=1.0)
    """Planner classifier confidence for ``task_mode``."""

    root_cause_candidates: list[RootCauseCandidate] = Field(default_factory=list)
    """Ranked root-cause hypotheses (populated for ``bugfix`` mode)."""

    relevant_symbols: list[RelevantSymbol] = Field(default_factory=list)
    """Symbols most relevant to the task, ranked by cross-layer rank fusion
    (RRF); each entry's ``score`` is its originating layer score, unchanged."""

    call_chain: list[CallChainEdge] = Field(default_factory=list)
    """Call-graph edges for the structural context window."""

    runtime_evidence: list[RuntimeEvidence] = Field(default_factory=list)
    """Runtime signals (OTel/Sentry; Phase 3 content; empty at MVP)."""

    neighbor_patterns: list[NeighborPattern] = Field(default_factory=list)
    """Structurally / semantically similar symbols."""

    risk_areas: list[RiskArea] = Field(default_factory=list)
    """Symbols flagged as risk areas."""

    compressed_context: list[CompressedContext] = Field(default_factory=list)
    """Compressed bullet-point context sections."""

    token_estimate: int = Field(ge=0)
    """Estimated token count for this capsule (tiktoken cl100k_base + 10% margin)."""

    sources: list[CapsuleSource] = Field(default_factory=list)
    """Audit trail — at least one entry per populated section."""

    untrusted_sections: list[str] = Field(default_factory=list)
    """Section IDs whose content is wrapped with ``<<<UNTRUSTED ... >>>`` markers."""


__all__ = [
    "CallChainEdge",
    "CapsuleSource",
    "CompressedContext",
    "ContextCapsule",
    "NeighborPattern",
    "RelevantSymbol",
    "RiskArea",
    "RootCauseCandidate",
    "RuntimeEvidence",
]
