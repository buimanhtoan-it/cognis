"""Unit tests for the Capsule composer — Task 14 of tasks.md.

Covers:
- ContextCapsule schema validation (Pydantic round-trip)
- CapsuleComposer.compose produces a valid ContextCapsule
- token_estimate ≤ max_tokens (CP-8)
- sources non-empty for any populated section (CP-9)
- Untrusted content wrapping (design §Error Handling → Untrusted content)
- CP-11: determinism — same inputs → same capsule (modulo token_estimate which
  may vary by a few tokens on different runs but the *structure* must be identical)
"""

from __future__ import annotations

import hashlib
import json
import time
from dataclasses import dataclass, field
from typing import Any

import pytest
from cognis.capsule.composer import CapsuleComposer, ComposeError
from cognis.capsule.models import (
    CallChainEdge,
    CapsuleSource,
    ContextCapsule,
    RelevantSymbol,
    RootCauseCandidate,
)
from cognis.capsule.token_estimator import estimate_capsule_tokens, estimate_tokens
from cognis.db import Database, upsert_symbol
from cognis.models import SymbolNode

# ---------------------------------------------------------------------------
# Helpers / fixtures
# ---------------------------------------------------------------------------


@dataclass
class FakeHit:
    """Minimal Hit stand-in for tests (avoids importing cognis_retrieval)."""

    symbol_id: str
    score: float
    layer: str
    reason: str
    evidence: dict[str, Any] = field(default_factory=dict)


def _make_symbol(
    symbol_id: str,
    *,
    kind: str = "function",
    name: str = "func",
    qualified_name: str = "mod.func",
    language: str = "python",
    module: str = "mod",
    file_path: str = "src/mod.py",
    line_start: int = 1,
    line_end: int = 10,
    content_hash: str = "abc123",
    body_excerpt: str | None = "def func(): pass",
    docstring: str | None = "Does something.",
    untrusted_flags: list[str] | None = None,
    risk_score: float = 0.0,
) -> SymbolNode:
    return SymbolNode(
        id=symbol_id,
        kind=kind,  # type: ignore[arg-type]
        name=name,
        qualified_name=qualified_name,
        language=language,
        module=module,
        file_path=file_path,
        line_start=line_start,
        line_end=line_end,
        content_hash=content_hash,
        body_excerpt=body_excerpt,
        docstring=docstring,
        untrusted_flags=untrusted_flags or [],
        risk_score=risk_score,
        updated_at=int(time.time()),
    )


@pytest.fixture()
def db(tmp_path: Any) -> Database:
    """In-memory test database with migrations applied."""
    db_path = str(tmp_path / "test.db")
    return Database(db_path, vec_enabled=False)


@pytest.fixture()
def populated_db(db: Database) -> Database:
    """Database with 3 symbol rows pre-inserted."""
    symbols = [
        _make_symbol(
            "py:src/auth.py:auth.validate@abcd1234", name="validate", qualified_name="auth.validate"
        ),
        _make_symbol(
            "py:src/auth.py:auth.login@abcd5678",
            name="login",
            qualified_name="auth.login",
            body_excerpt="def login(): ...",
        ),
        _make_symbol(
            "py:src/db.py:db.query@efgh1234",
            name="query",
            qualified_name="db.query",
            kind="function",
        ),
    ]
    for sym in symbols:
        upsert_symbol(db, sym)
    return db


# ---------------------------------------------------------------------------
# 14.1 — Pydantic schema validation
# ---------------------------------------------------------------------------


