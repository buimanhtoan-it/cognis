"""Language parsers for the cognis indexer pipeline.

Exports the ``LanguageParser`` protocol, ``ParsedSymbol`` dataclass, and the
concrete parsers for TypeScript, Python, Go, C#, and Java.

Usage::

    from cognis_indexer.parsers import PythonParser, TypeScriptParser, GoParser

    parser = PythonParser()
    symbols = parser.parse(source_code, "src/app/main.py")
"""

from cognis_indexer.parsers.base import LanguageParser, ParsedSymbol
from cognis_indexer.parsers.csharp import CSharpParser
from cognis_indexer.parsers.go import GoParser
from cognis_indexer.parsers.java import JavaParser
from cognis_indexer.parsers.python import PythonParser
from cognis_indexer.parsers.typescript import TypeScriptParser

__all__ = [
    "CSharpParser",
    "GoParser",
    "JavaParser",
    "LanguageParser",
    "ParsedSymbol",
    "PythonParser",
    "TypeScriptParser",
]
