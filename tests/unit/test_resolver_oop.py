"""Unit tests for the C#/Java OOP relationship resolver.

Parses real source with the language parsers, then asserts the
``OOPRelationshipResolver`` emits the right ``inherits`` / ``implements`` edges
and stays silent for external types and non-OOP languages.
"""

from __future__ import annotations

import pytest

try:
    from cognis_indexer.parsers.csharp import CSharpParser
    from cognis_indexer.parsers.java import JavaParser
    from cognis_indexer.parsers.python import PythonParser
    from cognis_indexer.resolver.oop import OOPRelationshipResolver

    _AVAILABLE = True
except ImportError:
    _AVAILABLE = False

pytestmark = pytest.mark.unit

skip_if_no_parsers = pytest.mark.skipif(
    not _AVAILABLE, reason="tree-sitter optional deps not installed"
)


def _edges_by_name(symbols, edges):
    """Map edges to ``(src_name, dst_name, kind)`` triples for easy assertions."""
    id_to_name = {s.id: s.name for s in symbols}
    return {(id_to_name.get(e.src_id), id_to_name.get(e.dst_id), e.kind) for e in edges}


@skip_if_no_parsers
class TestOOPResolverCSharp:
    def _resolve(self, src: str, path: str):
        syms = CSharpParser().parse(src, path)
        return syms, OOPRelationshipResolver().resolve(syms)

    def test_implements_interface(self) -> None:
        src = (
            "public interface IValidator { bool Validate(string t); }\n"
            "public class JwtValidator : IValidator {\n"
            "    public bool Validate(string t) { return true; }\n"
            "}\n"
        )
        syms, edges = self._resolve(src, "src/Auth.cs")
        triples = _edges_by_name(syms, edges)
        assert ("JwtValidator", "IValidator", "implements") in triples

    def test_inherits_base_class(self) -> None:
        src = "public class Base {}\npublic class Derived : Base {}\n"
        syms, edges = self._resolve(src, "src/Types.cs")
        triples = _edges_by_name(syms, edges)
        assert ("Derived", "Base", "inherits") in triples

    def test_base_then_interface_mixed_list(self) -> None:
        src = "public class Base {}\npublic interface IFoo {}\npublic class C : Base, IFoo {}\n"
        syms, edges = self._resolve(src, "src/Mixed.cs")
        triples = _edges_by_name(syms, edges)
        assert ("C", "Base", "inherits") in triples
        assert ("C", "IFoo", "implements") in triples

    def test_external_base_produces_no_edge(self) -> None:
        # System.Object is not in the batch → no edge.
        src = "public class Widget : System.Object {}\n"
        _syms, edges = self._resolve(src, "src/Widget.cs")
        assert edges == []

    def test_generic_base_resolves_simple_name(self) -> None:
        src = "public class Repository {}\npublic class UserRepository : Repository {}\n"
        syms, edges = self._resolve(src, "src/Repo.cs")
        triples = _edges_by_name(syms, edges)
        assert ("UserRepository", "Repository", "inherits") in triples


@skip_if_no_parsers
class TestOOPResolverJava:
    def _resolve(self, src: str, path: str):
        syms = JavaParser().parse(src, path)
        return syms, OOPRelationshipResolver().resolve(syms)

    def test_extends_and_implements(self) -> None:
        src = "class Base {}\ninterface IFoo {}\npublic class C extends Base implements IFoo {}\n"
        syms, edges = self._resolve(src, "src/C.java")
        triples = _edges_by_name(syms, edges)
        assert ("C", "Base", "inherits") in triples
        assert ("C", "IFoo", "implements") in triples

    def test_implements_only(self) -> None:
        src = (
            "interface IValidator { boolean validate(String t); }\n"
            "public class JwtValidator implements IValidator {\n"
            "    public boolean validate(String t) { return true; }\n"
            "}\n"
        )
        syms, edges = self._resolve(src, "src/Auth.java")
        triples = _edges_by_name(syms, edges)
        assert ("JwtValidator", "IValidator", "implements") in triples

    def test_external_super_produces_no_edge(self) -> None:
        src = "public class Widget extends Object {}\n"
        _syms, edges = self._resolve(src, "src/Widget.java")
        assert edges == []


@skip_if_no_parsers
class TestOOPResolverScope:
    def test_non_oop_language_is_ignored(self) -> None:
        # Python symbols must not produce OOP edges from this resolver.
        syms = PythonParser().parse("class A:\n    pass\nclass B(A):\n    pass\n", "src/m.py")
        edges = OOPRelationshipResolver().resolve(syms)
        assert edges == []

    def test_empty_batch(self) -> None:
        assert OOPRelationshipResolver().resolve([]) == []