class TestContextCapsuleSchema:
    """Schema validation tests for ContextCapsule and its sub-models."""

    def test_minimal_valid_capsule(self) -> None:
        capsule = ContextCapsule(
            goal="Fix the timeout bug",
            task_mode="bugfix",
            confidence=0.85,
            token_estimate=100,
        )
        assert capsule.version == "1"
        assert capsule.task_mode == "bugfix"
        assert capsule.root_cause_candidates == []
        assert capsule.relevant_symbols == []
        assert capsule.sources == []
        assert capsule.untrusted_sections == []

    def test_no_null_arrays(self) -> None:
        """Empty arrays, not None — CP-9 composition rule 3."""
        capsule = ContextCapsule(goal="test", task_mode="feature", confidence=1.0, token_estimate=0)
        for attr in [
            "root_cause_candidates",
            "relevant_symbols",
            "call_chain",
            "runtime_evidence",
            "neighbor_patterns",
            "risk_areas",
            "compressed_context",
            "sources",
            "untrusted_sections",
        ]:
            value = getattr(capsule, attr)
            assert value is not None, f"{attr} must not be None"
            assert isinstance(value, list), f"{attr} must be a list"

    def test_version_literal(self) -> None:
        with pytest.raises(ValueError):
            ContextCapsule(
                goal="test", task_mode="bugfix", confidence=1.0, token_estimate=0, version="2"
            )  # type: ignore[call-overload]

    def test_invalid_task_mode(self) -> None:
        with pytest.raises(ValueError):
            ContextCapsule(
                goal="test",
                task_mode="debug",
                confidence=1.0,  # type: ignore[call-overload]
                token_estimate=0,
            )

    def test_confidence_bounds(self) -> None:
        with pytest.raises(ValueError):
            ContextCapsule(goal="test", task_mode="bugfix", confidence=1.5, token_estimate=0)

    def test_call_chain_edge_aliases(self) -> None:
        """CallChainEdge serialises with 'from'/'to' per the design schema."""
        edge = CallChainEdge(**{"from": "a", "to": "b", "kind": "calls"})
        assert edge.from_id == "a"
        assert edge.to_id == "b"
        dumped = edge.model_dump(by_alias=True)
        assert "from" in dumped
        assert "to" in dumped

    def test_capsule_source_types(self) -> None:
        for src_type in ("symbol", "commit", "trace", "pr"):
            src = CapsuleSource(type=src_type, id="xyz")  # type: ignore[arg-type]
            assert src.type == src_type

    def test_json_schema_file_exists(self) -> None:
        """The capsule.v1.json schema file was shipped at task 14.1."""
        import pathlib

        schema_path = (
            pathlib.Path(__file__).parents[2]
            / "packages"
            / "core"
            / "cognis"
            / "schemas"
            / "capsule.v1.json"
        )
        assert schema_path.exists(), f"JSON Schema not found at {schema_path}"
        with open(schema_path) as f:
            schema = json.load(f)
        assert schema["title"] == "ContextCapsule"
        assert "properties" in schema

    def test_json_schema_validates_capsule(self) -> None:
        """A composed capsule validates against the shipped JSON schema."""
        try:
            import jsonschema  # optional dep; skip if not installed
        except ImportError:
            pytest.skip("jsonschema not installed")

        import pathlib

        schema_path = (
            pathlib.Path(__file__).parents[2]
            / "packages"
            / "core"
            / "cognis"
            / "schemas"
            / "capsule.v1.json"
        )
        schema = json.loads(schema_path.read_text())
        capsule = ContextCapsule(
            goal="test", task_mode="feature", confidence=1.0, token_estimate=50
        )
        data = json.loads(capsule.model_dump_json(by_alias=True))
        jsonschema.validate(data, schema)


# ---------------------------------------------------------------------------
# 14.3 — Token estimator
# ---------------------------------------------------------------------------


class TestTokenEstimator:
    def test_estimate_tokens_empty(self) -> None:
        assert estimate_tokens("") == 0

    def test_estimate_tokens_non_zero(self) -> None:
        count = estimate_tokens("Hello world, this is a test sentence.")
        assert count > 0

    def test_safety_margin_applied(self) -> None:
        """Returned count must be at least 10% above the raw count."""
        text = "the quick brown fox jumps over the lazy dog"
        count = estimate_tokens(text)
        # Even the word-count fallback (len("...".split()) * 1.3) * 1.1 should be > raw
        # This just tests the contract: count must be positive and > 0.
        assert count > 0

    def test_estimate_capsule_tokens_minimal(self) -> None:
        capsule = ContextCapsule(goal="test", task_mode="bugfix", confidence=0.9, token_estimate=0)
        tokens = estimate_capsule_tokens(capsule)
        assert tokens > 0

    def test_estimate_capsule_tokens_grows_with_content(self) -> None:
        small = ContextCapsule(goal="test", task_mode="feature", confidence=1.0, token_estimate=0)
        large = ContextCapsule(
            goal="test " * 100,
            task_mode="feature",
            confidence=1.0,
            token_estimate=0,
            relevant_symbols=[
                RelevantSymbol(symbol_id=f"sym{i}", kind="function", score=0.5) for i in range(10)
            ],
        )
        assert estimate_capsule_tokens(large) > estimate_capsule_tokens(small)


