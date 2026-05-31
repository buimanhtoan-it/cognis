"""Unit tests for the tree-sitter language parsers (task 6.1-6.5).

These tests use inline source strings and do NOT depend on fixture files,
making them fast and self-contained.

Marks:
- ``unit`` — all tests here.
- Imports guarded: tests are skipped gracefully if tree-sitter packages are
  absent (optional dependency group).
"""

from __future__ import annotations

import pytest

# ---------------------------------------------------------------------------
# Guard: skip entire module if tree-sitter optional deps are not installed.
# ---------------------------------------------------------------------------
try:
    from cognis_indexer.parsers.go import GoParser
    from cognis_indexer.parsers.python import PythonParser
    from cognis_indexer.parsers.typescript import TypeScriptParser

    _TS_AVAILABLE = True
except ImportError:
    _TS_AVAILABLE = False

pytestmark = pytest.mark.unit

skip_if_no_parsers = pytest.mark.skipif(
    not _TS_AVAILABLE,
    reason="tree-sitter optional deps not installed",
)


# ===========================================================================
# Helper
# ===========================================================================


def names(symbols: list) -> list[str]:
    """Return list of symbol names from a parse result."""
    return [s.name for s in symbols]


def qnames(symbols: list) -> list[str]:
    return [s.qualified_name for s in symbols]


def kinds(symbols: list) -> list[str]:
    return [s.kind for s in symbols]


# ===========================================================================
# Task 6.1 — base.py protocol
# ===========================================================================


@skip_if_no_parsers
class TestLanguageParserProtocol:
    """Verify the LanguageParser protocol is satisfied by all concrete parsers."""

    def test_python_satisfies_protocol(self) -> None:
        from cognis_indexer.parsers.base import LanguageParser

        p = PythonParser()
        assert isinstance(p, LanguageParser)
        assert p.language == "python"

    def test_typescript_satisfies_protocol(self) -> None:
        from cognis_indexer.parsers.base import LanguageParser

        p = TypeScriptParser()
        assert isinstance(p, LanguageParser)
        assert p.language == "typescript"

    def test_go_satisfies_protocol(self) -> None:
        from cognis_indexer.parsers.base import LanguageParser

        p = GoParser()
        assert isinstance(p, LanguageParser)
        assert p.language == "go"

    def test_parsed_symbol_line_range(self) -> None:
        p = PythonParser()
        syms = p.parse("def foo():\n    pass\n", "src/foo.py")
        assert len(syms) == 1
        assert syms[0].line_range == (syms[0].line_start, syms[0].line_end)


# ===========================================================================
# Task 6.2 — TypeScript parser
# ===========================================================================


