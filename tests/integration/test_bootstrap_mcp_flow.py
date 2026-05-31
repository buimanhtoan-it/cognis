"""Critical-path integration tests spanning CLI bootstrap and MCP queries.

These tests exercise the real operator path on a copied fixture repo:

1. ``cognis-cli bootstrap --skip-embeddings`` on a TypeScript fixture.
2. Verify the resulting UCKG is healthy and queryable.
3. Run MCP tools against the actual database produced by bootstrap.
"""

from __future__ import annotations

import json
import shutil
from collections.abc import Iterator
from pathlib import Path

import pytest
from click.testing import CliRunner

pytest.importorskip("tree_sitter_typescript")

from cognis.cli.main import cli
from cognis.db import Database

from tests.conftest import FIXTURES_ROOT

pytestmark = pytest.mark.integration

TS_FIXTURE = FIXTURES_ROOT / "repos" / "mini-ts-app"


@pytest.fixture(autouse=True)
def _reset_mcp_runtime(monkeypatch: pytest.MonkeyPatch) -> Iterator[None]:
    import cognis_mcpd.tools as tools
    from cognis_mcpd.embedder_pool import reset_shared_semantic_layer_for_tests
    from cognis_mcpd.result_cache import reset_cache_for_tests

    monkeypatch.delenv("COGNIS_DB_PATH", raising=False)
    monkeypatch.delenv("COGNIS_REPO_ROOT", raising=False)
    reset_cache_for_tests()
    reset_shared_semantic_layer_for_tests()
    tools._SEMANTIC_DISABLED_UNTIL = 0.0
    yield
    import cognis.db as db_module

    reset_cache_for_tests()
    reset_shared_semantic_layer_for_tests()
    tools._SEMANTIC_DISABLED_UNTIL = 0.0
    for db in list(tools._DB_CACHE.values()):
        db.close_thread_connection()
    tools._DB_CACHE.clear()
    thread_cache = getattr(db_module._THREAD_LOCAL, "cache", None)
    if thread_cache:
        for conn in list(thread_cache.values()):
            conn.close()
        thread_cache.clear()


def _invoke(runner: CliRunner, repo_root: Path, *args: str):
    return runner.invoke(cli, ["--repo-root", str(repo_root), *args])


def _copy_fixture_repo(tmp_path: Path) -> Path:
    repo_root = tmp_path / "mini-ts-app"
    shutil.copytree(TS_FIXTURE, repo_root)
    return repo_root


def _bootstrap_fixture_repo(repo_root: Path) -> dict[str, object]:
    runner = CliRunner()
    result = _invoke(runner, repo_root, "bootstrap", "--skip-embeddings", "--json", str(repo_root))
    assert result.exit_code == 0, result.output
    return json.loads(result.output)


def test_bootstrap_fixture_repo_produces_queryable_uckg(tmp_path: Path) -> None:
    """Bootstrap on the TS fixture should create a healthy, queryable database."""
    repo_root = _copy_fixture_repo(tmp_path)
    payload = _bootstrap_fixture_repo(repo_root)

    assert payload["command"] == "bootstrap"
    assert payload["skip_embeddings"] is True
    assert [phase["name"] for phase in payload["phases"]] == ["init", "index", "health"]
    assert payload["phases"][0]["status"] == "ok"
    assert payload["phases"][1]["status"] == "ok"
    assert payload["overall"] in {"ok", "warn"}

    db_path = Path(str(payload["db_path"]))
    assert db_path.exists()
    assert (repo_root / ".cognis" / "config.yaml").exists()
    assert (repo_root / ".cognis" / "config.revision").exists()

    db = Database(str(db_path))
    conn = db.connect()
    symbol_count = conn.execute("SELECT COUNT(*) FROM symbol").fetchone()[0]
    validate_row = conn.execute(
        "SELECT id FROM symbol WHERE name = 'validate' AND file_path = 'src/auth/jwt.ts'"
    ).fetchone()
    fts_count = conn.execute(
        "SELECT COUNT(*) FROM symbol_fts WHERE symbol_fts MATCH 'validate'"
    ).fetchone()[0]

    assert symbol_count >= 30
    assert validate_row is not None
    assert fts_count >= 1


def test_bootstrapped_repo_supports_real_mcp_queries(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """MCP tools should work against the DB produced by bootstrap, not only seeded test DBs."""
    repo_root = _copy_fixture_repo(tmp_path)
    payload = _bootstrap_fixture_repo(repo_root)
    db_path = Path(str(payload["db_path"]))

    monkeypatch.setenv("COGNIS_DB_PATH", str(db_path))
    monkeypatch.setenv("COGNIS_REPO_ROOT", str(repo_root))

    from cognis_mcpd.tools import dependency_trace, discover_symbols

    discover_hits = discover_symbols("login authentication jwt", k=8)
    assert isinstance(discover_hits, list)
    assert discover_hits, "expected auth/login discovery hits from the indexed fixture repo"
    hit_files = {str(hit.get("file_path")) for hit in discover_hits if hit.get("file_path")}
    assert {
        "src/auth/jwt.ts",
        "src/middleware/auth.ts",
        "src/routes/login.ts",
    } & hit_files, hit_files

    db = Database(str(db_path))
    conn = db.connect()
    validate_row = conn.execute(
        "SELECT id FROM symbol WHERE name = 'validate' AND file_path = 'src/auth/jwt.ts'"
    ).fetchone()
    assert validate_row is not None, "expected validate symbol in indexed fixture DB"

    trace = dependency_trace(str(validate_row["id"]), direction="in", depth=3)
    assert "error" not in trace, trace
    trace_files = {
        str(hit.get("file_path")) for hit in trace.get("hits", []) if hit.get("file_path")
    }
    assert "src/middleware/auth.ts" in trace_files
