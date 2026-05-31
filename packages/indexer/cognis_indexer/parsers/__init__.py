"""Language parsers for the cognis indexer pipeline.

Exports the ``LanguageParser`` protocol, ``ParsedSymbol`` dataclass, and the
three concrete parsers for TypeScript, Python, and Go.

Usage::

    from cognis_indexer.parsers import PythonParser, TypeScriptParser, GoParser

    parser = PythonParser()
    symbols = parser.parse(source_code, "src/app/main.py")
"""

from cognis_indexer.parsers.base import LanguageParser, ParsedSymbol
from cognis_indexer.parsers.go import GoParser
from cognis_indexer.parsers.python import PythonParser
from cognis_indexer.parsers.typescript import TypeScriptParser

__all__ = [
    "GoParser",
    "LanguageParser",
    "ParsedSymbol",
    "PythonParser",
    "TypeScriptParser",
]
