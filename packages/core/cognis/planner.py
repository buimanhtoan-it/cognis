"""Cognitive Context Planner — rule-based MVP.

Implements the three-step pipeline described in the design document's
*Cognitive Context Planner* section:

1. :meth:`Planner.classify` — regex-based task-mode classifier (no LLM call).
2. :meth:`Planner.layer_plan` — look up the layer weight table for a mode.
3. :meth:`Planner.allocate_budget` — distribute ``max_tokens`` across available
   layers, reallocating absent-layer tokens to the next-priority layer.

Design reference
----------------
Task modes: ``bugfix``, ``feature``, ``refactor``, ``explain``, ``migrate``,
``review``.  The planner is *deterministic* and *latency-bounded* — no external
call, no LLM inference.  Full classify + plan + budget MUST complete in < 30ms
p95 (design NFR Performance, task 13.4).

Correctness property
--------------------
CP-8 (PBT): ``sum(quotas.values()) ≤ max_tokens`` for any input.
Absent layers reallocate their budget to the next-priority layer — no tokens
are lost.
"""

from __future__ import annotations

import re
from dataclasses import dataclass
from typing import Literal

# ---------------------------------------------------------------------------
# Types
# ---------------------------------------------------------------------------

TaskMode = Literal["bugfix", "feature", "refactor", "explain", "migrate", "review"]
"""The six task classification modes understood by the planner."""

RESERVED_TOKENS: int = 500
"""Tokens reserved for the goal + sources + risk_areas block in every capsule."""

# All valid layer names (MVP implements lexical, semantic, structural;
# temporal and behavioral are Phase 2/3 but are modelled here so the budget
# allocator can reallocate their weight to whichever layers ARE available).
_ALL_LAYERS: tuple[str, ...] = (
    "lexical",
    "semantic",
    "structural",
    "temporal",
    "behavioral",
)


# ---------------------------------------------------------------------------
# SectionQuotas
# ---------------------------------------------------------------------------


@dataclass
class SectionQuotas:
    """Per-layer token budgets after :meth:`Planner.allocate_budget`.

    ``reserved`` is always subtracted from ``max_tokens`` before the
    percentages are applied, so ``sum(quotas.values()) ≤ max_tokens``.

    .. code-block:: python

        q = planner.allocate_budget(8000, plan, {"lexical", "semantic", "structural"})
        assert sum(q.values()) <= 8000
    """

    lexical: int = 0
    semantic: int = 0
    structural: int = 0
    temporal: int = 0
    behavioral: int = 0
    reserved: int = RESERVED_TOKENS
    """Tokens set aside for goal + sources + risk_areas (always 500)."""

    # ------------------------------------------------------------------
    # Helpers
    # ------------------------------------------------------------------

    def values(self) -> list[int]:
        """Return all quota values (including ``reserved``) as a list."""
        return [
            self.lexical,
            self.semantic,
            self.structural,
            self.temporal,
            self.behavioral,
            self.reserved,
        ]

    def layer_values(self) -> dict[str, int]:
        """Return the five layer quotas as a ``{layer: tokens}`` dict.

        ``reserved`` is intentionally excluded so callers can iterate
        over retrieval layers without special-casing the reserved block.
        """
        return {
            "lexical": self.lexical,
            "semantic": self.semantic,
            "structural": self.structural,
            "temporal": self.temporal,
            "behavioral": self.behavioral,
        }


# ---------------------------------------------------------------------------
# Layer plan table (design: Cognitive Context Planner → Layer plan per mode)
# ---------------------------------------------------------------------------

# Each entry is a list of (layer_name, weight_percent) in *priority order*.
# Weights within a mode sum to 100.
_LAYER_PLANS: dict[TaskMode, list[tuple[str, float]]] = {
    "bugfix": [
        ("structural", 35.0),
        ("temporal", 20.0),
        ("behavioral", 20.0),
        ("semantic", 15.0),
        ("lexical", 10.0),
    ],
    "feature": [
        ("semantic", 40.0),
        ("structural", 30.0),
        ("temporal", 20.0),
        ("lexical", 10.0),
    ],
    "refactor": [
        ("structural", 50.0),
        ("semantic", 20.0),
        ("temporal", 20.0),
        ("lexical", 10.0),
    ],
    "explain": [
        ("semantic", 40.0),
        ("structural", 30.0),
        ("lexical", 20.0),
        ("temporal", 10.0),
    ],
    "migrate": [
        ("structural", 40.0),
        ("semantic", 40.0),
        ("temporal", 20.0),
    ],
    "review": [
        ("structural", 40.0),
        ("temporal", 30.0),
        ("behavioral", 30.0),
    ],
}