@skip_if_no_parsers
class TestTypeScriptParser:
    """Tests for TypeScriptParser covering all required node types."""

    def _p(self) -> TypeScriptParser:
        return TypeScriptParser()

    def test_function_declaration(self) -> None:
        src = "export function validate(token: string): boolean { return true; }"
        syms = self._p().parse(src, "src/auth/jwt.ts")
        assert "validate" in names(syms)
        sym = next(s for s in syms if s.name == "validate")
        assert sym.kind == "function"
        assert sym.qualified_name == "ts:src/auth/jwt.ts:validate"

    def test_interface_declaration(self) -> None:
        src = "export interface AccessTokenClaims { sub: string; }"
        syms = self._p().parse(src, "src/auth/jwt.ts")
        assert "AccessTokenClaims" in names(syms)
        sym = next(s for s in syms if s.name == "AccessTokenClaims")
        assert sym.kind == "interface"

    def test_class_declaration_with_methods(self) -> None:
        src = """
export class JwtError extends Error {
    constructor(msg: string, public code: string) { super(msg); }
    get name() { return 'JwtError'; }
}
"""
        syms = self._p().parse(src, "src/auth/jwt.ts")
        assert "JwtError" in names(syms)
        cls_sym = next(s for s in syms if s.name == "JwtError")
        assert cls_sym.kind == "class"
        # Methods should be extracted
        method_names = [s.name for s in syms if s.kind == "method"]
        assert "constructor" in method_names or "name" in method_names

    def test_arrow_function_assigned_to_const(self) -> None:
        src = "export const loadConfig = (path: string) => ({ port: 3000 });"
        syms = self._p().parse(src, "src/utils/secrets.ts")
        assert "loadConfig" in names(syms)
        sym = next(s for s in syms if s.name == "loadConfig")
        assert sym.kind == "function"

    def test_exported_symbols_detected(self) -> None:
        src = """
export function sign(input: SignInput): string { return ''; }
function internal_helper(): void {}
"""
        syms = self._p().parse(src, "src/auth/jwt.ts")
        # Both should be parsed; export detection is at the symbol level
        assert "sign" in names(syms)

    def test_non_exported_function_still_parsed(self) -> None:
        src = "function base64UrlEncode(buf: Buffer): string { return ''; }"
        syms = self._p().parse(src, "src/auth/jwt.ts")
        assert "base64UrlEncode" in names(syms)

    def test_multiple_classes_and_interfaces(self) -> None:
        src = """
export class HttpError extends Error {}
export class ValidationError extends Error {}
export interface ErrorResponse { message: string; }
"""
        syms = self._p().parse(src, "src/middleware/errorHandler.ts")
        assert "HttpError" in names(syms)
        assert "ValidationError" in names(syms)
        assert "ErrorResponse" in names(syms)

    def test_content_hash_present(self) -> None:
        src = "export function foo(): void {}"
        syms = self._p().parse(src, "src/foo.ts")
        assert len(syms) == 1
        assert len(syms[0].content_hash) == 16

    def test_line_numbers(self) -> None:
        src = "\n\nexport function foo(): void {}"
        syms = self._p().parse(src, "src/foo.ts")
        assert len(syms) == 1
        assert syms[0].line_start == 3

    def test_id_format(self) -> None:
        src = "export function foo(): void {}"
        syms = self._p().parse(src, "src/foo.ts")
        sym = syms[0]
        assert sym.id.startswith("ts:src/foo.ts:foo@")
        assert len(sym.id.split("@")[-1]) == 16

    def test_empty_source(self) -> None:
        syms = self._p().parse("", "src/empty.ts")
        assert syms == []

    def test_parse_failure_returns_empty(self) -> None:
        """Completely invalid source should not raise; returns empty or partial."""
        try:
            syms = self._p().parse("this is not valid typescript !!@#$", "bad.ts")
            # Either empty or partial is acceptable
            assert isinstance(syms, list)
        except Exception as exc:  # broad catch is intentional for parse-failure test
            pytest.fail(f"Parser raised unexpectedly: {exc}")

    def test_default_export_function(self) -> None:
        src = "export default function handler(): void {}"
        syms = self._p().parse(src, "src/handler.ts")
        assert "handler" in names(syms)


# ===========================================================================
# Task 6.3 — Python parser
# ===========================================================================


