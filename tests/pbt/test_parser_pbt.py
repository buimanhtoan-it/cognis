"""Property-Based Tests for the language parsers — CP-1 and CP-2.

**Validates: Requirements 1.1, 1.2, 2.1**

CP-1 (Index idempotency):
    Parsing the same source twice produces an identical symbol set.

CP-2 (Symbol id stability under cosmetic edits):
    - Whitespace-only edits MUST NOT change ``symbol.id``.
    - Comment-only edits MUST NOT change ``symbol.id``.
    - Structural edits (rename, body change) MUST change ``content_hash``.

Run these with::

    pytest tests/pbt/test_parser_pbt.py -m pbt

They are intentionally excluded from the default ``pytest`` run because
Hypothesis may take seconds per example.
"""

from __future__ import annotations

import pytest
from hypothesis import given, settings
from hypothesis import strategies as st

# ---------------------------------------------------------------------------
# Skip if optional tree-sitter deps not installed
# ---------------------------------------------------------------------------
try:
    from cognis_indexer.parsers._normalize import content_hash
    from cognis_indexer.parsers.go import GoParser
    from cognis_indexer.parsers.python import PythonParser
    from cognis_indexer.parsers.typescript import TypeScriptParser

    _AVAILABLE = True
except ImportError:
    _AVAILABLE = False

pytestmark = [pytest.mark.pbt]

skip_if_unavailable = pytest.mark.skipif(
    not _AVAILABLE,
    reason="tree-sitter optional deps not installed",
)

# ---------------------------------------------------------------------------
# Fixture source snippets used as stable inputs for cosmetic-edit tests
# ---------------------------------------------------------------------------

_PY_SNIPPET = """\
def authenticate(token: str, secret: str) -> bool:
    if not token:
        return False
    return token == secret
"""

_TS_SNIPPET = """\
export function validate(token: string, secret: string): boolean {
    if (!token) return false;
    return token === secret;
}
"""

_GO_SNIPPET = """\
package auth

func Validate(token string, secret string) bool {
    if token == "" {
        return false
    }
    return token == secret
}
"""


# ---------------------------------------------------------------------------
# Helpers to produce cosmetic variants
# ---------------------------------------------------------------------------


def _add_trailing_whitespace(src: str) -> str:
    """Add trailing spaces to every line."""
    return "\n".join(line + "   " for line in src.splitlines()) + "\n"


def _extra_blank_lines(src: str) -> str:
    """Insert blank lines between every existing line."""
    return "\n\n".join(src.splitlines()) + "\n"


def _add_python_comment(src: str) -> str:
    """Prepend a Python line comment to the first line."""
    return "# a cosmetic comment\n" + src


def _add_js_comment(src: str) -> str:
    """Prepend a JS/TS block comment to the first line."""
    return "/* cosmetic comment */\n" + src


def _add_go_comment(src: str) -> str:
    """Prepend a Go line comment."""
    return "// cosmetic comment\n" + src


# ---------------------------------------------------------------------------
# Strategies
# ---------------------------------------------------------------------------

# Simple whitespace variants that should NOT change the hash
_ws_variants = st.one_of(
    st.just(_add_trailing_whitespace),
    st.just(_extra_blank_lines),
    st.just(lambda s: s.replace("    ", "  ")),  # indent change
)

_comment_variants_py = st.just(_add_python_comment)
_comment_variants_ts = st.just(_add_js_comment)
_comment_variants_go = st.just(_add_go_comment)


# ===========================================================================
# CP-1: Index idempotency (parsing same content twice yields same set)
# ===========================================================================


