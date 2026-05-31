"""Attribute extractor for the cognis enricher pipeline.

Detects side-effect and contract metadata from symbol body text using
regex patterns — no external parser dependency required at MVP.

Extracted attributes (matching :class:`cognis.models.SymbolAttribute` keys):

``db_table``
    SQL string literals — ``FROM``, ``JOIN``, ``INTO``, ``UPDATE``, ``TABLE``
    followed by an identifier.

``http_route``
    Decorator / middleware patterns for FastAPI/Flask (Python), Express/Hono
    (TypeScript), and Gin (Go).

``env_var``
    Environment-variable reads: ``os.environ[...]``, ``os.getenv(...)``,
    ``os.environ.get(...)``, ``process.env.X``, ``process.env["X"]``,
    ``os.Getenv("X")`` (Go).

``external_call``
    HTTP-client call sites: ``requests.*``, ``httpx.*``, ``fetch(``,
    ``axios.*``, ``http.Get/Post/Client`` (Go).

Design reference: design.md *Indexer Pipeline → Enricher*.
"""

from __future__ import annotations

import re
from dataclasses import dataclass, field

# ---------------------------------------------------------------------------
# Compiled patterns
# ---------------------------------------------------------------------------

# --- db_table ---------------------------------------------------------------
# Matches SQL keywords followed by a bare table name (word chars only).
# We apply this to strings/bodies but do NOT require the keyword to be inside
# a quote — the enricher calls this on the raw body_excerpt which may contain
# string literals as well as f-strings, raw SQL, etc.

_SQL_KEYWORD_RE = re.compile(
    r"""(?ix)
    (?:FROM|JOIN|INTO|UPDATE|TABLE)   # SQL keyword
    \s+
    (\w+)                              # table name (first word after keyword)
    """,
)

# --- http_route --------------------------------------------------------------
# Matches HTTP method + route path from decorator/middleware patterns.
# Group 1: HTTP method (GET/POST/PUT/PATCH/DELETE/HEAD/OPTIONS)
# Group 2: route path string (up to first quote/paren/space)
#
# Covers:
#   Python FastAPI/Flask: @router.get("/path"), @app.post("/path")
#   TypeScript Express:   router.get('/path', ...), app.post('/path'
#   Go Gin:               r.GET("/path", ...), router.POST("/path"

_HTTP_ROUTE_RE = re.compile(
    r"""(?ix)
    (?:GET|POST|PUT|PATCH|DELETE|HEAD|OPTIONS)  # HTTP method
    \s*\(\s*                                     # opening paren with optional spaces
    ['"\/]                                       # opening quote or forward-slash
    ([^'"\)\s]+)                                 # route path (no quote/paren/space)
    """,
)

# Also handle decorator-style: @router.GET("/path") or @app.get("/path")
_HTTP_DECORATOR_RE = re.compile(
    r"""(?ix)
    \.
    (get|post|put|patch|delete|head|options|route)  # method name
    \s*\(
    \s*['"\/]
    ([^'"\)\s]+)                                     # path
    """,
)

# --- env_var -----------------------------------------------------------------
# Python: os.environ["X"], os.environ['X'], os.getenv("X"), os.environ.get("X")
_ENV_PY_BRACKET_RE = re.compile(
    r"""os\.environ\[['"](\w+)['"]\]""",
)
_ENV_PY_GETENV_RE = re.compile(
    r"""os\.getenv\(\s*['"](\w+)['"]\s*(?:,|\))""",
)
_ENV_PY_GET_RE = re.compile(
    r"""os\.environ\.get\(\s*['"](\w+)['"]\s*(?:,|\))""",
)

# TypeScript/JS: process.env.VAR_NAME, process.env["VAR_NAME"]
_ENV_TS_DOT_RE = re.compile(
    r"""process\.env\.([A-Za-z_][A-Za-z0-9_]*)""",
)
_ENV_TS_BRACKET_RE = re.compile(
    r"""process\.env\[['"]([A-Za-z_][A-Za-z0-9_]*)['"]\]""",
)

# Go: os.Getenv("VAR_NAME")
_ENV_GO_RE = re.compile(
    r"""os\.Getenv\(\s*["']([A-Za-z_][A-Za-z0-9_]*)["']\s*\)""",
)

# --- external_call -----------------------------------------------------------
# Python requests/httpx
_EXT_REQUESTS_RE = re.compile(
    r"""(?<!\w)requests\.(get|post|put|delete|head|patch|request)\s*\(""",
    re.IGNORECASE,
)
_EXT_HTTPX_RE = re.compile(
    r"""(?<!\w)httpx\.(get|post|put|delete|head|patch|request|AsyncClient|Client)\s*[\(\.]""",
    re.IGNORECASE,
)

# TypeScript/JS fetch and axios
_EXT_FETCH_RE = re.compile(
    r"""(?<!\w)fetch\s*\(""",
)
_EXT_AXIOS_RE = re.compile(
    r"""(?<!\w)axios\.(get|post|put|delete|patch|create|request)\s*[\(\.]""",
    re.IGNORECASE,
)
_EXT_AXIOS_CREATE_RE = re.compile(
    r"""(?<!\w)axios\.create\s*\(""",
    re.IGNORECASE,
)

# Go net/http
_EXT_HTTP_GET_RE = re.compile(
    r"""(?<!\w)http\.Get\s*\(""",
)
_EXT_HTTP_POST_RE = re.compile(
    r"""(?<!\w)http\.Post\s*\(""",
)
_EXT_HTTP_CLIENT_RE = re.compile(
    r"""(?<!\w)http\.Client\b""",
)