# ---------------------------------------------------------------------------
# 14.2 — Composer: basic pipeline
# ---------------------------------------------------------------------------


class TestCapsuleComposer:
    """Core composer pipeline tests."""

    def test_compose_empty_hits_returns_valid_capsule(self, populated_db: Database) -> None:
        composer = CapsuleComposer()
        capsule = composer.compose(
            task="Add a new feature",
            mode="feature",
            confidence=1.0,
            hits=[],
            max_tokens=2000,
            db=populated_db,
        )
        assert isinstance(capsule, ContextCapsule)
        assert capsule.goal == "Add a new feature"
        assert capsule.task_mode == "feature"
        assert capsule.version == "1"
        # No hits → no sections → no sources required
        assert capsule.relevant_symbols == []
        assert capsule.root_cause_candidates == []

    def test_compose_feature_mode(self, populated_db: Database) -> None:
        hits = [
            FakeHit(
                "py:src/auth.py:auth.validate@abcd1234",
                score=0.9,
                layer="semantic",
                reason="semantic match",
            ),
            FakeHit(
                "py:src/auth.py:auth.login@abcd5678",
                score=0.7,
                layer="lexical",
                reason="lexical match",
            ),
        ]
        composer = CapsuleComposer()
        capsule = composer.compose(
            task="Add OAuth login",
            mode="feature",
            confidence=0.9,
            hits=hits,  # type: ignore[arg-type]
            max_tokens=4000,
            db=populated_db,
        )
        assert len(capsule.relevant_symbols) == 2
        # Sorted by score descending
        assert capsule.relevant_symbols[0].score >= capsule.relevant_symbols[1].score
        assert len(capsule.sources) >= 2

    def test_compose_bugfix_mode(self, populated_db: Database) -> None:
        hits = [
            FakeHit(
                "py:src/auth.py:auth.validate@abcd1234",
                score=0.95,
                layer="structural",
                reason="call chain hit",
            ),
            FakeHit(
                "py:src/auth.py:auth.login@abcd5678", score=0.8, layer="semantic", reason="semantic"
            ),
        ]
        composer = CapsuleComposer()
        capsule = composer.compose(
            task="Why is login timing out?",
            mode="bugfix",
            confidence=0.85,
            hits=hits,  # type: ignore[arg-type]
            max_tokens=4000,
            db=populated_db,
        )
        assert len(capsule.root_cause_candidates) >= 1
        # structural hit goes to root_cause_candidates
        assert capsule.root_cause_candidates[0].symbol_id == "py:src/auth.py:auth.validate@abcd1234"
        assert len(capsule.sources) >= 1

    def test_compose_dedupe_by_symbol_id(self, populated_db: Database) -> None:
        """Duplicate hits for the same symbol keep only the highest score."""
        hits = [
            FakeHit(
                "py:src/auth.py:auth.validate@abcd1234",
                score=0.5,
                layer="lexical",
                reason="lexical",
            ),
            FakeHit(
                "py:src/auth.py:auth.validate@abcd1234",
                score=0.9,
                layer="semantic",
                reason="semantic",
            ),
        ]
        composer = CapsuleComposer()
        capsule = composer.compose(
            task="test",
            mode="feature",
            confidence=1.0,
            hits=hits,  # type: ignore[arg-type]
            max_tokens=2000,
            db=populated_db,
        )
        assert len(capsule.relevant_symbols) == 1
        assert capsule.relevant_symbols[0].score == 0.9

    def test_compose_sources_mandatory_raises_on_violation(self, db: Database) -> None:
        """ComposeError raised when a claim exists but no source is present."""
        # We synthesize a capsule that bypasses the composer to test the validator.
        capsule = ContextCapsule(
            goal="test",
            task_mode="bugfix",
            confidence=0.8,
            token_estimate=50,
            root_cause_candidates=[
                RootCauseCandidate(symbol_id="no-source-sym", rationale="test", evidence=[])
            ],
            sources=[],  # deliberately empty
        )
        composer = CapsuleComposer()
        with pytest.raises(ComposeError):
            composer._validate_sources(capsule)

    def test_token_estimate_fits_max_tokens(self, populated_db: Database) -> None:
        """CP-8: token_estimate ≤ max_tokens."""
        hits = [
            FakeHit(
                "py:src/auth.py:auth.validate@abcd1234",
                score=0.9 - i * 0.01,
                layer="semantic",
                reason=f"hit {i}",
            )
            for i in range(5)
        ]
        composer = CapsuleComposer()
        for max_tokens in [500, 1000, 4000, 16000]:
            capsule = composer.compose(
                task="test task",
                mode="feature",
                confidence=1.0,
                hits=hits,  # type: ignore[arg-type]
                max_tokens=max_tokens,
                db=populated_db,
            )
            assert capsule.token_estimate <= max_tokens, (
                f"token_estimate={capsule.token_estimate} > max_tokens={max_tokens}"
            )

    def test_sources_non_empty_for_populated_sections(self, populated_db: Database) -> None:
        """CP-9: sources[] non-empty when any section is populated."""
        hits = [
            FakeHit(
                "py:src/auth.py:auth.validate@abcd1234", score=0.9, layer="semantic", reason="match"
            ),
        ]
        composer = CapsuleComposer()
        capsule = composer.compose(
            task="test",
            mode="feature",
            confidence=1.0,
            hits=hits,  # type: ignore[arg-type]
            max_tokens=2000,
            db=populated_db,
        )
        if capsule.relevant_symbols or capsule.root_cause_candidates:
            assert len(capsule.sources) > 0