@skip_if_unavailable
class TestCP1Idempotency:
    """CP-1: ∀ file, P(file) == P(P(file)) at the id level."""

    def test_python_idempotent(self) -> None:
        p = PythonParser()
        first = p.parse(_PY_SNIPPET, "src/auth.py")
        second = p.parse(_PY_SNIPPET, "src/auth.py")
        assert {s.id for s in first} == {s.id for s in second}

    def test_typescript_idempotent(self) -> None:
        p = TypeScriptParser()
        first = p.parse(_TS_SNIPPET, "src/auth.ts")
        second = p.parse(_TS_SNIPPET, "src/auth.ts")
        assert {s.id for s in first} == {s.id for s in second}

    def test_go_idempotent(self) -> None:
        p = GoParser()
        first = p.parse(_GO_SNIPPET, "internal/auth/jwt.go")
        second = p.parse(_GO_SNIPPET, "internal/auth/jwt.go")
        assert {s.id for s in first} == {s.id for s in second}

    @given(extra_ws=st.text(alphabet="\t \n", min_size=0, max_size=10))
    @settings(max_examples=50)
    def test_python_idempotent_pbt(self, extra_ws: str) -> None:
        """**Validates: Requirements 1.2** CP-1 property for Python."""
        p = PythonParser()
        src = _PY_SNIPPET + extra_ws
        first = p.parse(src, "src/auth.py")
        second = p.parse(src, "src/auth.py")
        assert {s.id for s in first} == {s.id for s in second}

    @given(extra_ws=st.text(alphabet="\t \n", min_size=0, max_size=10))
    @settings(max_examples=50)
    def test_typescript_idempotent_pbt(self, extra_ws: str) -> None:
        """**Validates: Requirements 1.2** CP-1 property for TypeScript."""
        p = TypeScriptParser()
        src = _TS_SNIPPET + extra_ws
        first = p.parse(src, "src/auth.ts")
        second = p.parse(src, "src/auth.ts")
        assert {s.id for s in first} == {s.id for s in second}

    @given(extra_ws=st.text(alphabet="\t \n", min_size=0, max_size=10))
    @settings(max_examples=50)
    def test_go_idempotent_pbt(self, extra_ws: str) -> None:
        """**Validates: Requirements 1.2** CP-1 property for Go."""
        p = GoParser()
        src = _GO_SNIPPET + extra_ws
        first = p.parse(src, "internal/auth/jwt.go")
        second = p.parse(src, "internal/auth/jwt.go")
        assert {s.id for s in first} == {s.id for s in second}


# ===========================================================================
# CP-2: Symbol id stability under cosmetic edits
# ===========================================================================