# ---------------------------------------------------------------------------
# Public dataclass
# ---------------------------------------------------------------------------


@dataclass
class ExtractedAttribute:
    """A single extracted attribute value before it becomes a SymbolAttribute row."""

    key: str
    """One of: ``db_table``, ``http_route``, ``env_var``, ``external_call``."""

    value: str
    """The extracted value (table name, route path, var name, client type, ...)."""


# ---------------------------------------------------------------------------
# AttributeExtractor
# ---------------------------------------------------------------------------


class AttributeExtractor:
    """Extract side-effect and contract metadata from symbol body text.

    Usage::

        extractor = AttributeExtractor()
        attrs = extractor.extract(body_text)
        # attrs is a list[ExtractedAttribute]

    All methods are pure / stateless; a single instance can be safely reused
    across threads.
    """

    # Ordered list of (pattern, attribute_key, group_index) for simple
    # single-group patterns.  Complex patterns are handled in dedicated helpers.
    _SIMPLE_PATTERNS: list[tuple[re.Pattern[str], str, int]] = field(default_factory=list)

    def __init__(self) -> None:
        # Pre-built in __init__ so mypy sees concrete list
        self._env_patterns: list[tuple[re.Pattern[str], str]] = [
            (_ENV_PY_BRACKET_RE, "env_var"),
            (_ENV_PY_GETENV_RE, "env_var"),
            (_ENV_PY_GET_RE, "env_var"),
            (_ENV_TS_DOT_RE, "env_var"),
            (_ENV_TS_BRACKET_RE, "env_var"),
            (_ENV_GO_RE, "env_var"),
        ]

    # ------------------------------------------------------------------
    # Public API
    # ------------------------------------------------------------------

    def extract(self, body: str) -> list[ExtractedAttribute]:
        """Return all detected attributes in *body* (deduped by key+value).

        Args:
            body: Raw body text of a symbol (e.g. ``ParsedSymbol.body_excerpt``).

        Returns:
            List of :class:`ExtractedAttribute` instances, deduplicated.
        """
        if not body:
            return []

        seen: set[tuple[str, str]] = set()
        results: list[ExtractedAttribute] = []

        def _add(key: str, value: str) -> None:
            pair = (key, value)
            if pair not in seen:
                seen.add(pair)
                results.append(ExtractedAttribute(key=key, value=value))

        for attr in self._extract_db_tables(body):
            _add(attr.key, attr.value)
        for attr in self._extract_http_routes(body):
            _add(attr.key, attr.value)
        for attr in self._extract_env_vars(body):
            _add(attr.key, attr.value)
        for attr in self._extract_external_calls(body):
            _add(attr.key, attr.value)

        return results

    # ------------------------------------------------------------------
    # Private helpers
    # ------------------------------------------------------------------

    def _extract_db_tables(self, body: str) -> list[ExtractedAttribute]:
        results = []
        for match in _SQL_KEYWORD_RE.finditer(body):
            table_name = match.group(1)
            # Skip SQL reserved words that could follow a keyword:
            # e.g. "UPDATE SET", "FROM WHERE"
            if table_name.upper() not in {
                "SET",
                "WHERE",
                "VALUES",
                "SELECT",
                "AND",
                "OR",
                "ON",
                "AS",
                "IN",
                "BY",
                "NULL",
            }:
                results.append(ExtractedAttribute(key="db_table", value=table_name))
        return results

    def _extract_http_routes(self, body: str) -> list[ExtractedAttribute]:
        results = []
        # Method-then-path: GET("/path", ...) style (used in Gin r.GET, Express router.get)
        for match in _HTTP_ROUTE_RE.finditer(body):
            path = match.group(1)
            if path and path.startswith("/"):
                results.append(ExtractedAttribute(key="http_route", value=path))
        # Decorator style: .get("/path"), .post("/path")
        for match in _HTTP_DECORATOR_RE.finditer(body):
            path = match.group(2)
            if path and path.startswith("/"):
                results.append(ExtractedAttribute(key="http_route", value=path))
        return results

    def _extract_env_vars(self, body: str) -> list[ExtractedAttribute]:
        results = []
        for pattern, key in self._env_patterns:
            for match in pattern.finditer(body):
                results.append(ExtractedAttribute(key=key, value=match.group(1)))
        return results

    def _extract_external_calls(self, body: str) -> list[ExtractedAttribute]:
        results = []

        # Python requests
        for match in _EXT_REQUESTS_RE.finditer(body):
            results.append(
                ExtractedAttribute(
                    key="external_call",
                    value=f"requests.{match.group(1).lower()}",
                )
            )

        # Python httpx
        for match in _EXT_HTTPX_RE.finditer(body):
            results.append(
                ExtractedAttribute(
                    key="external_call",
                    value=f"httpx.{match.group(1).lower()}",
                )
            )

        # fetch (JS/TS)
        for _ in _EXT_FETCH_RE.finditer(body):
            results.append(ExtractedAttribute(key="external_call", value="fetch"))

        # axios
        for match in _EXT_AXIOS_RE.finditer(body):
            results.append(
                ExtractedAttribute(
                    key="external_call",
                    value=f"axios.{match.group(1).lower()}",
                )
            )

        # Go http
        for _ in _EXT_HTTP_GET_RE.finditer(body):
            results.append(ExtractedAttribute(key="external_call", value="http.Get"))
        for _ in _EXT_HTTP_POST_RE.finditer(body):
            results.append(ExtractedAttribute(key="external_call", value="http.Post"))
        for _ in _EXT_HTTP_CLIENT_RE.finditer(body):
            results.append(ExtractedAttribute(key="external_call", value="http.Client"))

        return results