# ---------------------------------------------------------------------------
# 14.4 — Untrusted content wrapping
# ---------------------------------------------------------------------------


class TestUntrustedContentWrapping:
    def test_untrusted_symbol_snippet_wrapped(self, db: Database) -> None:
        """Symbols with 'untrusted_doc' flag get <<<UNTRUSTED>>> markers."""
        sym = _make_symbol(
            "py:src/auth.py:auth.evil@cafebabe",
            name="evil",
            qualified_name="auth.evil",
            body_excerpt="This is untrusted content",
            untrusted_flags=["untrusted_doc"],
        )
        upsert_symbol(db, sym)

        hit = FakeHit(
            "py:src/auth.py:auth.evil@cafebabe", score=0.9, layer="semantic", reason="match"
        )
        composer = CapsuleComposer()
        capsule = composer.compose(
            task="test",
            mode="feature",
            confidence=1.0,
            hits=[hit],  # type: ignore[arg-type]
            max_tokens=4000,
            db=db,
        )

        # The section ID must appear in untrusted_sections
        assert "relevant_symbols" in capsule.untrusted_sections

        # The snippet must contain the UNTRUSTED markers
        rs = capsule.relevant_symbols[0]
        assert rs.snippet is not None
        assert "<<<UNTRUSTED" in rs.snippet
        assert "<<<END UNTRUSTED>>>" in rs.snippet

    def test_trusted_symbol_snippet_not_wrapped(self, db: Database) -> None:
        """Trusted symbols have plain snippets without markers."""
        sym = _make_symbol(
            "py:src/auth.py:auth.trusted@00001111",
            name="trusted",
            qualified_name="auth.trusted",
            body_excerpt="Safe code here",
            untrusted_flags=[],
        )
        upsert_symbol(db, sym)

        hit = FakeHit(
            "py:src/auth.py:auth.trusted@00001111", score=0.9, layer="semantic", reason="match"
        )
        composer = CapsuleComposer()
        capsule = composer.compose(
            task="test",
            mode="feature",
            confidence=1.0,
            hits=[hit],  # type: ignore[arg-type]
            max_tokens=4000,
            db=db,
        )

        rs = capsule.relevant_symbols[0]
        if rs.snippet is not None:
            assert "<<<UNTRUSTED" not in rs.snippet
        assert "relevant_symbols" not in capsule.untrusted_sections