@skip_if_no_parsers
class TestPythonParser:
    """Tests for PythonParser covering all required node types."""

    def _p(self) -> PythonParser:
        return PythonParser()

    def test_sync_function(self) -> None:
        src = "def create_app(config=None):\n    pass\n"
        syms = self._p().parse(src, "src/app/main.py")
        assert "create_app" in names(syms)
        sym = next(s for s in syms if s.name == "create_app")
        assert sym.kind == "function"

    def test_async_function(self) -> None:
        src = "async def lifespan(app):\n    yield\n"
        syms = self._p().parse(src, "src/app/main.py")
        assert "lifespan" in names(syms)
        sym = next(s for s in syms if s.name == "lifespan")
        assert sym.kind == "function"
        assert "async" in (sym.signature or "")

    def test_class_definition(self) -> None:
        src = "class Settings:\n    debug: bool = False\n"
        syms = self._p().parse(src, "src/app/config.py")
        assert "Settings" in names(syms)
        sym = next(s for s in syms if s.name == "Settings")
        assert sym.kind == "class"

    def test_class_with_methods(self) -> None:
        src = """
class User:
    def __init__(self, name: str):
        self.name = name

    def greet(self) -> str:
        return f'Hello {self.name}'
"""
        syms = self._p().parse(src, "src/db/users_repo.py")
        assert "User" in names(syms)
        assert "__init__" in names(syms)
        assert "greet" in names(syms)
        # Methods should have kind="method"
        method_syms = [s for s in syms if s.name in ("__init__", "greet")]
        for ms in method_syms:
            assert ms.kind == "method"

    def test_decorated_function(self) -> None:
        src = "@app.get('/health')\nasync def health_check():\n    return {}\n"
        syms = self._p().parse(src, "src/api/health.py")
        assert "health_check" in names(syms)

    def test_all_caps_assignment_becomes_const(self) -> None:
        src = "JWT_SECRET = 'my-secret'\n"
        syms = self._p().parse(src, "src/app/security.py")
        assert "JWT_SECRET" in names(syms)
        sym = next(s for s in syms if s.name == "JWT_SECRET")
        assert sym.kind == "const"

    def test_lowercase_assignment_not_extracted(self) -> None:
        src = "my_var = 42\n"
        syms = self._p().parse(src, "src/foo.py")
        assert "my_var" not in names(syms)

    def test_docstring_extraction(self) -> None:
        src = 'def foo():\n    """This is a docstring."""\n    pass\n'
        syms = self._p().parse(src, "src/foo.py")
        assert len(syms) == 1
        assert syms[0].docstring is not None
        assert "docstring" in syms[0].docstring

    def test_qualified_name_includes_class_prefix(self) -> None:
        src = "class Foo:\n    def bar(self): pass\n"
        syms = self._p().parse(src, "src/foo.py")
        method = next(s for s in syms if s.name == "bar")
        assert "Foo.bar" in method.qualified_name

    def test_multiple_all_caps(self) -> None:
        src = "A = 1\nJWT_SECRET = 'x'\nJWT_FALLBACK_SECRET = 'y'\nPAYLOAD_PEPPER = 'z'\n"
        syms = self._p().parse(src, "src/app/security.py")
        const_names = [s.name for s in syms if s.kind == "const"]
        assert "JWT_SECRET" in const_names
        assert "JWT_FALLBACK_SECRET" in const_names
        assert "PAYLOAD_PEPPER" in const_names
        # Single-letter "A" should not be treated as const
        assert "A" not in const_names

    def test_content_hash_present(self) -> None:
        src = "def foo(): pass\n"
        syms = self._p().parse(src, "src/foo.py")
        assert len(syms[0].content_hash) == 16

    def test_empty_source(self) -> None:
        syms = self._p().parse("", "src/empty.py")
        assert syms == []


# ===========================================================================
# Task 6.4 — Go parser
# ===========================================================================


