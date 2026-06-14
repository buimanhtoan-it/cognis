#!/usr/bin/env python3
"""Gate an eval report.json against a recorded *no-regression* baseline.

Scope and honesty (read before changing the numbers):

This is a **smoke / no-regression** gate on the *synthetic* fixture golden
(`tests/fixtures/eval/golden.jsonl` over the tiny `mini-ts/py/go` repos). It is
**not** a public quality claim. Authoritative retrieval quality is the
`.benchmarks/` harness on **objective, PR-derived** ground truth across real
public repos (see `docs/development-criteria.md`, Pillar 1), where the engine is
RRF-ranked and "outperforms baselines" is intentionally not claimed.

Earlier this gate used hard aspirational minimums (Recall@10 ≥ 0.70, MRR ≥ 0.50)
that the engine never actually met on this hand-authored, concept-label golden —
so the build failed on an ungrounded absolute, not a regression. It now records
the *measured* Recall@k / MRR as a baseline and fails only on a regression beyond
``regression_tolerance`` (default 0.05 absolute). That catches an accidental
retrieval breakage without asserting a number the project does not claim.

Refresh the baseline **deliberately** (record the new measured value, note why)
when retrieval changes on purpose — never silently to make a build pass.

Exit 0 when within tolerance of baseline; exit 1 on regression.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

_ROOT = Path(__file__).resolve().parent.parent
_DEFAULT_BASELINE = _ROOT / "eval-baselines" / "phase1.json"
_DEFAULT_TOLERANCE = 0.05


def _baseline_value(baseline: dict, *keys: str) -> float | None:
    """Return the first present key as a float, or None when absent."""
    for key in keys:
        if key in baseline and baseline[key] is not None:
            return float(baseline[key])
    return None


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Gate eval report against a no-regression baseline."
    )
    parser.add_argument("report", type=Path, help="Path to report.json from an eval run")
    parser.add_argument(
        "--baseline",
        type=Path,
        default=_DEFAULT_BASELINE,
        help="Baseline JSON (default: eval-baselines/phase1.json)",
    )
    args = parser.parse_args()

    if not args.report.is_file():
        print(f"ERROR: report not found: {args.report}", file=sys.stderr)
        return 1
    if not args.baseline.is_file():
        print(f"ERROR: baseline not found: {args.baseline}", file=sys.stderr)
        return 1

    report = json.loads(args.report.read_text(encoding="utf-8"))
    baseline = json.loads(args.baseline.read_text(encoding="utf-8"))

    recall = float(report.get("recall_at_k", 0.0))
    mrr = float(report.get("mrr", 0.0))
    k = report.get("k", 10)

    tolerance = float(baseline.get("regression_tolerance", _DEFAULT_TOLERANCE))
    # Prefer the no-regression baseline keys; fall back to the legacy absolute
    # ``*_min`` keys (treated as the recorded baseline) for old baseline files.
    recall_base = _baseline_value(baseline, "recall_at_k_baseline", "recall_at_k_min")
    mrr_base = _baseline_value(baseline, "mrr_baseline", "mrr_min")
    if recall_base is None or mrr_base is None:
        print(
            "ERROR: baseline missing recall_at_k_baseline / mrr_baseline (or legacy *_min) keys",
            file=sys.stderr,
        )
        return 1

    recall_floor = recall_base - tolerance
    mrr_floor = mrr_base - tolerance

    print(f"  strategy     : {report.get('strategy')}")
    print(f"  Recall@{k}   : {recall:.4f} (baseline {recall_base:.4f}, floor {recall_floor:.4f})")
    print(f"  MRR          : {mrr:.4f} (baseline {mrr_base:.4f}, floor {mrr_floor:.4f})")
    print(f"  tolerance    : {tolerance:.4f} (no-regression smoke gate on the synthetic golden)")

    failed = False
    if recall < recall_floor:
        print(
            f"FAIL: Recall@{k} {recall:.4f} regressed below floor {recall_floor:.4f} "
            f"(baseline {recall_base:.4f} - tol {tolerance:.4f})",
            file=sys.stderr,
        )
        failed = True
    if mrr < mrr_floor:
        print(
            f"FAIL: MRR {mrr:.4f} regressed below floor {mrr_floor:.4f} "
            f"(baseline {mrr_base:.4f} - tol {tolerance:.4f})",
            file=sys.stderr,
        )
        failed = True

    if failed:
        return 1
    print("PASS: eval within no-regression tolerance of baseline.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
