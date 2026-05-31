"""Shared fixtures for integration tests.

Integration tests are marked ``@pytest.mark.integration`` and excluded from
the default ``make test`` run. They exercise the full tool pipeline against a
known-state test database seeded with symbols from the ``mini-ts-app`` fixture.

Run with: ``pytest -m integration``
"""

from __future__ import annotations

import time
from pathlib import Path

import pytest

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

FIXTURE_REPO_DIR = Path(__file__).parent.parent / "fixtures" / "repos"

# Symbol ids planted in the test DB.
PLANTED_BUG_SYMBOL_ID = "ts:src/auth/jwt.ts:validate@deadbeef"
PLANTED_AUTH_SYMBOL_ID = "ts:src/middleware/auth.ts:requireAuth@cafebabe"
PLANTED_ROUTE_SYMBOL_ID = "ts:src/routes/login.ts:postLogin@feedface"


# ---------------------------------------------------------------------------
# DB fixture
# ---------------------------------------------------------------------------


@pytest.fixture()
def tmp_db(tmp_path: Path) -> object:
    """Create a temporary UCKG database seeded with mini-ts-app symbols."""
    from cognis.db import Database

    db_path = tmp_path / "uckg.db"
    db = Database(str(db_path))
    conn = db.connect()

    # Run the schema bootstrap so all tables exist.
    try:
        from cognis.db import run_migrations

        run_migrations(db)
    except (ImportError, Exception):
        # Fallback: create minimal schema inline if migration runner not yet wired.
        _bootstrap_minimal_schema(conn)

    _seed_mini_ts_app_symbols(conn)
    conn.commit()
    return db


def _bootstrap_minimal_schema(conn: object) -> None:  # type: ignore[misc]
    """Create minimal tables needed for integration tests."""
    import sqlite3 as _sqlite3  # noqa: F401 — keep for type compatibility

    conn.executescript(  # type: ignore[attr-defined]
        """
        CREATE TABLE IF NOT EXISTS meta (
            key   TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );
        INSERT OR IGNORE INTO meta VALUES ('index_version', '0.0.1.dev0');

        CREATE TABLE IF NOT EXISTS symbol (
            id              TEXT PRIMARY KEY,
            kind            TEXT NOT NULL,
            name            TEXT NOT NULL,
            qualified_name  TEXT NOT NULL,
            language        TEXT NOT NULL,
            module          TEXT NOT NULL,
            file_path       TEXT NOT NULL,
            line_start      INTEGER NOT NULL,
            line_end        INTEGER NOT NULL,
            signature       TEXT,
            docstring       TEXT,
            content_hash    TEXT NOT NULL,
            body_excerpt    TEXT,
            semantic_summary TEXT,
            risk_score      REAL DEFAULT 0.0,
            ambiguous       INTEGER DEFAULT 0,
            untrusted_flags TEXT,
            updated_at      INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_symbol_file ON symbol(file_path);
        CREATE INDEX IF NOT EXISTS idx_symbol_qname ON symbol(qualified_name);
        CREATE INDEX IF NOT EXISTS idx_symbol_kind  ON symbol(kind);

        CREATE TABLE IF NOT EXISTS edge (
            src_id      TEXT NOT NULL,
            dst_id      TEXT NOT NULL,
            kind        TEXT NOT NULL,
            confidence  REAL DEFAULT 1.0,
            meta        TEXT,
            PRIMARY KEY (src_id, dst_id, kind)
        );
        CREATE INDEX IF NOT EXISTS idx_edge_src ON edge(src_id, kind);
        CREATE INDEX IF NOT EXISTS idx_edge_dst ON edge(dst_id, kind);

        CREATE TABLE IF NOT EXISTS symbol_attribute (
            symbol_id TEXT NOT NULL,
            key       TEXT NOT NULL,
            value     TEXT NOT NULL,
            PRIMARY KEY (symbol_id, key, value)
        );

        CREATE TABLE IF NOT EXISTS file (
            path         TEXT PRIMARY KEY,
            language     TEXT NOT NULL,
            size_bytes   INTEGER NOT NULL,
            content_hash TEXT NOT NULL,
            parsed_at    INTEGER NOT NULL,
            parse_status TEXT NOT NULL
        );

        CREATE VIRTUAL TABLE IF NOT EXISTS symbol_fts
            USING fts5(
                id UNINDEXED, name, qualified_name, signature, docstring, body_excerpt,
                content='symbol', content_rowid='rowid'
            );
        """
    )


