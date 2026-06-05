#!/usr/bin/env python3
"""Eval runner script for cognis — Task 17.2.

Usage (after indexing a repo):

    # Basic run against the default golden set:
    python scripts/run_eval.py

    # Point at a specific golden set and DB:
    COGNIS_DB_PATH=.cognis/uckg.db \\
        python scripts/run_eval.py \\
            --golden tests/fixtures/eval/golden.jsonl \\
            --out eval-reports/

    # Run with a specific k cutoff:
    python scripts/run_eval.py --k 10

Prerequisites:
    - A live UCKG database at COGNIS_DB_PATH (or .cognis/uckg.db)
    - cognis package installed (pip install -e .)
    - Optional: embed-local extra for semantic retrieval

Outputs:
    eval-reports/<timestamp>/report.json
    eval-reports/<timestamp>/summary.md
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

# Ensure project root is on sys.path when run as a script.
_PROJECT_ROOT = Path(__file__).resolve().parent.parent
if str(_PROJECT_ROOT) not in sys.path:
    sys.path.insert(0, str(_PROJECT_ROOT))


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Run cognis eval harness and write JSON + Markdown report."
    )
    parser.add_argument(
        "--golden",
        type=Path,
        default=None,
        help="Path to golden JSONL file (default: tests/fixtures/eval/golden.jsonl)",
    )
    parser.add_argument(
        "--out",
        type=Path,
        default=None,
        help="Output directory for reports (default: eval-reports/)",
    )
    parser.add_argument(
        "--k",
        type=int,
        default=10,
        help="Recall cutoff (default: 10)",
    )
    parser.add_argument(
        "--db-path",
        type=Path,
        default=None,
        help="UCKG database path (default: COGNIS_DB_PATH or .cognis/uckg.db)",
    )
    parser.add_argument(
        "--null",
        action="store_true",
        help="Force NullStrategy (smoke test only)",
    )
    parser.add_argument(
        "--no-timestamp",
        action="store_true",
        help=(
            "Write reports directly into --out instead of a nested "
            "<out>/<timestamp>/ subdirectory. Use when the caller already "
            "provides a unique output directory (e.g. CI)."
        ),
    )
    args = parser.parse_args()

    golden = args.golden or (_PROJECT_ROOT / "tests" / "fixtures" / "eval" / "golden.jsonl")
    out_base = args.out or (_PROJECT_ROOT / "eval-reports")

    if not golden.exists():
        print(f"ERROR: golden set not found at {golden}", file=sys.stderr)
        print("Run `cognis-cli init` first or pass --golden.", file=sys.stderr)
        return 1

    import os

    if args.db_path is not None:
        os.environ["COGNIS_DB_PATH"] = str(args.db_path.resolve())

    try:
        from cognis_eval.runner import NullStrategy, prepare_out_dir, run_eval
        from cognis_eval.strategy import HybridStrategy, strategy_from_env
    except ImportError as exc:
        print(f"ERROR: cognis_eval not importable: {exc}", file=sys.stderr)
        print("Install the package: pip install -e .", file=sys.stderr)
        return 1

    # By default each run gets its own ``<out>/<timestamp>/`` subdir. When the
    # caller already supplies a unique directory (CI computes one per run),
    # ``--no-timestamp`` writes straight into ``--out`` so the report path is
    # predictable and the baseline gate can find ``<out>/report.json``.
    if args.no_timestamp:
        out_dir = out_base
        out_dir.mkdir(parents=True, exist_ok=True)
    else:
        out_dir = prepare_out_dir(out_base)
    print(f"Running eval against: {golden}")
    print(f"Output directory    : {out_dir}")
    print(f"Recall@k cutoff     : {args.k}")
    print()

    strategy = NullStrategy() if args.null else (strategy_from_env() or HybridStrategy())

    report, written_to = run_eval(golden, out_dir, k=args.k, strategy=strategy)

    print(f"  strategy          : {report.strategy}")
    print(f"  queries           : {report.num_queries}")
    print(f"  Recall@{report.k:<5}       : {report.recall_at_k:.4f}")
    print(f"  MRR               : {report.mrr:.4f}")
    print(f"  token efficiency  : {report.capsule_token_efficiency:.4f}")
    print()
    print(f"Report written to   : {written_to}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