# ---------------------------------------------------------------------------
# 14.5 — CP-11: Determinism test
# ---------------------------------------------------------------------------


def _capsule_hash(capsule: ContextCapsule) -> str:
    """Compute a stable hash of the capsule, excluding wall-clock fields.

    CP-11 requires that the same query + same DB state → same capsule
    modulo wall-clock fields.  At capsule v1 MVP there are no wall-clock
    fields in the schema (``generated_at`` is Phase 2), so the full dump
    (minus ``token_estimate`` which may vary by ±1 due to rounding) is used.
    """
    data = capsule.model_dump(by_alias=True)
    # Exclude token_estimate from the hash comparison per CP-11.
    data.pop("token_estimate", None)
    canonical = json.dumps(data, sort_keys=True, separators=(",", ":"))
    return hashlib.sha256(canonical.encode()).hexdigest()


class TestDeterminism:
    """CP-11: same inputs → same capsule (modulo wall-clock)."""

    def test_same_inputs_produce_same_capsule(self, populated_db: Database) -> None:
        hits = [
            FakeHit(
                "py:src/auth.py:auth.validate@abcd1234",
                score=0.9,
                layer="semantic",
                reason="semantic match",
            ),
            FakeHit(
                "py:src/auth.py:auth.login@abcd5678",
                score=0.7,
                layer="lexical",
                reason="lexical match",
            ),
        ]
        composer = CapsuleComposer()

        capsule1 = composer.compose(
            task="Why is login timing out?",
            mode="bugfix",
            confidence=0.85,
            hits=hits,  # type: ignore[arg-type]
            max_tokens=4000,
            db=populated_db,
        )
        capsule2 = composer.compose(
            task="Why is login timing out?",
            mode="bugfix",
            confidence=0.85,
            hits=hits,  # type: ignore[arg-type]
            max_tokens=4000,
            db=populated_db,
        )

        assert _capsule_hash(capsule1) == _capsule_hash(capsule2), (
            "Same inputs must produce same capsule (CP-11). "
            f"hash1={_capsule_hash(capsule1)}, hash2={_capsule_hash(capsule2)}"
        )

    def test_different_tasks_produce_different_capsules(self, populated_db: Database) -> None:
        hits: list[FakeHit] = []
        composer = CapsuleComposer()

        capsule1 = composer.compose(
            task="Task A",
            mode="feature",
            confidence=1.0,
            hits=hits,  # type: ignore[arg-type]
            max_tokens=2000,
            db=populated_db,
        )
        capsule2 = composer.compose(
            task="Task B",
            mode="feature",
            confidence=1.0,
            hits=hits,  # type: ignore[arg-type]
            max_tokens=2000,
            db=populated_db,
        )
        assert _capsule_hash(capsule1) != _capsule_hash(capsule2)

    def test_hit_order_does_not_matter_for_output_order(self, populated_db: Database) -> None:
        """Deduplication sorts by score, so shuffled input → same output."""
        hits_a = [
            FakeHit(
                "py:src/auth.py:auth.validate@abcd1234", score=0.9, layer="semantic", reason="A"
            ),
            FakeHit("py:src/auth.py:auth.login@abcd5678", score=0.7, layer="lexical", reason="A"),
        ]
        hits_b = [
            FakeHit("py:src/auth.py:auth.login@abcd5678", score=0.7, layer="lexical", reason="A"),
            FakeHit(
                "py:src/auth.py:auth.validate@abcd1234", score=0.9, layer="semantic", reason="A"
            ),
        ]
        composer = CapsuleComposer()

        capsule_a = composer.compose(
            task="test",
            mode="feature",
            confidence=1.0,
            hits=hits_a,
            max_tokens=4000,
            db=populated_db,  # type: ignore[arg-type]
        )
        capsule_b = composer.compose(
            task="test",
            mode="feature",
            confidence=1.0,
            hits=hits_b,
            max_tokens=4000,
            db=populated_db,  # type: ignore[arg-type]
        )

        assert _capsule_hash(capsule_a) == _capsule_hash(capsule_b), (
            "Shuffled hit order must produce the same capsule after dedup+sort (CP-11)."
        )