@skip_if_unavailable
class TestCP2IdStability:
    """CP-2: cosmetic edits MUST NOT change content_hash; structural edits MUST."""

    # --- Whitespace-only edits ---

    def test_py_whitespace_same_hash(self) -> None:
        """**Validates: Requirements 1.1, 2.1** — whitespace-only edits don't churn IDs."""
        a = content_hash(_PY_SNIPPET)
        b = content_hash(_add_trailing_whitespace(_PY_SNIPPET))
        assert a == b, "trailing whitespace changed content_hash"

    def test_py_blank_lines_same_hash(self) -> None:
        a = content_hash(_PY_SNIPPET)
        b = content_hash(_extra_blank_lines(_PY_SNIPPET))
        assert a == b, "extra blank lines changed content_hash"

    def test_ts_whitespace_same_hash(self) -> None:
        a = content_hash(_TS_SNIPPET)
        b = content_hash(_add_trailing_whitespace(_TS_SNIPPET))
        assert a == b

    def test_go_whitespace_same_hash(self) -> None:
        a = content_hash(_GO_SNIPPET)
        b = content_hash(_add_trailing_whitespace(_GO_SNIPPET))
        assert a == b

    # --- Comment-only edits ---

    def test_py_comment_same_hash(self) -> None:
        a = content_hash(_PY_SNIPPET)
        b = content_hash(_add_python_comment(_PY_SNIPPET))
        assert a == b, "Python comment changed content_hash"

    def test_ts_block_comment_same_hash(self) -> None:
        a = content_hash(_TS_SNIPPET)
        b = content_hash(_add_js_comment(_TS_SNIPPET))
        assert a == b

    def test_go_comment_same_hash(self) -> None:
        a = content_hash(_GO_SNIPPET)
        b = content_hash(_add_go_comment(_GO_SNIPPET))
        assert a == b

    # --- Structural edits MUST change hash ---

    def test_rename_changes_hash(self) -> None:
        a = content_hash(_PY_SNIPPET)
        renamed = _PY_SNIPPET.replace("authenticate", "login")
        b = content_hash(renamed)
        assert a != b, "renaming function should change content_hash"

    def test_signature_change_changes_hash(self) -> None:
        a = content_hash(_PY_SNIPPET)
        modified = _PY_SNIPPET.replace("token: str, secret: str", "token: str")
        b = content_hash(modified)
        assert a != b

    def test_body_change_changes_hash(self) -> None:
        a = content_hash(_GO_SNIPPET)
        modified = _GO_SNIPPET.replace("return token == secret", "return true")
        b = content_hash(modified)
        assert a != b

    # --- PBT: random whitespace appended to fixture body yields same hash ---

    @given(
        suffix=st.text(
            alphabet=st.sampled_from([" ", "\t", "\n"]),
            min_size=0,
            max_size=20,
        )
    )
    @settings(max_examples=100)
    def test_py_random_whitespace_same_hash(self, suffix: str) -> None:
        """**Validates: Requirements 1.1, 2.1** CP-2 whitespace property for normalize_body."""
        # Strip suffix of non-whitespace by construction
        # Only whitespace chars so the normalized form should be identical
        base = content_hash(_PY_SNIPPET)
        variant = content_hash(_PY_SNIPPET + suffix)
        assert base == variant

    @given(
        suffix=st.text(
            alphabet=st.sampled_from([" ", "\t", "\n"]),
            min_size=0,
            max_size=20,
        )
    )
    @settings(max_examples=100)
    def test_ts_random_whitespace_same_hash(self, suffix: str) -> None:
        """**Validates: Requirements 1.1, 2.1** CP-2 whitespace property for TypeScript."""
        base = content_hash(_TS_SNIPPET)
        variant = content_hash(_TS_SNIPPET + suffix)
        assert base == variant

    @given(
        suffix=st.text(
            alphabet=st.sampled_from([" ", "\t", "\n"]),
            min_size=0,
            max_size=20,
        )
    )
    @settings(max_examples=100)
    def test_go_random_whitespace_same_hash(self, suffix: str) -> None:
        """**Validates: Requirements 1.1, 2.1** CP-2 whitespace property for Go."""
        base = content_hash(_GO_SNIPPET)
        variant = content_hash(_GO_SNIPPET + suffix)
        assert base == variant

    # --- PBT: parser-level symbol ID stability under whitespace edits ---

    @given(
        variant=st.sampled_from(
            [
                _add_trailing_whitespace(_PY_SNIPPET),
                _extra_blank_lines(_PY_SNIPPET),
                "# leading comment\n" + _PY_SNIPPET,
                _PY_SNIPPET + "\n\n",
            ]
        )
    )
    @settings(max_examples=20)
    def test_python_parser_ids_stable_under_whitespace(self, variant: str) -> None:
        """**Validates: Requirements 1.1, 2.1** CP-2: parser IDs stable for Python."""
        p = PythonParser()
        orig = p.parse(_PY_SNIPPET, "src/auth.py")
        edited = p.parse(variant, "src/auth.py")
        # The same function names should have the same content_hash
        orig_hashes = {s.name: s.content_hash for s in orig}
        edited_hashes = {s.name: s.content_hash for s in edited}
        for name in orig_hashes:
            if name in edited_hashes:
                assert orig_hashes[name] == edited_hashes[name], (
                    f"content_hash changed for {name!r} under cosmetic edit"
                )

    @given(
        variant=st.sampled_from(
            [
                _add_trailing_whitespace(_TS_SNIPPET),
                _extra_blank_lines(_TS_SNIPPET),
                "// leading comment\n" + _TS_SNIPPET,
                _TS_SNIPPET + "\n\n",
            ]
        )
    )
    @settings(max_examples=20)
    def test_typescript_parser_ids_stable_under_whitespace(self, variant: str) -> None:
        """**Validates: Requirements 1.1, 2.1** CP-2: parser IDs stable for TypeScript."""
        p = TypeScriptParser()
        orig = p.parse(_TS_SNIPPET, "src/auth.ts")
        edited = p.parse(variant, "src/auth.ts")
        orig_hashes = {s.name: s.content_hash for s in orig}
        edited_hashes = {s.name: s.content_hash for s in edited}
        for name in orig_hashes:
            if name in edited_hashes:
                assert orig_hashes[name] == edited_hashes[name], (
                    f"content_hash changed for {name!r} under cosmetic edit"
                )

    @given(
        variant=st.sampled_from(
            [
                _add_trailing_whitespace(_GO_SNIPPET),
                _extra_blank_lines(_GO_SNIPPET),
                "// leading comment\n" + _GO_SNIPPET,
                _GO_SNIPPET + "\n\n",
            ]
        )
    )
    @settings(max_examples=20)
    def test_go_parser_ids_stable_under_whitespace(self, variant: str) -> None:
        """**Validates: Requirements 1.1, 2.1** CP-2: parser IDs stable for Go."""
        p = GoParser()
        orig = p.parse(_GO_SNIPPET, "internal/auth/jwt.go")
        edited = p.parse(variant, "internal/auth/jwt.go")
        orig_hashes = {s.name: s.content_hash for s in orig}
        edited_hashes = {s.name: s.content_hash for s in edited}
        for name in orig_hashes:
            if name in edited_hashes:
                assert orig_hashes[name] == edited_hashes[name], (
                    f"content_hash changed for {name!r} under cosmetic edit"
                )