@skip_if_no_parsers
class TestGoParser:
    """Tests for GoParser covering all required node types."""

    def _p(self) -> GoParser:
        return GoParser()

    def test_top_level_function(self) -> None:
        src = "package main\n\nfunc NewValidator(secret string) *Validator { return nil }\n"
        syms = self._p().parse(src, "internal/auth/jwt.go")
        assert "NewValidator" in names(syms)
        sym = next(s for s in syms if s.name == "NewValidator")
        assert sym.kind == "function"

    def test_method_with_pointer_receiver(self) -> None:
        src = "package auth\n\nfunc (v *Validator) Validate(token string) error { return nil }\n"
        syms = self._p().parse(src, "internal/auth/jwt.go")
        assert "Validate" in names(syms)
        sym = next(s for s in syms if s.name == "Validate")
        assert sym.kind == "method"
        assert "Validator.Validate" in sym.qualified_name

    def test_method_with_value_receiver(self) -> None:
        src = "package main\n\nfunc (c Config) Addr() string { return '' }\n"
        syms = self._p().parse(src, "internal/config/config.go")
        assert "Addr" in names(syms)
        sym = next(s for s in syms if s.name == "Addr")
        assert sym.kind == "method"
        assert "Config.Addr" in sym.qualified_name

    def test_struct_type(self) -> None:
        src = "package auth\n\ntype Validator struct { secret string }\n"
        syms = self._p().parse(src, "internal/auth/jwt.go")
        assert "Validator" in names(syms)
        sym = next(s for s in syms if s.name == "Validator")
        assert sym.kind == "class"

    def test_interface_type(self) -> None:
        src = "package auth\n\ntype Authenticator interface { Authenticate(token string) error }\n"
        syms = self._p().parse(src, "internal/auth/jwt.go")
        assert "Authenticator" in names(syms)
        sym = next(s for s in syms if s.name == "Authenticator")
        assert sym.kind == "interface"

    def test_qualified_name_for_method(self) -> None:
        src = "package db\n\nfunc (r *OrderRepo) Insert(o Order) error { return nil }\n"
        syms = self._p().parse(src, "internal/db/repo.go")
        sym = next(s for s in syms if s.name == "Insert")
        assert sym.qualified_name == "go:internal/db/repo.go:OrderRepo.Insert"

    def test_content_hash_present(self) -> None:
        src = "package main\n\nfunc Foo() {}\n"
        syms = self._p().parse(src, "cmd/server/main.go")
        assert len(syms[0].content_hash) == 16

    def test_empty_source(self) -> None:
        syms = self._p().parse("", "empty.go")
        assert syms == []

    def test_exported_uppercase(self) -> None:
        src = "package main\n\nfunc Exported() {}\nfunc unexported() {}\n"
        syms = self._p().parse(src, "main.go")
        sym_names = names(syms)
        assert "Exported" in sym_names
        assert "unexported" in sym_names  # both are parsed; export is a convention


# ===========================================================================
# Task 6.5 — content_hash / normalize
# ===========================================================================


class TestNormalize:
    """Tests for the _normalize module (content hash and ID stability)."""

    def test_same_content_same_hash(self) -> None:
        from cognis_indexer.parsers._normalize import content_hash

        assert content_hash("def foo(): pass") == content_hash("def foo(): pass")

    def test_whitespace_only_diff_same_hash(self) -> None:
        from cognis_indexer.parsers._normalize import content_hash

        a = "def foo():  \n\t  pass\n"
        b = "def foo(): pass"
        assert content_hash(a) == content_hash(b)

    def test_comment_only_diff_same_hash(self) -> None:
        from cognis_indexer.parsers._normalize import content_hash

        a = "def foo(): # inline comment\n    pass"
        b = "def foo():\n    pass"
        assert content_hash(a) == content_hash(b)

    def test_block_comment_same_hash(self) -> None:
        from cognis_indexer.parsers._normalize import content_hash

        a = "func foo() { /* do stuff */ return nil }"
        b = "func foo() {  return nil }"
        assert content_hash(a) == content_hash(b)

    def test_structural_edit_changes_hash(self) -> None:
        from cognis_indexer.parsers._normalize import content_hash

        a = "def foo(): pass"
        b = "def bar(): pass"
        assert content_hash(a) != content_hash(b)

    def test_signature_change_changes_hash(self) -> None:
        from cognis_indexer.parsers._normalize import content_hash

        a = "def foo(x): pass"
        b = "def foo(x, y): pass"
        assert content_hash(a) != content_hash(b)

    def test_hash_is_16_hex_chars(self) -> None:
        from cognis_indexer.parsers._normalize import content_hash

        h = content_hash("anything")
        assert len(h) == 16
        assert all(c in "0123456789abcdef" for c in h)

    def test_make_symbol_id_format(self) -> None:
        from cognis_indexer.parsers._normalize import make_symbol_id

        sym_id = make_symbol_id("ts", "src/auth/jwt.ts", "validate", "function body text")
        parts = sym_id.split("@")
        assert len(parts) == 2
        assert parts[0] == "ts:src/auth/jwt.ts:validate"
        assert len(parts[1]) == 16

    def test_normalize_strips_triple_docstring(self) -> None:
        from cognis_indexer.parsers._normalize import content_hash

        a = 'def foo():\n    """This docstring."""\n    pass'
        b = "def foo():\n    pass"
        assert content_hash(a) == content_hash(b)