# ---------------------------------------------------------------------------
# Classifier patterns (design: Task classifier, rule-based)
# Priority order: first match wins (most specific patterns first).
# ---------------------------------------------------------------------------

_CLASSIFIER_RULES: list[tuple[re.Pattern[str], TaskMode, float]] = [
    # Stack-trace detection (forced bugfix, highest confidence).
    # "at line N" and "#N" are strong code-location patterns.
    (re.compile(r"Traceback|at line \d+|#\d+", re.IGNORECASE), "bugfix", 1.0),
    # Primary keyword patterns (design order: bugfix → refactor → explain →
    # migrate → review; else → feature).
    # bugfix: literal "timeout" or "time out" / "timing out".
    (
        re.compile(
            r"error|exception|traceback|stack\s+trace|fail|broken"
            r"|time[\s-]?out|hang\b",
            re.IGNORECASE,
        ),
        "bugfix",
        0.85,
    ),
    (
        re.compile(
            r"refactor|extract|rename|split|move|consolidate",
            re.IGNORECASE,
        ),
        "refactor",
        0.85,
    ),
    (
        re.compile(
            r"how\b|why\b|what does|explain|architecture|design",
            re.IGNORECASE,
        ),
        "explain",
        0.85,
    ),
    (
        re.compile(
            r"migrate|upgrade|port from|convert to",
            re.IGNORECASE,
        ),
        "migrate",
        0.85,
    ),
    (
        re.compile(
            r"review|risk|impact|breaking",
            re.IGNORECASE,
        ),
        "review",
        0.85,
    ),
]

# Confidence threshold below which we fall back to "feature".
_CONFIDENCE_FLOOR: float = 0.6


# ---------------------------------------------------------------------------
# Planner
# ---------------------------------------------------------------------------


