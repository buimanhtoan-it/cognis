"""Unit tests for ``cognis.cli.main`` (task 2.2).

Covers:

- ``--version`` / ``-V`` exit cleanly and print the runtime version.
- ``--help`` lists every subcommand declared by the spec.
- ``init`` materializes the full ``.cognis/`` layout from ``Config.default()``.
- ``init`` is idempotent and respects ``--force``.
- ``health`` reports per-section status (config, db, embedder, version) and
  honours ``--json``.
- Every stub subcommand prints a not-yet-implemented message and exits 0 so
  CI smoke tests can shell out without spurious failures.
- ``main(["--version"])`` returns 0 (regression cover for the original
  scaffold test ``test_cli_main_returns_zero_on_version_flag``).
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest
import yaml
from click.testing import CliRunner
from cognis import __version__
from cognis.cli.main import (
    CAPSULE_CACHE_DIRNAME,
    DEFAULT_DB_FILENAME,
    cli,
    main,
)
from cognis.config import (
    CONFIG_DIR_NAME,
    CONFIG_FILE_NAME,
    CONFIG_REVISION,
    CONFIG_REVISION_FILE_NAME,
    Config,
)

# ---------------------------------------------------------------------------
# Environment isolation
# ---------------------------------------------------------------------------


@pytest.fixture(autouse=True)
def _clear_cognis_db_path_env(monkeypatch: pytest.MonkeyPatch) -> None:
    """Prevent leaked ``COGNIS_DB_PATH`` from the developer shell breaking unit tests."""
    monkeypatch.delenv("COGNIS_DB_PATH", raising=False)


# ---------------------------------------------------------------------------
# main() entry point
# ---------------------------------------------------------------------------


@pytest.mark.unit
def test_cli_main_returns_zero_on_version_flag(capsys: pytest.CaptureFixture[str]) -> None:
    """Regression cover for the original scaffold test: ``main(["--version"])`` exits 0."""
    rc = main(["--version"])
    captured = capsys.readouterr()

    assert rc == 0
    assert "cognis-cli" in captured.out
    assert __version__ in captured.out


@pytest.mark.unit
def test_cli_main_returns_zero_on_short_version_flag(capsys: pytest.CaptureFixture[str]) -> None:
    rc = main(["-V"])
    captured = capsys.readouterr()

    assert rc == 0
    assert "cognis-cli" in captured.out
    assert __version__ in captured.out


@pytest.mark.unit
def test_cli_main_returns_zero_on_help() -> None:
    """``main(['--help'])`` exits 0 (Click's normal help path)."""
    runner = CliRunner()
    result = runner.invoke(cli, ["--help"])
    assert result.exit_code == 0
    # Every subcommand the spec demands is advertised.
    for subcmd in (
        "init",
        "bootstrap",
        "paths",
        "mcp-config",
        "index",
        "eval",
        "health",
        "up",
        "down",
        "mcp-conformance",
        "profile",
    ):
        assert subcmd in result.output


@pytest.mark.unit
def test_cli_main_unknown_subcommand_exits_nonzero() -> None:
    rc = main(["does-not-exist"])
    assert rc != 0


# ---------------------------------------------------------------------------
# init
# ---------------------------------------------------------------------------


def _invoke(runner: CliRunner, repo_root: Path, *args: str) -> object:
    """Invoke the CLI with an explicit ``--repo-root`` (test isolation)."""
    return runner.invoke(cli, ["--repo-root", str(repo_root), *args])


@pytest.mark.unit
def test_init_creates_full_layout(tmp_path: Path) -> None:
    runner = CliRunner()
    result = _invoke(runner, tmp_path, "init")

    assert result.exit_code == 0, result.output  # type: ignore[attr-defined]

    cognis_dir = tmp_path / CONFIG_DIR_NAME
    assert cognis_dir.is_dir()
    assert (cognis_dir / CONFIG_FILE_NAME).is_file()
    assert (cognis_dir / CONFIG_REVISION_FILE_NAME).is_file()
    assert (cognis_dir / CAPSULE_CACHE_DIRNAME).is_dir()
    assert (cognis_dir / "audit.log").is_file()
    assert (cognis_dir / "eval" / "golden.jsonl").is_file()


@pytest.mark.unit
def test_init_writes_config_default_yaml(tmp_path: Path) -> None:
    runner = CliRunner()
    result = _invoke(runner, tmp_path, "init")
    assert result.exit_code == 0, result.output  # type: ignore[attr-defined]

    cfg_path = tmp_path / CONFIG_DIR_NAME / CONFIG_FILE_NAME
    written = cfg_path.read_text(encoding="utf-8")

    # Round-trips back to Config.default() (no semantic drift).
    assert Config.from_yaml_str(written) == Config.default()
    # And the textual form matches Config.default().to_yaml() byte-for-byte
    # (the task spec calls this out explicitly).
    assert written == Config.default().to_yaml()
    # Sanity check: it's actually YAML, not JSON.
    parsed = yaml.safe_load(written)
    assert isinstance(parsed, dict)
    assert "embedder" in parsed
    assert (tmp_path / CONFIG_DIR_NAME / CONFIG_REVISION_FILE_NAME).read_text(
        encoding="utf-8"
    ).strip() == str(CONFIG_REVISION)


@pytest.mark.unit
def test_init_is_idempotent(tmp_path: Path) -> None:
    """Re-running ``init`` preserves user-edited config and golden set."""
    runner = CliRunner()
    assert _invoke(runner, tmp_path, "init").exit_code == 0  # type: ignore[attr-defined]

    cfg_path = tmp_path / CONFIG_DIR_NAME / CONFIG_FILE_NAME
    cfg_path.write_text("planner:\n  default_max_tokens: 4321\n", encoding="utf-8")
    (tmp_path / CONFIG_DIR_NAME / CONFIG_REVISION_FILE_NAME).unlink()
    golden_path = tmp_path / CONFIG_DIR_NAME / "eval" / "golden.jsonl"
    golden_path.write_text('{"id":"q1"}\n', encoding="utf-8")

    result = _invoke(runner, tmp_path, "init")
    assert result.exit_code == 0  # type: ignore[attr-defined]

    # User edits survived.
    migrated = cfg_path.read_text(encoding="utf-8")
    assert "default_max_tokens: 4321" in migrated
    assert "reference" in migrated
    assert "discover_symbols" in migrated
    assert golden_path.read_text(encoding="utf-8") == '{"id":"q1"}\n'
    assert (tmp_path / CONFIG_DIR_NAME / CONFIG_REVISION_FILE_NAME).read_text(
        encoding="utf-8"
    ).strip() == str(CONFIG_REVISION)


@pytest.mark.unit
def test_init_can_skip_config_migration(tmp_path: Path) -> None:
    runner = CliRunner()
    assert _invoke(runner, tmp_path, "init").exit_code == 0  # type: ignore[attr-defined]

    cfg_path = tmp_path / CONFIG_DIR_NAME / CONFIG_FILE_NAME
    cfg_path.write_text(
        "repo:\n  ignore:\n    - node_modules\nmcp:\n  allow_tools:\n    - symbol_lookup\n",
        encoding="utf-8",
    )
    (tmp_path / CONFIG_DIR_NAME / CONFIG_REVISION_FILE_NAME).unlink()

    result = _invoke(runner, tmp_path, "init", "--no-migrate")
    assert result.exit_code == 0  # type: ignore[attr-defined]

    preserved = cfg_path.read_text(encoding="utf-8")
    assert "reference" not in preserved
    assert "discover_symbols" not in preserved
    assert not (tmp_path / CONFIG_DIR_NAME / CONFIG_REVISION_FILE_NAME).exists()


@pytest.mark.unit
def test_init_force_overwrites_config(tmp_path: Path) -> None:
    runner = CliRunner()
    assert _invoke(runner, tmp_path, "init").exit_code == 0  # type: ignore[attr-defined]

    cfg_path = tmp_path / CONFIG_DIR_NAME / CONFIG_FILE_NAME
    cfg_path.write_text("# user edited\n", encoding="utf-8")

    result = _invoke(runner, tmp_path, "init", "--force")
    assert result.exit_code == 0  # type: ignore[attr-defined]
    assert cfg_path.read_text(encoding="utf-8") == Config.default().to_yaml()


# ---------------------------------------------------------------------------
# health
# ---------------------------------------------------------------------------


@pytest.mark.unit
def test_health_uninitialized_repo_warns_but_succeeds(tmp_path: Path) -> None:
    """A bare repo with no ``.cognis/`` is a warn (not fail) — exit 0."""
    runner = CliRunner()
    result = _invoke(runner, tmp_path, "health")

    assert result.exit_code == 0, result.output  # type: ignore[attr-defined]
    assert __version__ in result.output  # type: ignore[attr-defined]
    # Each section appears in the human-readable summary.
    for section in ("config", "db", "embedder", "version"):
        assert section in result.output  # type: ignore[attr-defined]
    assert "overall:" in result.output  # type: ignore[attr-defined]


@pytest.mark.unit
def test_health_after_init_each_check_present_json(tmp_path: Path) -> None:
    runner = CliRunner()
    assert _invoke(runner, tmp_path, "init").exit_code == 0  # type: ignore[attr-defined]

    result = _invoke(runner, tmp_path, "health", "--json")
    assert result.exit_code == 0, result.output  # type: ignore[attr-defined]

    payload = json.loads(result.output)  # type: ignore[attr-defined]
    assert payload["runtime_version"] == __version__
    assert payload["overall"] in {"ok", "warn", "fail"}
    assert set(payload["checks"]) == {"config", "db", "index", "vector", "embedder", "version"}
    for check in payload["checks"].values():
        assert check["status"] in {"ok", "warn", "fail"}
        assert isinstance(check["message"], str) and check["message"]


@pytest.mark.unit
def test_health_warns_when_config_defaults_are_stale(tmp_path: Path) -> None:
    runner = CliRunner()
    cognis_dir = tmp_path / CONFIG_DIR_NAME
    cognis_dir.mkdir()
    (cognis_dir / CONFIG_FILE_NAME).write_text(
        yaml.safe_dump(
            {
                "repo": {"ignore": ["node_modules", ".git"]},
                "mcp": {"allow_tools": ["symbol_lookup", "semantic_search"]},
            }
        ),
        encoding="utf-8",
    )

    result = _invoke(runner, tmp_path, "health", "--json")
    assert result.exit_code == 0, result.output  # type: ignore[attr-defined]

    payload = json.loads(result.output)  # type: ignore[attr-defined]
    assert payload["checks"]["config"]["status"] == "warn"
    assert "stale defaults are pending" in payload["checks"]["config"]["message"]


@pytest.mark.unit
def test_health_version_says_not_initialized_when_no_db(tmp_path: Path) -> None:
    """Per task spec: when DB is absent, version check says 'not initialized'."""
    runner = CliRunner()
    result = _invoke(runner, tmp_path, "health", "--json")
    assert result.exit_code == 0  # type: ignore[attr-defined]

    payload = json.loads(result.output)  # type: ignore[attr-defined]
    msg = payload["checks"]["version"]["message"].lower()
    assert "not initialized" in msg


@pytest.mark.unit
def _seed_minimal_symbol_table(conn: object) -> None:
    import sqlite3 as _sqlite3

    assert isinstance(conn, _sqlite3.Connection)
    """Create a one-row ``symbol`` table so readiness checks pass."""
    conn.execute(
        """
        CREATE TABLE symbol (
            id TEXT PRIMARY KEY, kind TEXT NOT NULL, name TEXT NOT NULL,
            qualified_name TEXT NOT NULL, language TEXT NOT NULL, module TEXT NOT NULL,
            file_path TEXT NOT NULL, line_start INTEGER NOT NULL, line_end INTEGER NOT NULL,
            signature TEXT, docstring TEXT, content_hash TEXT NOT NULL,
            body_excerpt TEXT, semantic_summary TEXT,
            risk_score REAL DEFAULT 0.0, ambiguous INTEGER DEFAULT 0,
            untrusted_flags TEXT, updated_at INTEGER NOT NULL
        )
        """
    )
    conn.execute(
        """
        INSERT INTO symbol VALUES (
            'test:sym', 'function', 'fn', 'fn', 'python', 'mod', 'f.py',
            1, 2, NULL, NULL, 'abc', NULL, NULL, 0.0, 0, NULL, 0
        )
        """
    )


def test_health_version_matches_when_db_records_runtime_version(tmp_path: Path) -> None:
    """When ``meta.index_version`` matches ``cognis.__version__`` the check is ok."""
    import sqlite3

    runner = CliRunner()
    assert _invoke(runner, tmp_path, "init").exit_code == 0  # type: ignore[attr-defined]

    db_path = tmp_path / CONFIG_DIR_NAME / DEFAULT_DB_FILENAME
    conn = sqlite3.connect(db_path)
    try:
        conn.execute("CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
        conn.execute("INSERT INTO meta (key, value) VALUES ('index_version', ?)", (__version__,))
        _seed_minimal_symbol_table(conn)
        conn.commit()
    finally:
        conn.close()

    result = _invoke(runner, tmp_path, "health", "--json")
    assert result.exit_code == 0  # type: ignore[attr-defined]
    payload = json.loads(result.output)  # type: ignore[attr-defined]
    assert payload["checks"]["version"]["status"] == "ok"
    assert __version__ in payload["checks"]["version"]["message"]


@pytest.mark.unit
def test_health_version_fails_on_drift(tmp_path: Path) -> None:
    """When ``meta.index_version`` differs the check fails (re-index required)."""
    import sqlite3

    runner = CliRunner()
    assert _invoke(runner, tmp_path, "init").exit_code == 0  # type: ignore[attr-defined]

    db_path = tmp_path / CONFIG_DIR_NAME / DEFAULT_DB_FILENAME
    conn = sqlite3.connect(db_path)
    try:
        conn.execute("CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
        conn.execute("INSERT INTO meta (key, value) VALUES ('index_version', '0.0.0.dev999')")
        _seed_minimal_symbol_table(conn)
        conn.commit()
    finally:
        conn.close()

    result = _invoke(runner, tmp_path, "health", "--json")
    assert result.exit_code == 1  # type: ignore[attr-defined]
    payload = json.loads(result.output)  # type: ignore[attr-defined]
    assert payload["checks"]["version"]["status"] == "fail"
    assert "differs" in payload["checks"]["version"]["message"]
    assert payload["overall"] == "fail"


# ---------------------------------------------------------------------------
# Stub subcommands
# ---------------------------------------------------------------------------


@pytest.mark.unit
@pytest.mark.parametrize(
    "args",
    [
        ["up"],
        ["up", "--no-detach"],
        ["down"],
        ["profile"],
        ["profile", "--target", "planner", "--iterations", "5"],
    ],
)
def test_stub_subcommands_exit_zero_with_message(
    tmp_path: Path, args: list[str], monkeypatch: pytest.MonkeyPatch
) -> None:
    """Stubs exit 0; ``up``/``down`` without compose file stay stubbed in isolated tmp repos."""
    monkeypatch.setattr("cognis.cli.main._find_compose_file", lambda _r: None)
    runner = CliRunner()
    result = _invoke(runner, tmp_path, *args)
    assert result.exit_code == 0, result.output  # type: ignore[attr-defined]
    if args[0] in ("up", "down"):
        assert "deploy/compose.yaml" in result.output  # type: ignore[attr-defined]
    else:
        assert "not yet implemented" in result.output  # type: ignore[attr-defined]
        assert "docs/quickstart.md" in result.output  # type: ignore[attr-defined]


@pytest.mark.unit
@pytest.mark.parametrize(
    "subcmd",
    [
        "init",
        "bootstrap",
        "paths",
        "mcp-config",
        "index",
        "eval",
        "health",
        "up",
        "down",
        "mcp-conformance",
        "profile",
    ],
)
def test_subcommand_help_works(subcmd: str) -> None:
    """``cognis-cli <sub> --help`` exits 0 and shows the docstring."""
    runner = CliRunner()
    result = runner.invoke(cli, [subcmd, "--help"])
    assert result.exit_code == 0
    assert "Usage:" in result.output


# ---------------------------------------------------------------------------
# index command (real wiring of the IndexerPipeline)
# ---------------------------------------------------------------------------


@pytest.mark.unit
def test_paths_emits_json_workspace_layout(tmp_path: Path) -> None:
    """``cognis-cli paths`` returns resolved paths for extensions."""
    runner = CliRunner()
    _invoke(runner, tmp_path, "init")
    result = _invoke(runner, tmp_path, "paths")
    assert result.exit_code == 0  # type: ignore[attr-defined]
    payload = json.loads(result.output)  # type: ignore[attr-defined]
    assert payload["repo_root"] == str(tmp_path.resolve())
    assert payload["db_path"].endswith("uckg.db")
    assert payload["indexd_status_path"].endswith("indexd-status.json")
    assert "commands" in payload
    assert payload["commands"]["cognis_cli_module"] == "cognis.cli.main"


@pytest.mark.unit
def test_doctor_reports_prerequisite_checklist(tmp_path: Path) -> None:
    """``cognis-cli doctor`` returns a structured prerequisite checklist."""
    runner = CliRunner()
    result = _invoke(runner, tmp_path, "doctor")
    assert result.exit_code == 0, result.output  # type: ignore[attr-defined]
    payload = json.loads(result.output)  # type: ignore[attr-defined]

    # Top-level contract the extension's PrerequisiteReport type depends on.
    assert isinstance(payload["ready"], bool)
    assert "combined_install_target" in payload
    assert isinstance(payload["items"], list) and payload["items"]

    ids = {item["id"] for item in payload["items"]}
    # The required-for-setup groups must always be present in the checklist.
    assert {"indexer", "embed_local", "mcp"} <= ids

    for item in payload["items"]:
        assert item["status"] in {"ok", "missing"}
        assert isinstance(item["required"], bool)
        assert item["install_target"].startswith(".[")
        assert item["label"] and item["description"] and item["detail"]


@pytest.mark.unit
def test_doctor_flags_missing_required_module(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """When a required module is unimportable, doctor marks the item missing."""
    import importlib.util as importlib_util

    real_find_spec = importlib_util.find_spec

    def fake_find_spec(name: str, *args: object, **kwargs: object) -> object:
        if name == "fastmcp":
            return None  # simulate MCP server not installed
        return real_find_spec(name, *args, **kwargs)  # type: ignore[arg-type]

    monkeypatch.setattr("cognis.cli.main.importlib.util.find_spec", fake_find_spec)

    runner = CliRunner()
    result = _invoke(runner, tmp_path, "doctor")
    assert result.exit_code == 0, result.output  # type: ignore[attr-defined]
    payload = json.loads(result.output)  # type: ignore[attr-defined]

    assert payload["ready"] is False
    mcp_item = next(item for item in payload["items"] if item["id"] == "mcp")
    assert mcp_item["status"] == "missing"
    assert "fastmcp" in mcp_item["detail"]
    assert "mcp" in payload["combined_install_target"]


@pytest.mark.unit
def test_mcp_config_emits_mcp_servers_block(tmp_path: Path) -> None:
    """``cognis-cli mcp-config`` returns mcpServers JSON for IDE hosts."""
    runner = CliRunner()
    _invoke(runner, tmp_path, "init")
    result = _invoke(runner, tmp_path, "mcp-config", "--host", "cursor")
    assert result.exit_code == 0  # type: ignore[attr-defined]
    payload = json.loads(result.output)  # type: ignore[attr-defined]
    assert payload["host"] == "cursor"
    server_name = payload["server_name"]
    assert server_name.startswith("cognis-")
    assert server_name in payload["config"]["mcpServers"]
    server = payload["config"]["mcpServers"][server_name]
    assert "COGNIS_DB_PATH" in server["env"]
    assert server["env"]["COGNIS_DB_PATH"].endswith("uckg.db")
    assert str(tmp_path.resolve()) in server["env"]["COGNIS_DB_PATH"]
    assert "COGNIS_REPO_ROOT" not in server["env"]


@pytest.mark.unit
def test_mcp_config_derives_server_name_from_repo_folder(tmp_path: Path) -> None:
    repo = tmp_path / "my-app"
    repo.mkdir()
    runner = CliRunner()
    _invoke(runner, repo, "init")
    result = _invoke(runner, repo, "mcp-config", "--host", "cursor")
    assert result.exit_code == 0  # type: ignore[attr-defined]
    payload = json.loads(result.output)  # type: ignore[attr-defined]
    assert payload["server_name"] == "cognis-my-app"


@pytest.mark.unit
def test_mcp_config_full_env_includes_repo_root_and_audit_log(tmp_path: Path) -> None:
    runner = CliRunner()
    _invoke(runner, tmp_path, "init")
    result = _invoke(runner, tmp_path, "mcp-config", "--host", "cursor", "--full-env")
    assert result.exit_code == 0  # type: ignore[attr-defined]
    payload = json.loads(result.output)  # type: ignore[attr-defined]
    server = payload["config"]["mcpServers"][payload["server_name"]]
    assert server["env"]["COGNIS_REPO_ROOT"] == str(tmp_path.resolve())
    assert server["env"]["COGNIS_AUDIT_LOG"].endswith("audit.log")


@pytest.mark.unit
def test_mcp_config_preserves_explicit_timeout_env(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Explicit MCP timeout env vars should be carried into generated client config."""
    runner = CliRunner()
    _invoke(runner, tmp_path, "init")
    monkeypatch.setenv("COGNIS_MCP_SOFT_TIMEOUT_S", "9")
    monkeypatch.setenv("COGNIS_MCP_HARD_TIMEOUT_S", "18")
    monkeypatch.setenv("COGNIS_MCP_DISCOVER_SEMANTIC_TIMEOUT_S", "11")
    result = _invoke(runner, tmp_path, "mcp-config", "--host", "cursor")
    assert result.exit_code == 0  # type: ignore[attr-defined]
    payload = json.loads(result.output)  # type: ignore[attr-defined]
    server = payload["config"]["mcpServers"][payload["server_name"]]
    assert server["env"]["COGNIS_MCP_SOFT_TIMEOUT_S"] == "9"
    assert server["env"]["COGNIS_MCP_HARD_TIMEOUT_S"] == "18"
    assert server["env"]["COGNIS_MCP_DISCOVER_SEMANTIC_TIMEOUT_S"] == "11"


@pytest.mark.unit
def test_mcp_config_applies_windows_timeout_defaults(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Generated MCP config should include safer semantic timeouts on Windows."""
    runner = CliRunner()
    _invoke(runner, tmp_path, "init")
    # Simulate Windows for MCP-config generation only — patch the platform
    # indirection rather than the global sys.platform, which would otherwise
    # break subprocess/shutil on the (Linux/macOS) host running this test.
    monkeypatch.setattr("cognis.cli.main._current_platform", lambda: "win32")
    for key in (
        "COGNIS_MCP_SOFT_TIMEOUT_S",
        "COGNIS_MCP_HARD_TIMEOUT_S",
        "COGNIS_MCP_DISCOVER_SEMANTIC_TIMEOUT_S",
        "COGNIS_MCP_WARM_SEMANTIC_ON_STARTUP",
    ):
        monkeypatch.delenv(key, raising=False)

    result = _invoke(runner, tmp_path, "mcp-config", "--host", "cursor")
    assert result.exit_code == 0  # type: ignore[attr-defined]

    payload = json.loads(result.output)  # type: ignore[attr-defined]
    server = payload["config"]["mcpServers"][payload["server_name"]]
    assert server["env"]["COGNIS_MCP_SOFT_TIMEOUT_S"] == "30"
    assert server["env"]["COGNIS_MCP_HARD_TIMEOUT_S"] == "60"
    assert server["env"]["COGNIS_MCP_DISCOVER_SEMANTIC_TIMEOUT_S"] == "30"
    assert server["env"]["COGNIS_MCP_WARM_SEMANTIC_ON_STARTUP"] == "1"
    assert payload["config_paths"]["cursor_workspace"].endswith(".cursor\\mcp.json") or payload[
        "config_paths"
    ]["cursor_workspace"].endswith(".cursor/mcp.json")


@pytest.mark.unit
def test_bootstrap_json_reports_phases(tmp_path: Path) -> None:
    """``cognis-cli bootstrap --json`` returns structured phase output."""
    (tmp_path / "main.py").write_text("def hello() -> None:\n    pass\n", encoding="utf-8")
    runner = CliRunner()
    result = _invoke(runner, tmp_path, "bootstrap", "--skip-embeddings", "--json", str(tmp_path))
    assert result.exit_code == 0  # type: ignore[attr-defined]
    payload = json.loads(result.output)  # type: ignore[attr-defined]
    assert payload["command"] == "bootstrap"
    phase_names = [p["name"] for p in payload["phases"]]
    assert phase_names == ["init", "index", "health"]
    assert payload["overall"] in ("ok", "warn", "fail")


@pytest.mark.unit
def test_bootstrap_skip_embeddings_on_empty_repo(tmp_path: Path) -> None:
    """``cognis-cli bootstrap --skip-embeddings`` chains init, index, and health."""
    (tmp_path / "main.py").write_text("def hello() -> None:\n    pass\n", encoding="utf-8")
    runner = CliRunner()
    result = _invoke(runner, tmp_path, "bootstrap", "--skip-embeddings", str(tmp_path))
    assert result.exit_code == 0, result.output  # type: ignore[attr-defined]
    assert (tmp_path / CONFIG_DIR_NAME / CONFIG_FILE_NAME).exists()
    assert "repo root :" in result.output  # type: ignore[attr-defined]
    assert "overall: ok" in result.output or '"overall": "ok"' in result.output  # type: ignore[attr-defined]


@pytest.mark.unit
def test_index_skip_embeddings_runs_on_empty_repo(tmp_path: Path) -> None:
    """``cognis-cli index --skip-embeddings`` succeeds on a repo with no source files."""
    runner = CliRunner()
    result = _invoke(runner, tmp_path, "index", "--skip-embeddings", str(tmp_path))
    assert result.exit_code == 0, result.output  # type: ignore[attr-defined]
    assert "files processed" in result.output  # type: ignore[attr-defined]


@pytest.mark.unit
def test_index_rejects_missing_path(tmp_path: Path) -> None:
    """A non-existent path is rejected with a UsageError-style message."""
    runner = CliRunner()
    bogus = tmp_path / "does-not-exist"
    result = _invoke(runner, tmp_path, "index", "--skip-embeddings", str(bogus))
    assert result.exit_code != 0  # type: ignore[attr-defined]


@pytest.mark.unit
def test_diagnose_empty_index_blames_unfinished_run_when_source_exists(
    tmp_path: Path,
) -> None:
    """When indexable source exists, the empty-index diagnosis must NOT blame ignore rules.

    Regression: a populated repo whose index DB is empty (interrupted run or a
    stalled embedder) previously got a misleading "all excluded by .gitignore"
    message even though plenty of source is indexable.
    """
    from cognis.cli.main import _diagnose_empty_index

    (tmp_path / "pkg").mkdir()
    (tmp_path / "pkg" / "mod.py").write_text("def alpha():\n    return 1\n", encoding="utf-8")

    lines = _diagnose_empty_index(tmp_path)
    text = "\n".join(lines)

    # Must acknowledge indexable files were found and point at re-running index.
    assert "indexable file" in text
    assert "did not finish" in text or "index --full" in text
    # Must NOT wrongly accuse ignore rules when source is clearly indexable.
    assert "excluded by ignore rules" not in text


@pytest.mark.unit
def test_health_empty_db_points_to_index_not_gitignore(tmp_path: Path) -> None:
    """Health on a present-but-empty DB guides to running index, not exclusion.

    The repo has real source, so when the UCKG exists but holds no file rows
    (an interrupted index run), the check must say "build the index" rather than
    asserting the source was excluded by ignore rules.
    """
    from cognis.db import Database

    (tmp_path / "mod.py").write_text("def beta():\n    return 2\n", encoding="utf-8")
    runner = CliRunner()
    _invoke(runner, tmp_path, "init")

    # Create the UCKG with schema but no indexed files (the interrupted-run
    # state). ``Database.connect`` runs migrations, so the file/symbol tables
    # exist but are empty.
    db_path = tmp_path / CONFIG_DIR_NAME / "uckg.db"
    db = Database(str(db_path))
    db.connect()
    db.close_thread_connection()

    result = _invoke(runner, tmp_path, "health", "--json")
    payload = json.loads(result.output)  # type: ignore[attr-defined]
    index_check = payload["checks"]["index"]
    assert index_check["status"] == "fail"
    message = index_check["message"]
    # Accurate guidance: run the index. Must not assert source was excluded.
    assert "index --full" in message or "no indexed files" in message
    assert "no TypeScript/Python/Go source was found" not in message
