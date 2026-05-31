#!/usr/bin/env python3
"""Compare an eval report.json against eval-baselines/phase1.json.

Exit 0 when gates pass; exit 1 on regression.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

_ROOT = Path(__file__).resolve().parent.parent
_DEFAULT_BASELINE = _ROOT / "eval-baselines" / "phase1.json"


def main() -> int:
    parser = argparse.ArgumentParser(description="Gate eval report against phase1 baseline.")
    parser.add_argument(
        "report",
        type=Path,
        help="Path to report.json from eval run",
    )
    parser.add_argument(
        "--baseline",
        type=Path,
        default=_DEFAULT_BASELINE,
        help="Baseline thresholds JSON (default: eval-baselines/phase1.json)",
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
    recall_min = float(baseline.get("recall_at_k_min", 0.70))
    mrr_min = float(baseline.get("mrr_min", 0.50))

    print(f"  strategy     : {report.get('strategy')}")
    print(f"  Recall@{report.get('k', 10)}: {recall:.4f} (min {recall_min:.4f})")
    print(f"  MRR          : {mrr:.4f} (min {mrr_min:.4f})")

    failed = False
    if recall < recall_min:
        print(
            f"FAIL: Recall@{report.get('k', 10)} {recall:.4f} < {recall_min:.4f}", file=sys.stderr
        )
        failed = True
    if mrr < mrr_min:
        print(f"FAIL: MRR {mrr:.4f} < {mrr_min:.4f}", file=sys.stderr)
        failed = True

    if failed:
        return 1
    print("PASS: eval gates met.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
