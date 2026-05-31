"""Capsule composer package — Task 14 of ``.kiro/specs/cognis/tasks.md``.

Submodules:

- :mod:`cognis.capsule.models` — Pydantic v2 models for :class:`ContextCapsule` v1.
- :mod:`cognis.capsule.token_estimator` — tiktoken-based token estimation.
- :mod:`cognis.capsule.composer` — :class:`CapsuleComposer` pipeline.

Public re-exports:
"""

from __future__ import annotations

from cognis.capsule.composer import CapsuleComposer, ComposeError
from cognis.capsule.models import (
    CallChainEdge,
    CapsuleSource,
    CompressedContext,
    ContextCapsule,
    NeighborPattern,
    RelevantSymbol,
    RiskArea,
    RootCauseCandidate,
    RuntimeEvidence,
)
from cognis.capsule.token_estimator import estimate_capsule_tokens, estimate_tokens

__all__ = [
    "CallChainEdge",
    "CapsuleComposer",
    "CapsuleSource",
    "ComposeError",
    "CompressedContext",
    "ContextCapsule",
    "NeighborPattern",
    "RelevantSymbol",
    "RiskArea",
    "RootCauseCandidate",
    "RuntimeEvidence",
    "estimate_capsule_tokens",
    "estimate_tokens",
]