def _seed_mini_ts_app_symbols(conn: object) -> None:  # type: ignore[misc]
    """Insert the planted symbols for integration tests."""
    now = int(time.time())
    symbols = [
        # The planted auth-timeout bug symbol.
        (
            PLANTED_BUG_SYMBOL_ID,
            "function",
            "validate",
            "auth.jwt.validate",
            "typescript",
            "src/auth",
            "src/auth/jwt.ts",
            15,
            55,
            "function validate(token: string): JwtPayload",
            "Validates JWT tokens. Known auth-timeout bug: synchronous crypto blocks event loop.",
            "deadbeef",
            "async function validate(token: string): Promise<JwtPayload> {\n"
            "  // TODO: this blocks the event loop under high load\n"
            "  return jwt.verify(token, process.env.JWT_SECRET!);\n"
            "}",
            None,
            0.8,
            0,
            '["planted-bug"]',
            now,
        ),
        # Auth middleware.
        (
            PLANTED_AUTH_SYMBOL_ID,
            "function",
            "requireAuth",
            "middleware.auth.requireAuth",
            "typescript",
            "src/middleware",
            "src/middleware/auth.ts",
            8,
            30,
            "function requireAuth(req, res, next): void",
            "Express middleware that validates JWT and attaches user to request.",
            "cafebabe",
            "export function requireAuth(req: Request, res: Response, next: NextFunction) {\n"
            "  const token = req.headers.authorization?.split(' ')[1];\n"
            "  const payload = validate(token);\n"
            "  req.user = payload;\n"
            "  next();\n"
            "}",
            None,
            0.5,
            0,
            None,
            now,
        ),
        # Login route.
        (
            PLANTED_ROUTE_SYMBOL_ID,
            "function",
            "postLogin",
            "routes.login.postLogin",
            "typescript",
            "src/routes",
            "src/routes/login.ts",
            20,
            45,
            "async function postLogin(req: Request, res: Response): Promise<void>",
            "POST /login handler — authenticates user and issues JWT.",
            "feedface",
            "router.post('/login', async (req, res) => {\n"
            "  const { username, password } = req.body;\n"
            "  // ... auth logic ...\n"
            "  const token = jwt.sign(payload, process.env.JWT_SECRET!);\n"
            "  res.json({ token });\n"
            "});",
            None,
            0.3,
            0,
            None,
            now,
        ),
        # Additional symbols for tool coverage.
        (
            "ts:src/app.ts:createApp@1111aaaa",
            "function",
            "createApp",
            "app.createApp",
            "typescript",
            "src",
            "src/app.ts",
            1,
            40,
            "function createApp(): Express",
            "Creates and configures the Express application.",
            "1111aaaa",
            "export function createApp(): Express {\n"
            "  const app = express();\n"
            "  app.use(express.json());\n"
            "  registerRoutes(app);\n"
            "  return app;\n"
            "}",
            None,
            0.2,
            0,
            None,
            now,
        ),
    ]

    conn.executemany(
        """
        INSERT OR IGNORE INTO symbol (
            id, kind, name, qualified_name, language, module, file_path,
            line_start, line_end, signature, docstring, content_hash,
            body_excerpt, semantic_summary, risk_score, ambiguous,
            untrusted_flags, updated_at
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        """,
        symbols,
    )

    # Insert edges: postLogin → requireAuth → validate (the call chain).
    edges = [
        (PLANTED_ROUTE_SYMBOL_ID, PLANTED_AUTH_SYMBOL_ID, "calls", 1.0),
        (PLANTED_AUTH_SYMBOL_ID, PLANTED_BUG_SYMBOL_ID, "calls", 1.0),
        ("ts:src/app.ts:createApp@1111aaaa", PLANTED_ROUTE_SYMBOL_ID, "calls", 0.9),
    ]
    conn.executemany(
        "INSERT OR IGNORE INTO edge (src_id, dst_id, kind, confidence) VALUES (?, ?, ?, ?)",
        edges,
    )

    # Insert FTS entries.
    import contextlib

    for sym in symbols:
        parts = list(sym)
        sym_id = parts[0]
        name = parts[2]
        qname = parts[3]
        signature = parts[9]
        docstring = parts[10]
        body_excerpt = parts[12]
        with contextlib.suppress(Exception):
            conn.execute(  # type: ignore[attr-defined]
                "INSERT OR IGNORE INTO symbol_fts (id, name, qualified_name, signature, docstring, body_excerpt)"
                " VALUES (?, ?, ?, ?, ?, ?)",
                (sym_id, name, qname, signature, docstring, body_excerpt),
            )