class Planner:
    """Rule-based Cognitive Context Planner (MVP).

    All three methods are **synchronous** and **free of I/O** — no DB calls,
    no LLM calls.  This guarantees the < 30ms p95 latency budget.

    Usage
    -----
    .. code-block:: python

        planner = Planner()
        mode, confidence = planner.classify("Why is /login timing out?")
        plan = planner.layer_plan(mode)
        quotas = planner.allocate_budget(8000, plan, {"lexical", "semantic", "structural"})
    """

    # ------------------------------------------------------------------
    # 13.1 — classify
    # ------------------------------------------------------------------

    def classify(self, task: str) -> tuple[TaskMode, float]:
        """Classify a task string into a :data:`TaskMode` and confidence score.

        Algorithm (design *Task classifier, rule-based*):

        1. Check for stack-trace-like substrings first (forces ``bugfix``,
           confidence 1.0).
        2. Try each pattern rule in priority order; return on first match.
        3. If no rule fires, return ``("feature", 1.0)`` (the default mode).
        4. If the matched confidence is < 0.6, fall back to ``("feature",
           confidence)`` per design spec.

        Parameters
        ----------
        task:
            Free-form user task / query string.

        Returns
        -------
        (mode, confidence):
            ``mode`` is one of the six :data:`TaskMode` values.
            ``confidence`` is in [0.0, 1.0].
        """
        if not task:
            return ("feature", 1.0)

        for pattern, mode, confidence in _CLASSIFIER_RULES:
            if pattern.search(task):
                if confidence < _CONFIDENCE_FLOOR:
                    return ("feature", confidence)
                return (mode, confidence)

        # No rule matched → default feature mode.
        return ("feature", 1.0)

    # ------------------------------------------------------------------
    # 13.2 — layer_plan
    # ------------------------------------------------------------------

    def layer_plan(self, mode: TaskMode) -> dict[str, float]:
        """Return the layer weight table for the given task mode.

        Weights are percentages (0-100) and reflect the priority-ordered
        budget split from the design document's layer plan table.

        Parameters
        ----------
        mode:
            A :data:`TaskMode` string.

        Returns
        -------
        dict[str, float]:
            ``{layer_name: weight_percent}`` in priority order (Python 3.7+
            dicts preserve insertion order).  Weights sum to 100.
        """
        return {layer: weight for layer, weight in _LAYER_PLANS[mode]}

    # ------------------------------------------------------------------
    # 13.3 — allocate_budget
    # ------------------------------------------------------------------

    def allocate_budget(
        self,
        max_tokens: int,
        plan: dict[str, float],
        available_layers: set[str],
    ) -> SectionQuotas:
        """Distribute ``max_tokens`` across retrieval layers.

        Algorithm (design *Budget allocation*):

        1. Reserve 500 tokens for the goal + sources + risk_areas block.
        2. If ``max_tokens ≤ RESERVED_TOKENS`` there are no tokens left for
           layers; all layer quotas are 0.
        3. Walk the plan in priority order.  For each layer:
           - If the layer is in ``available_layers``, allocate
             ``round(weight / total_available_weight * distributable_tokens)``.
           - If the layer is absent, its weight is redistributed to the
             **next** available layer in priority order (no budget loss).
        4. The final available layer absorbs any rounding remainder so the
           invariant ``sum(quotas.values()) == max_tokens`` holds exactly when
           at least one layer is available.

        The rounding strategy guarantees
        ``sum(quotas.values()) ≤ max_tokens`` (CP-8).

        Parameters
        ----------
        max_tokens:
            Hard upper bound on total token spend (including ``reserved``).
        plan:
            Layer weight dict as returned by :meth:`layer_plan`.
        available_layers:
            Set of layer names that are actually implemented / reachable in
            this deployment.  Absent layers have their budget redistributed.

        Returns
        -------
        SectionQuotas:
            Per-layer token allocations.  ``sum(q.values()) ≤ max_tokens``.
        """
        quotas = SectionQuotas()

        # Step 1: reserve tokens for goal + sources + risk_areas.
        reserved = min(RESERVED_TOKENS, max_tokens)
        quotas.reserved = reserved
        distributable = max_tokens - reserved

        if distributable <= 0:
            # No tokens left for layers.
            return quotas

        # Step 2: build priority-ordered list of (layer, weight) from plan,
        # keeping only layers that appear in the plan.  Layers not in the plan
        # at all carry weight 0 and get nothing.
        priority_layers: list[tuple[str, float]] = list(plan.items())

        # Step 3: compute the total weight of *available* layers so we can
        # redistribute absent-layer weight proportionally.
        # We do a single-pass redistribution: walk in priority order;
        # accumulate "carry" from absent layers; give carry to the NEXT
        # available layer.

        # Compute total weight of all layers in the plan.
        total_plan_weight: float = sum(w for _, w in priority_layers)
        if total_plan_weight == 0.0:
            return quotas

        # Normalise weights so absent-layer budget is correctly reassigned.
        # We walk once, forwarding absent-layer weight to the next available.
        carried_weight: float = 0.0
        layer_allocations: list[tuple[str, float]] = []  # (layer, effective_weight)

        for _i, (layer, weight) in enumerate(priority_layers):
            if layer not in available_layers:
                # Carry this layer's weight forward to the next available.
                carried_weight += weight
            else:
                effective = weight + carried_weight
                carried_weight = 0.0
                layer_allocations.append((layer, effective))

        # Any remaining carried weight (all layers absent, or tail layers
        # absent) needs a home.  If there are *any* available layers, give it
        # to the last one that was allocated.  If no layer was allocated at
        # all, the budget just stays in the "layer" bucket but is unreachable —
        # we leave all layer quotas at 0 (budget effectively collapses to
        # reserved only, but we still return a valid object).
        if carried_weight > 0.0 and layer_allocations:
            last_layer, last_weight = layer_allocations[-1]
            layer_allocations[-1] = (last_layer, last_weight + carried_weight)

        if not layer_allocations:
            return quotas

        # Step 4: convert effective weights to token counts.
        total_effective: float = sum(w for _, w in layer_allocations)
        if total_effective == 0.0:
            return quotas

        allocated_so_far: int = 0
        token_assignments: list[tuple[str, int]] = []

        for idx, (layer, eff_weight) in enumerate(layer_allocations):
            is_last = idx == len(layer_allocations) - 1
            if is_last:
                # Last available layer absorbs rounding remainder.
                tokens = distributable - allocated_so_far
            else:
                tokens = int(eff_weight / total_effective * distributable)
            token_assignments.append((layer, tokens))
            allocated_so_far += tokens

        # Write into SectionQuotas.
        for layer, tokens in token_assignments:
            if layer == "lexical":
                quotas.lexical = tokens
            elif layer == "semantic":
                quotas.semantic = tokens
            elif layer == "structural":
                quotas.structural = tokens
            elif layer == "temporal":
                quotas.temporal = tokens
            elif layer == "behavioral":
                quotas.behavioral = tokens
            # Unknown layer names are silently ignored (forward-compat).

        return quotas


__all__ = [
    "RESERVED_TOKENS",
    "Planner",
    "SectionQuotas",
    "TaskMode",
]
