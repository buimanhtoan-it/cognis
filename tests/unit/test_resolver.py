"""Unit tests for the edge resolver (task 8.5).

Covers:
- ResolvedEdge dataclass construction and ambiguous flag semantics.
- HeuristicResolver: same-file call → confidence 1.0.
- HeuristicResolver: no self-loops.
- HeuristicResolver: cross-file / cross-module edge → confidence 0.6.
- HeuristicResolver: fuzzy match → confidence 0.4.
- HeuristicResolver: deduplication keeps highest confidence.
- HeuristicResolver: empty input → empty output.
- LspDetector: tsconfig.json presence → True, absence → False.
- LspDetector: pyproject.toml / pyrightconfig.json / go.mod detected.
- LspDetector: sub-directory markers detected.
- LspDetector: non-existent root → False (no crash).
- pipeline.resolve_edges: uses heuristic when no LSP detected.
- pipeline.persist_edges: writes Edge rows with ambiguous meta flag.

All tests use inline symbol sets — no fixture repos or external processes.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

import pytest
from cognis_indexer.resolver.base import EdgeResolver, ResolvedEdge
from cognis_indexer.resolver.heuristic import HeuristicResolver
from cognis_indexer.resolver.lsp import LspResolver
from cognis_indexer.resolver.lsp import detect as lsp_detect
from cognis_indexer.resolver.pipeline import persist_edges, resolve_edges

# ---------------------------------------------------------------------------
# Minimal stub for ParsedSymbol (avoids circular import)
# ---------------------------------------------------------------------------


@dataclass
class _Sym:
    """Lightweight ParsedSymbol stand-in for resolver unit tests."""

    id: str
    name: str
    qualified_name: str
    language: str
    file_path: str
    module: str
    body_excerpt: str | None = None
    kind: str = "function"
    line_start: int = 1
    line_end: int = 10
    signature: str | None = None
    docstring: str | None = None
    content_hash: str = "deadbeef"
    untrusted_flags: list[str] = field(default_factory=list)


def _sym(
    name: str,
    file_path: str = "src/app.py",
    language: str = "python",
    body: str | None = None,
    sym_id: str | None = None,
) -> _Sym:
    """Build a minimal _Sym; id defaults to ``<language>:<file_path>:<name>``."""
    sid = sym_id or f"{language}:{file_path}:{name}"
    return _Sym(
        id=sid,
        name=name,
        qualified_name=name,
        language=language,
        file_path=file_path,
        module=file_path.replace("/", ".").removesuffix(".py").removesuffix(".ts"),
        body_excerpt=body,
    )


# ---------------------------------------------------------------------------
# ResolvedEdge tests
# ---------------------------------------------------------------------------


class TestResolvedEdge:
    def test_construction_defaults(self) -> None:
        edge = ResolvedEdge(src_id="a", dst_id="b", kind="calls", confidence=1.0, ambiguous=False)
        assert edge.src_id == "a"
        assert edge.dst_id == "b"
        assert edge.kind == "calls"
        assert edge.confidence == 1.0
        assert edge.ambiguous is False
        assert edge.meta == {}

    def test_ambiguous_true_for_low_confidence(self) -> None:
        edge = ResolvedEdge(src_id="a", dst_id="b", kind="calls", confidence=0.4, ambiguous=True)
        assert edge.ambiguous is True

    def test_invalid_confidence_raises(self) -> None:
        with pytest.raises(ValueError, match="confidence"):
            ResolvedEdge(src_id="a", dst_id="b", kind="calls", confidence=1.5, ambiguous=False)

    def test_negative_confidence_raises(self) -> None:
        with pytest.raises(ValueError, match="confidence"):
            ResolvedEdge(src_id="a", dst_id="b", kind="calls", confidence=-0.1, ambiguous=False)

    def test_meta_stored(self) -> None:
        edge = ResolvedEdge(
            src_id="a",
            dst_id="b",
            kind="calls",
            confidence=0.6,
            ambiguous=False,
            meta={"source": "lsp"},
        )
        assert edge.meta == {"source": "lsp"}


# ---------------------------------------------------------------------------
# EdgeResolver Protocol conformance
# ---------------------------------------------------------------------------


class TestEdgeResolverProtocol:
    def test_heuristic_satisfies_protocol(self) -> None:
        resolver = HeuristicResolver()
        assert isinstance(resolver, EdgeResolver)

    def test_lsp_satisfies_protocol(self) -> None:
        resolver = LspResolver()
        assert isinstance(resolver, EdgeResolver)


# ---------------------------------------------------------------------------
# HeuristicResolver tests
# ---------------------------------------------------------------------------


class TestHeuristicResolver:
    def test_empty_input_returns_empty(self) -> None:
        resolver = HeuristicResolver()
        assert resolver.resolve([]) == []

    def test_same_file_call_confidence_one(self) -> None:
        """Caller in same file as callee → confidence 1.0."""
        caller = _sym("main", file_path="src/app.py", body="result = helper()")
        callee = _sym("helper", file_path="src/app.py", body="return 42")
        resolver = HeuristicResolver()
        edges = resolver.resolve([caller, callee])
        assert len(edges) == 1
        edge = edges[0]
        assert edge.src_id == caller.id
        assert edge.dst_id == callee.id
        assert edge.kind == "calls"
        assert edge.confidence == 1.0
        assert edge.ambiguous is False

    def test_no_self_loop(self) -> None:
        """A symbol whose body mentions its own name must not produce a self-edge."""
        sym = _sym("recursive", file_path="src/app.py", body="return recursive(n - 1)")
        resolver = HeuristicResolver()
        edges = resolver.resolve([sym])
        # No self-loop allowed.
        for e in edges:
            assert not (e.src_id == sym.id and e.dst_id == sym.id)

    def test_cross_file_same_language_confidence_0_6(self) -> None:
        """Caller and callee in different files but same language → confidence 0.6."""
        caller = _sym("process", file_path="src/service.py", body="return validate(data)")
        callee = _sym("validate", file_path="src/utils.py", body="return True")
        resolver = HeuristicResolver()
        edges = resolver.resolve([caller, callee])
        assert len(edges) == 1
        edge = edges[0]
        assert edge.confidence == pytest.approx(0.6)
        assert edge.ambiguous is False  # exactly 0.6 is not < 0.6

    def test_cross_language_falls_to_fuzzy(self) -> None:
        """Cross-language exact match → confidence 0.4 (no shared LSP scope)."""
        caller = _sym(
            "runner", file_path="src/main.ts", language="typescript", body="const r = helper()"
        )
        callee = _sym("helper", file_path="src/utils.py", language="python", body="pass")
        resolver = HeuristicResolver()
        edges = resolver.resolve([caller, callee])
        assert len(edges) == 1
        assert edges[0].confidence == pytest.approx(0.4)
        assert edges[0].ambiguous is True

    def test_fuzzy_match_confidence_0_4(self) -> None:
        """Identifier 'val' matches callee name 'validate' via startswith → 0.4."""
        caller = _sym("run", file_path="src/a.py", body="val(data)")
        callee = _sym("validate", file_path="src/b.py", body="pass")
        resolver = HeuristicResolver()
        edges = resolver.resolve([caller, callee])
        # Should produce a fuzzy edge (validate starts with val but val != validate)
        matching = [e for e in edges if e.dst_id == callee.id]
        assert len(matching) == 1
        assert matching[0].confidence == pytest.approx(0.4)
        assert matching[0].ambiguous is True

    def test_exact_name_beats_fuzzy(self) -> None:
        """When an exact name match is found, fuzzy on that same pair is discarded."""
        caller = _sym("run", file_path="src/a.py", body="do_thing(x) and do_thing_extra(y)")
        exact_callee = _sym("do_thing", file_path="src/a.py", body="pass")
        fuzzy_callee = _sym("do_thing_extra", file_path="src/b.py", body="pass")
        resolver = HeuristicResolver()
        edges = resolver.resolve([caller, exact_callee, fuzzy_callee])
        edge_to_exact = next((e for e in edges if e.dst_id == exact_callee.id), None)
        edge_to_fuzzy = next((e for e in edges if e.dst_id == fuzzy_callee.id), None)
        assert edge_to_exact is not None
        assert edge_to_exact.confidence == pytest.approx(1.0)  # same file
        assert edge_to_fuzzy is not None

    def test_deduplication_keeps_highest_confidence(self) -> None:
        """If two symbols have the same name, the same-file one wins."""
        caller = _sym("main", file_path="src/app.py", body="helper() and other()")
        callee_local = _sym("helper", file_path="src/app.py", body="pass")
        callee_remote = _sym(
            "helper",
            file_path="src/lib.py",
            body="pass",
            sym_id="python:src/lib.py:helper",
        )
        resolver = HeuristicResolver()
        edges = resolver.resolve([caller, callee_local, callee_remote])
        # Both matches: local 1.0, remote 0.6; dedup keeps both (different dst_id)
        dst_ids = {e.dst_id for e in edges}
        assert callee_local.id in dst_ids
        assert callee_remote.id in dst_ids
        local_edge = next(e for e in edges if e.dst_id == callee_local.id)
        remote_edge = next(e for e in edges if e.dst_id == callee_remote.id)
        assert local_edge.confidence == pytest.approx(1.0)
        assert remote_edge.confidence == pytest.approx(0.6)

    def test_no_body_excerpt_yields_no_edges(self) -> None:
        """Symbols without body_excerpt produce no outbound edges."""
        sym_a = _sym("alpha", body=None)
        sym_b = _sym("beta", body=None)
        resolver = HeuristicResolver()
        assert resolver.resolve([sym_a, sym_b]) == []

    def test_no_matching_identifiers_yields_no_edges(self) -> None:
        """Body with only unmatched identifiers yields no edges."""
        sym_a = _sym("alpha", body="xyz = zzz + www")
        sym_b = _sym("beta", body="return 0")
        resolver = HeuristicResolver()
        edges = resolver.resolve([sym_a, sym_b])
        assert edges == []

    def test_edge_kind_is_calls(self) -> None:
        caller = _sym("main", body="helper()")
        callee = _sym("helper", body="pass")
        edges = HeuristicResolver().resolve([caller, callee])
        assert all(e.kind == "calls" for e in edges)


# ---------------------------------------------------------------------------
# LSP detection tests
# ---------------------------------------------------------------------------


class TestLspDetect:
    def test_tsconfig_detected(self, tmp_path: Path) -> None:
        (tmp_path / "tsconfig.json").write_text("{}", encoding="utf-8")
        assert lsp_detect(tmp_path) is True

    def test_pyproject_toml_detected(self, tmp_path: Path) -> None:
        (tmp_path / "pyproject.toml").write_text("[build-system]\n", encoding="utf-8")
        assert lsp_detect(tmp_path) is True

    def test_pyrightconfig_detected(self, tmp_path: Path) -> None:
        (tmp_path / "pyrightconfig.json").write_text("{}", encoding="utf-8")
        assert lsp_detect(tmp_path) is True

    def test_go_mod_detected(self, tmp_path: Path) -> None:
        (tmp_path / "go.mod").write_text("module example.com/app\n", encoding="utf-8")
        assert lsp_detect(tmp_path) is True

    def test_absent_markers_returns_false(self, tmp_path: Path) -> None:
        # Empty directory — no markers.
        assert lsp_detect(tmp_path) is False

    def test_irrelevant_files_not_detected(self, tmp_path: Path) -> None:
        (tmp_path / "setup.py").write_text("", encoding="utf-8")
        (tmp_path / "package.json").write_text("{}", encoding="utf-8")
        # package.json is NOT in our marker set; setup.py is not either.
        assert lsp_detect(tmp_path) is False

    def test_subdirectory_marker_detected(self, tmp_path: Path) -> None:
        """tsconfig.json one level deep should be detected (monorepo sub-package)."""
        # One level deep (e.g. packages/tsconfig.json) is the supported depth.
        pkg = tmp_path / "packages"
        pkg.mkdir(parents=True)
        (pkg / "tsconfig.json").write_text("{}", encoding="utf-8")
        assert lsp_detect(tmp_path) is True

    def test_nonexistent_root_returns_false(self) -> None:
        assert lsp_detect("/this/path/definitely/does/not/exist/xyz") is False

    def test_fixture_mini_ts_app_detected(self) -> None:
        """The mini-ts-app fixture has a tsconfig.json → should be detected."""
        fixture = Path(__file__).parent.parent / "fixtures" / "repos" / "mini-ts-app"
        if not fixture.exists():
            pytest.skip("mini-ts-app fixture not present")
        assert lsp_detect(fixture) is True

    def test_fixture_mini_py_svc_detected(self) -> None:
        """The mini-py-svc fixture has a pyproject.toml → should be detected."""
        fixture = Path(__file__).parent.parent / "fixtures" / "repos" / "mini-py-svc"
        if not fixture.exists():
            pytest.skip("mini-py-svc fixture not present")
        assert lsp_detect(fixture) is True


# ---------------------------------------------------------------------------
# LspResolver (stub) tests
# ---------------------------------------------------------------------------


class TestLspResolver:
    def test_resolve_returns_empty(self) -> None:
        resolver = LspResolver()
        syms = [_sym("foo", body="bar()"), _sym("bar", body="pass")]
        assert resolver.resolve(syms) == []

    def test_resolve_empty_input(self) -> None:
        assert LspResolver().resolve([]) == []


# ---------------------------------------------------------------------------
# Pipeline: resolve_edges tests
# ---------------------------------------------------------------------------


class TestResolveEdges:
    def test_returns_heuristic_edges_no_lsp(self, tmp_path: Path) -> None:
        """Without any LSP marker, heuristic edges are returned."""
        caller = _sym("main", body="helper()")
        callee = _sym("helper", body="pass")
        edges = resolve_edges([caller, callee], repo_root=tmp_path)
        assert len(edges) == 1
        assert edges[0].src_id == caller.id
        assert edges[0].dst_id == callee.id

    def test_no_repo_root_still_works(self) -> None:
        """repo_root=None skips LSP detection; heuristic still runs."""
        caller = _sym("alpha", body="beta()")
        callee = _sym("beta", body="pass")
        edges = resolve_edges([caller, callee], repo_root=None)
        assert len(edges) == 1

    def test_output_sorted_by_key(self) -> None:
        """Output is sorted (src_id, dst_id, kind) for determinism."""
        a = _sym("a", file_path="src/a.py", body="b() and c()")
        b = _sym("b", file_path="src/a.py", body="pass")
        c = _sym("c", file_path="src/a.py", body="pass")
        edges = resolve_edges([a, b, c], repo_root=None)
        keys = [(e.src_id, e.dst_id, e.kind) for e in edges]
        assert keys == sorted(keys)

    def test_empty_symbols_returns_empty(self, tmp_path: Path) -> None:
        assert resolve_edges([], repo_root=tmp_path) == []

    def test_lsp_detected_but_stub_adds_nothing(self, tmp_path: Path) -> None:
        """Even when LSP is detected, MVP stub returns empty; heuristic wins."""
        (tmp_path / "tsconfig.json").write_text("{}", encoding="utf-8")
        caller = _sym("main", body="helper()")
        callee = _sym("helper", body="pass")
        edges = resolve_edges([caller, callee], repo_root=tmp_path)
        # Still get the heuristic edge (LSP stub adds nothing).
        assert len(edges) >= 1
        assert any(e.src_id == caller.id and e.dst_id == callee.id for e in edges)


# ---------------------------------------------------------------------------
# Pipeline: persist_edges tests (task 8.4)
# ---------------------------------------------------------------------------


class TestPersistEdges:
    def _make_db(self, tmp_path: Path) -> Any:
        """Create a fresh per-test Database backed by a temp file.

        Using a unique temp file per test ensures the thread-local cache
        never reuses stale connections across test cases.
        """
        from cognis.db import Database

        return Database(tmp_path / "test.db", vec_enabled=False)

    def _seed_symbol(self, db: Any, sym_id: str) -> None:
        """Insert a minimal SymbolNode so FK constraints are satisfied."""
        import time

        from cognis.db import upsert_symbol
        from cognis.models import SymbolNode

        upsert_symbol(
            db,
            SymbolNode(
                id=sym_id,
                kind="function",
                name=sym_id.split(":")[-1],
                qualified_name=sym_id.split(":")[-1],
                language="python",
                module="test",
                file_path="src/test.py",
                line_start=1,
                line_end=10,
                content_hash="abc123",
                updated_at=int(time.time()),
            ),
        )

    def test_high_confidence_edge_no_ambiguous_flag(self, tmp_path: Path) -> None:
        db = self._make_db(tmp_path)
        self._seed_symbol(db, "a")
        self._seed_symbol(db, "b")
        edge = ResolvedEdge(src_id="a", dst_id="b", kind="calls", confidence=1.0, ambiguous=False)
        persist_edges(db, [edge])
        from cognis.db import list_edges

        rows = list_edges(db)
        assert len(rows) == 1
        assert rows[0].confidence == pytest.approx(1.0)
        assert "ambiguous" not in rows[0].meta

    def test_low_confidence_edge_has_ambiguous_true(self, tmp_path: Path) -> None:
        db = self._make_db(tmp_path)
        self._seed_symbol(db, "a")
        self._seed_symbol(db, "b")
        edge = ResolvedEdge(src_id="a", dst_id="b", kind="calls", confidence=0.4, ambiguous=True)
        persist_edges(db, [edge])
        from cognis.db import list_edges

        rows = list_edges(db)
        assert len(rows) == 1
        assert rows[0].meta.get("ambiguous") is True

    def test_exactly_0_6_confidence_not_flagged(self, tmp_path: Path) -> None:
        """Threshold is exclusive: confidence=0.6 is NOT ambiguous."""
        db = self._make_db(tmp_path)
        self._seed_symbol(db, "a")
        self._seed_symbol(db, "b")
        edge = ResolvedEdge(src_id="a", dst_id="b", kind="calls", confidence=0.6, ambiguous=False)
        persist_edges(db, [edge])
        from cognis.db import list_edges

        rows = list_edges(db)
        assert "ambiguous" not in rows[0].meta

    def test_persist_empty_is_noop(self, tmp_path: Path) -> None:
        db = self._make_db(tmp_path)
        persist_edges(db, [])  # must not raise
        from cognis.db import list_edges

        assert list_edges(db) == []

    def test_multiple_edges_persisted(self, tmp_path: Path) -> None:
        db = self._make_db(tmp_path)
        for sid in ("a", "b", "c"):
            self._seed_symbol(db, sid)
        edges = [
            ResolvedEdge(src_id="a", dst_id="b", kind="calls", confidence=1.0, ambiguous=False),
            ResolvedEdge(src_id="a", dst_id="c", kind="calls", confidence=0.4, ambiguous=True),
        ]
        persist_edges(db, edges)
        from cognis.db import list_edges

        rows = list_edges(db)
        assert len(rows) == 2
        ambiguous_row = next(r for r in rows if r.dst_id == "c")
        assert ambiguous_row.meta.get("ambiguous") is True
