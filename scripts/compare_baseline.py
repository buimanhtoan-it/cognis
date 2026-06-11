"""Gate an e2e-report.json against a committed baseline.

Closes the automation gap in docs/development-criteria.md: turns the e2e report
into a regression gate so the four-pillar criteria are enforced, not just
documented.

Two classes of check (deliberately separated, because latency is
hardware-dependent and would otherwise flake across machines/CI):

  * HARD invariants — hardware-independent correctness of the user flow. A
    failure here fails the gate (exit 1). E.g. the embedding bar must move,
    semantic_search must return hits, the index must have vectors, health ok.
  * SOFT perf budgets — latency/throughput/footprint relative to the baseline.
    Reported always; they only FAIL the gate under ``--strict-perf`` (and even
    then with a generous tolerance), so normal CI on variable hardware is gated
    on correctness while perf is tracked for trend/regression review.

Usage:
    # refresh the committed baseline from a fresh report
    python scripts/compare_baseline.py --current eval-reports/e2e/e2e-report.json --update

    # gate a new report against the baseline (CI)
    python scripts/compare_baseline.py --current eval-reports/e2e/e2e-report.json
    python scripts/compare_baseline.py --current <report.json> --strict-perf --perf-tolerance 0.5
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path
from typing import Any

DEFAULT_BASELINE = Path("tests/e2e/baselines/sample.json")


def _get(d: dict, *path: str, default: Any = None) -> Any:
    cur: Any = d
    for key in path:
        if not isinstance(cur, dict) or key not in cur:
            return default
        cur = cur[key]
    return cur


def _hard_invariants(report: dict) -> list[tuple[str, bool, str]]:
    """Return (name, passed, detail) for hardware-independent flow correctness."""
    out: list[tuple[str, bool, str]] = []

    sym = int(_get(report, "index_stats", "symbol", default=0) or 0)
    vec = int(_get(report, "index_stats", "symbol_vec", default=0) or 0)
    out.append(("index has symbols", sym > 0, f"symbol={sym}"))
    out.append(("index has vectors", vec > 0, f"symbol_vec={vec}"))

    health = str(_get(report, "health_overall", default=""))
    out.append(("health ok/warn", health in {"ok", "warn"}, f"health={health!r}"))

    out.append(
        (
            "symbol_search found exact name",
            bool(_get(report, "symbol_search", "found_expected", default=False)),
            f"query={_get(report, 'symbol_search', 'query')!r}",
        )
    )

    sem_skipped = bool(_get(report, "semantic_search", "skipped", default=True))
    sem_hits = len(_get(report, "semantic_search", "names", default=[]) or [])
    # Skipped is acceptable only when the embedder extra is genuinely absent;
    # if vectors exist, semantic must return something.
    sem_ok = sem_skipped if vec == 0 else (not sem_skipped and sem_hits > 0)
    out.append(("semantic_search returns hits", sem_ok, f"skipped={sem_skipped} hits={sem_hits}"))

    moved = bool(_get(report, "embedding_progress", "moved", default=False))
    # Only require movement when there were enough symbols to chunk (progress is
    # chunk-granular); tiny repos embed in a single chunk and legitimately show
    # one update.
    if sym >= 300:
        out.append(("embedding progress bar moved", moved, f"moved={moved} (symbols={sym})"))
    return out


def _soft_perf(report: dict, baseline: dict, tol: float) -> list[tuple[str, bool, str]]:
    """Return (name, within_budget, detail) for latency/throughput/footprint."""
    out: list[tuple[str, bool, str]] = []

    def _cmp_lower_is_better(name: str, cur: float | None, base: float | None) -> None:
        if cur is None or base is None or base <= 0:
            out.append((name, True, f"cur={cur} base={base} (no baseline)"))
            return
        budget = base * (1.0 + tol)
        out.append((name, cur <= budget, f"cur={cur:.3f} base={base:.3f} budget<={budget:.3f}"))

    def _cmp_higher_is_better(name: str, cur: float | None, base: float | None) -> None:
        if cur is None or base is None or base <= 0:
            out.append((name, True, f"cur={cur} base={base} (no baseline)"))
            return
        floor = base * (1.0 - tol)
        out.append((name, cur >= floor, f"cur={cur:.1f} base={base:.1f} floor>={floor:.1f}"))

    _cmp_higher_is_better(
        "throughput symbols/sec",
        _get(report, "throughput", "symbols_per_sec"),
        _get(baseline, "throughput", "symbols_per_sec"),
    )
    _cmp_lower_is_better(
        "hot semantic query (s)",
        _get(report, "semantic_search", "hot_call_s"),
        _get(baseline, "semantic_search", "hot_call_s"),
    )
    _cmp_lower_is_better(
        "server warm/startup (s)",
        _get(report, "semantic_search", "server_warm_startup_s"),
        _get(baseline, "semantic_search", "server_warm_startup_s"),
    )
    cur_sym = int(_get(report, "index_stats", "symbol", default=0) or 0)
    base_sym = int(_get(baseline, "index_stats", "symbol", default=0) or 0)
    cur_bytes = int(_get(report, "index_stats", "db_bytes", default=0) or 0)
    base_bytes = int(_get(baseline, "index_stats", "db_bytes", default=0) or 0)
    _cmp_lower_is_better(
        "DB bytes/symbol",
        (cur_bytes / cur_sym) if cur_sym else None,
        (base_bytes / base_sym) if base_sym else None,
    )
    return out


def main() -> int:
    ap = argparse.ArgumentParser(description="Gate an e2e report against a baseline.")
    ap.add_argument("--current", required=True, help="path to the new e2e-report.json")
    ap.add_argument("--baseline", default=str(DEFAULT_BASELINE))
    ap.add_argument("--update", action="store_true", help="write current as the new baseline")
    ap.add_argument("--strict-perf", action="store_true", help="also fail on perf regressions")
    ap.add_argument(
        "--perf-tolerance", type=float, default=0.5, help="fractional perf slack (0.5=50%%)"
    )
    args = ap.parse_args()

    current = json.loads(Path(args.current).read_text(encoding="utf-8"))

    if args.update:
        Path(args.baseline).parent.mkdir(parents=True, exist_ok=True)
        Path(args.baseline).write_text(json.dumps(current, indent=2), encoding="utf-8")
        print(f"Wrote baseline → {args.baseline}")
        return 0

    baseline_path = Path(args.baseline)
    baseline = (
        json.loads(baseline_path.read_text(encoding="utf-8")) if baseline_path.exists() else {}
    )

    hard = _hard_invariants(current)
    soft = _soft_perf(current, baseline, args.perf_tolerance)

    print(f"Baseline: {args.baseline} (exists={baseline_path.exists()})")
    print(f"Current : {args.current}\n")
    print("HARD invariants (gate):")
    for name, ok, detail in hard:
        print(f"  [{'PASS' if ok else 'FAIL'}] {name} - {detail}")
    print(
        "\nSOFT perf budgets (tol +/-{:.0%}{}):".format(
            args.perf_tolerance, ", STRICT" if args.strict_perf else ", report-only"
        )
    )
    for name, ok, detail in soft:
        print(f"  [{'ok' if ok else 'OVER'}] {name} - {detail}")

    hard_failed = [n for n, ok, _ in hard if not ok]
    perf_failed = [n for n, ok, _ in soft if not ok]

    rc = 0
    if hard_failed:
        print(f"\nFAIL: hard invariants violated: {hard_failed}")
        rc = 1
    if args.strict_perf and perf_failed:
        print(f"\nFAIL (strict-perf): perf budgets exceeded: {perf_failed}")
        rc = 1
    if rc == 0:
        msg = "all hard invariants pass"
        if perf_failed and not args.strict_perf:
            msg += f"; perf over budget (report-only): {perf_failed}"
        print(f"\nPASS - {msg}")
    return rc


if __name__ == "__main__":
    raise SystemExit(main())
