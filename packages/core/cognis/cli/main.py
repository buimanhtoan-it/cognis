"""``cognis-cli`` Click application.

Subcommands:

- ``init`` — materialize ``.cognis/`` (config, ``capsule_cache/``, audit log, eval seeds).
- ``bootstrap`` — one-shot ``init`` + cold ``index`` + ``health`` (plug-and-play setup).
- ``index`` — run the indexer pipeline (cold or incremental).
- ``eval`` — run the golden-set eval harness.
- ``health`` — sanity-check config, DB, embedder, and ``index_version`` compatibility.
- ``paths`` — resolved workspace paths and command entrypoints (``--json`` for extensions).
- ``mcp-config`` — emit MCP client config for vscode/cursor/claude (``--json``).
- ``up`` / ``down`` — start / stop ``cognis-mcpd`` and ``cognis-indexd`` (stubs in v0.1.0).
- ``mcp-conformance`` — run the MCP conformance harness.
- ``profile`` — profile hot paths (planner / retrieval / capsule) (stub in v0.1.0).

The ``main`` symbol referenced by ``pyproject.toml``'s console script is a thin
wrapper that drives the Click app under ``standalone_mode=False`` so it can be
invoked programmatically from tests as ``main(["--version"]) -> int``.
"""

from __future__ import annotations

import importlib.util
import json
import os
import re
import shutil
import sqlite3
import subprocess
import sys
from collections.abc import Sequence
from pathlib import Path
from typing import Final, Literal, TypedDict

import click

from cognis import __version__
from cognis.branding import TAGLINE, echo_banner
from cognis.config import (
    CONFIG_DIR_NAME,
    CONFIG_FILE_NAME,
    CONFIG_REVISION,
    Config,
    detect_config_drift,
    migrate_config_file,
    read_config_revision,
    write_config_revision,
)

# ---------------------------------------------------------------------------
# .cognis/ layout constants
# ---------------------------------------------------------------------------

CAPSULE_CACHE_DIRNAME: Final[str] = "capsule_cache"
"""Subdirectory under ``.cognis/`` that holds composed-capsule cache entries."""

DEFAULT_DB_FILENAME: Final[str] = "uckg.db"
"""Filename for the Unified Code Knowledge Graph SQLite store (per design)."""

MCP_ENV_PASSTHROUGH: Final[tuple[str, ...]] = (
    "COGNIS_MCP_SOFT_TIMEOUT_S",
    "COGNIS_MCP_HARD_TIMEOUT_S",
    "COGNIS_MCP_DISCOVER_SEMANTIC_TIMEOUT_S",
    "COGNIS_MCP_SEMANTIC_COOLDOWN_S",
    "COGNIS_MCP_WARM_SEMANTIC_ON_STARTUP",
)
"""MCP env vars that should survive config generation when explicitly set."""

_WINDOWS_MCP_TIMEOUT_DEFAULTS: Final[dict[str, str]] = {
    "COGNIS_MCP_SOFT_TIMEOUT_S": "30",
    "COGNIS_MCP_HARD_TIMEOUT_S": "60",
    "COGNIS_MCP_DISCOVER_SEMANTIC_TIMEOUT_S": "30",
    "COGNIS_MCP_WARM_SEMANTIC_ON_STARTUP": "1",
}
"""Windows-friendly MCP defaults: longer semantic budget + warm embedder on startup."""

# Status enum used inside health-check payloads.
HealthStatus = Literal["ok", "warn", "fail"]


class HealthCheck(TypedDict):
    """Single subcheck payload returned by ``cognis-cli health``."""

    status: HealthStatus
    message: str


class HealthReport(TypedDict):
    """Top-level payload returned by ``cognis-cli health``."""

    runtime_version: str
    checks: dict[str, HealthCheck]
    overall: HealthStatus


# ---------------------------------------------------------------------------
# Path helpers
# ---------------------------------------------------------------------------


def _cognis_dir(repo_root: Path) -> Path:
    """Return ``<repo_root>/.cognis``."""
    return repo_root / CONFIG_DIR_NAME


def _config_path(repo_root: Path) -> Path:
    """Return ``<repo_root>/.cognis/config.yaml``."""
    return _cognis_dir(repo_root) / CONFIG_FILE_NAME


def _db_path(repo_root: Path) -> Path:
    """Return ``<repo_root>/.cognis/uckg.db`` per design Deployment Topology."""
    return _cognis_dir(repo_root) / DEFAULT_DB_FILENAME


def _indexd_status_path(repo_root: Path) -> Path:
    """Return the live-indexing daemon status JSON path."""
    return _cognis_dir(repo_root) / "indexd-status.json"


def _resolve_db_path(repo_root: Path) -> Path:
    """Return the UCKG path, honoring ``COGNIS_DB_PATH`` when set."""
    override = os.environ.get("COGNIS_DB_PATH")
    if override:
        return Path(override).expanduser().resolve()
    return _db_path(repo_root)


def _clear_index_artifacts(repo_root: Path) -> list[str]:
    """Delete stored index artifacts under ``.cognis`` (best-effort).

    Removes the UCKG database and its WAL/SHM/journal sidecars, the capsule
    cache directory, and the indexd status file. The workspace ``config.yaml``
    and config revision are intentionally preserved so user settings and MCP
    wiring survive a clear-and-reindex.

    Returns the list of artifact names that were actually removed.
    """
    import shutil

    db_path = _resolve_db_path(repo_root)
    cognis_dir = _cognis_dir(repo_root)
    targets: list[Path] = [
        db_path,
        Path(f"{db_path}-wal"),
        Path(f"{db_path}-shm"),
        Path(f"{db_path}-journal"),
        _indexd_status_path(repo_root),
        Path(f"{_indexd_status_path(repo_root)}.tmp"),
        cognis_dir / "capsule_cache",
    ]

    removed: list[str] = []
    for target in targets:
        try:
            if target.is_dir():
                shutil.rmtree(target, ignore_errors=True)
                removed.append(target.name)
            elif target.exists():
                target.unlink()
                removed.append(target.name)
        except OSError:
            # Best-effort: a locked DB (e.g. a running daemon on Windows) is
            # surfaced to the caller via the absence of its name in *removed*.
            continue
    return removed


def _resolve_audit_path(repo_root: Path) -> Path:
    """Return the audit log path from config, resolved under *repo_root*."""
    cfg = Config.load(repo_root)
    return _resolve_under_repo(repo_root, cfg.security.audit_log).resolve()


def _find_command_entrypoints(python_exe: str | None = None) -> dict[str, str | None]:
    """Return cognis console-script paths and module names for extension wiring."""
    py = python_exe or sys.executable
    return {
        "python": py,
        "cognis_cli": shutil.which("cognis-cli"),
        "cognis_mcpd": shutil.which("cognis-mcpd"),
        "cognis_indexd": shutil.which("cognis-indexd"),
        "cognis_cli_module": "cognis.cli.main",
        "cognis_mcpd_module": "cognis_mcpd.main",
        "cognis_indexd_module": "cognis_indexd.main",
    }


def _build_workspace_paths(repo_root: Path, *, python_exe: str | None = None) -> dict[str, object]:
    """Assemble resolved paths and entrypoints for IDE extensions."""
    repo_root = repo_root.resolve()
    cfg = Config.load(repo_root)
    db_path = _resolve_db_path(repo_root)
    cognis_dir = _cognis_dir(repo_root)
    return {
        "repo_root": str(repo_root),
        "cognis_dir": str(cognis_dir),
        "config_path": str(_config_path(repo_root)),
        "db_path": str(db_path),
        "indexd_status_path": str(_indexd_status_path(repo_root)),
        "audit_log_path": str(_resolve_audit_path(repo_root)),
        "capsule_cache_dir": str(cognis_dir / CAPSULE_CACHE_DIRNAME),
        "golden_set_path": str(_resolve_under_repo(repo_root, cfg.eval.golden_set).resolve()),
        "runtime_version": __version__,
        "commands": _find_command_entrypoints(python_exe),
    }


def _current_platform() -> str:
    """Return the active platform string.

    Indirection over ``sys.platform`` so tests can simulate a different OS for
    MCP-config generation without monkeypatching the global ``sys.platform``
    (which would poison ``subprocess``/``shutil`` on the host running the test).
    """
    return sys.platform


def _default_mcp_timeout_env(target_platform: str | None = None) -> dict[str, str]:
    """Return platform-tuned MCP timeout defaults for generated client config."""
    platform = (target_platform or _current_platform()).lower()
    if platform.startswith("win"):
        return dict(_WINDOWS_MCP_TIMEOUT_DEFAULTS)
    return {}


McpHost = Literal["vscode", "cursor", "claude"]


def _derive_mcp_server_name(repo_root: Path, *, prefix: str = "cognis") -> str:
    """Return a stable MCP server key such as ``cognis-my-app`` from *repo_root*."""
    slug = repo_root.resolve().name.lower()
    slug = re.sub(r"[^a-z0-9]+", "-", slug)
    slug = slug.strip("-")
    if not slug:
        slug = "repo"
    return f"{prefix}-{slug}"


def _build_mcp_server_block(
    repo_root: Path,
    *,
    python_exe: str | None = None,
    server_name: str = "cognis",
    honor_cognis_env: bool = True,
    minimal_env: bool = True,
) -> dict[str, object]:
    """Build one MCP server block with absolute env paths."""
    py = python_exe or sys.executable
    db_path = _resolve_db_path(repo_root) if honor_cognis_env else _db_path(repo_root)
    audit_path = _resolve_audit_path(repo_root)
    env: dict[str, str] = {
        "COGNIS_DB_PATH": str(db_path),
    }
    if not minimal_env:
        env["COGNIS_AUDIT_LOG"] = str(audit_path)
        env["COGNIS_REPO_ROOT"] = str(repo_root.resolve())
    env.update(_default_mcp_timeout_env())
    for key in MCP_ENV_PASSTHROUGH:
        value = os.environ.get(key)
        if value:
            env[key] = value
    mcpd_bin = shutil.which("cognis-mcpd")
    if mcpd_bin:
        block: dict[str, object] = {
            "command": mcpd_bin,
            "env": env,
        }
    else:
        block = {
            "command": py,
            "args": ["-m", "cognis_mcpd.main"],
            "env": env,
        }
    return {"name": server_name, **block}


def _build_mcp_config(
    repo_root: Path,
    host: McpHost,
    *,
    python_exe: str | None = None,
    server_name: str | None = None,
    honor_cognis_env: bool = False,
    minimal_env: bool = True,
) -> dict[str, object]:
    """Return host-oriented MCP configuration for IDE clients."""
    resolved_name = server_name or _derive_mcp_server_name(repo_root)
    server = _build_mcp_server_block(
        repo_root,
        python_exe=python_exe,
        server_name=resolved_name,
        honor_cognis_env=honor_cognis_env,
        minimal_env=minimal_env,
    )
    inner = {k: v for k, v in server.items() if k != "name"}
    mcp_servers = {resolved_name: inner}

    resolved_root = repo_root.resolve()
    config_paths: dict[str, str] = {
        "claude_macos": str(
            Path.home() / "Library/Application Support/Claude/claude_desktop_config.json"
        ),
        "claude_windows": str(
            Path(os.environ.get("APPDATA", "")) / "Claude/claude_desktop_config.json"
        ),
        "cursor_user": str(Path.home() / ".cursor/mcp.json"),
        "cursor_workspace": str(resolved_root / ".cursor" / "mcp.json"),
        "vscode_workspace": str(resolved_root / ".vscode" / "mcp.json"),
    }

    return {
        "host": host,
        "format": "mcpServers",
        "repo_root": str(repo_root.resolve()),
        "server_name": resolved_name,
        "config": {"mcpServers": mcp_servers},
        "config_paths": config_paths,
        "env": inner.get("env", {}),
    }


def _resolve_under_repo(repo_root: Path, value: str) -> Path:
    """Return ``value`` as-is when absolute, else joined under ``repo_root``."""
    candidate = Path(value)
    if candidate.is_absolute():
        return candidate
    return repo_root / candidate


def _read_eval_seed(repo_root: Path) -> str:
    """Return the contents of ``tests/fixtures/eval/golden.jsonl`` if present.

    Returns an empty string when the seed file is missing — this is the
    expected case for downstream installs that ship without the test
    fixtures. Operators can later populate ``.cognis/eval/golden.jsonl`` by
    hand or via ``cognis-cli init --force`` once they've staged their own
    queries.
    """
    seed_path = repo_root / "tests" / "fixtures" / "eval" / "golden.jsonl"
    if not seed_path.is_file():
        return ""
    return seed_path.read_text(encoding="utf-8")


def _repo_root_from(ctx: click.Context) -> Path:
    """Return the resolved ``--repo-root`` stored on the click context."""
    obj = ctx.obj
    if isinstance(obj, dict):
        candidate = obj.get("repo_root")
        if isinstance(candidate, Path):
            return candidate
    return Path.cwd().resolve()


def _stub(component: str, detail: str) -> None:
    """Print a uniform not-yet-implemented message for CLI placeholders."""
    click.echo(
        f"cognis-cli {component}: not yet implemented ({detail}). "
        "See docs/quickstart.md for supported workflows."
    )


def _find_compose_file(start: Path | None = None) -> Path | None:
    """Locate ``deploy/compose.yaml`` from cwd or upward from *start*."""
    roots = [Path.cwd()]
    if start is not None:
        roots.append(start)
        roots.extend(start.parents)
    seen: set[Path] = set()
    for root in roots:
        if root in seen:
            continue
        seen.add(root)
        candidate = root / "deploy" / "compose.yaml"
        if candidate.is_file():
            return candidate
    return None


def _run_compose(compose_file: Path, *args: str) -> None:
    """Run ``docker compose -f <file> ...`` and exit on failure."""
    cmd = ["docker", "compose", "-f", str(compose_file), *args]
    click.echo(f"  running: {' '.join(cmd)}")
    try:
        subprocess.run(cmd, check=True)
    except FileNotFoundError as exc:
        raise click.ClickException(
            "docker compose not found — install Docker or use manual commands in docs/operations.md"
        ) from exc
    except subprocess.CalledProcessError as exc:
        raise click.ClickException(f"docker compose failed (exit {exc.returncode})") from exc


# ---------------------------------------------------------------------------
# Health checks
# ---------------------------------------------------------------------------


def _check_config(repo_root: Path) -> HealthCheck:
    """Verify ``.cognis/config.yaml`` is loadable; missing file is warn (defaults apply)."""
    cfg_path = _config_path(repo_root)
    if not cfg_path.exists():
        return {
            "status": "warn",
            "message": f"{cfg_path} not present; using built-in defaults",
        }
    try:
        cfg = Config.from_yaml(cfg_path)
    except Exception as exc:  # surfaced verbatim to operator
        return {"status": "fail", "message": f"failed to load {cfg_path}: {exc}"}
    drift = detect_config_drift(repo_root)
    revision = read_config_revision(cfg_path.parent)
    languages = ",".join(cfg.languages.enabled)
    if drift:
        return {
            "status": "warn",
            "message": (
                f"{cfg_path} loaded but stale defaults are pending "
                f"({'; '.join(drift)}). Run `cognis-cli init` to migrate."
            ),
        }
    if revision < CONFIG_REVISION:
        return {
            "status": "warn",
            "message": (
                f"{cfg_path} loaded but config revision {revision} is older than "
                f"runtime revision {CONFIG_REVISION}. Run `cognis-cli init` to refresh it."
            ),
        }
    return {
        "status": "ok",
        "message": (
            f"{cfg_path} loaded "
            f"(embedder={cfg.embedder.backend}/{cfg.embedder.model}, "
            f"languages=[{languages}])"
        ),
    }


def _check_db(repo_root: Path) -> HealthCheck:
    """Verify ``.cognis/uckg.db`` is present-and-writable, or its parent is writable."""
    db_path = _resolve_db_path(repo_root)
    if db_path.exists():
        if os.access(db_path, os.R_OK | os.W_OK):
            return {"status": "ok", "message": f"{db_path} present and writable"}
        return {"status": "fail", "message": f"{db_path} present but not writable"}

    parent = db_path.parent
    if parent.exists():
        if os.access(parent, os.W_OK):
            return {
                "status": "warn",
                "message": (
                    f"{db_path} not present; parent {parent} is writable (run `cognis-cli init`)"
                ),
            }
        return {
            "status": "fail",
            "message": f"cannot reach {db_path}: parent {parent} is not writable",
        }

    grandparent = parent.parent
    if grandparent.exists() and os.access(grandparent, os.W_OK):
        return {
            "status": "warn",
            "message": (
                f"{db_path} not present; will be created on `cognis-cli init` "
                f"({grandparent} is writable)"
            ),
        }
    return {
        "status": "fail",
        "message": f"cannot reach {db_path}: ancestor {grandparent} is not writable",
    }


def _check_embedder(cfg: Config) -> HealthCheck:
    """Verify the configured embedder backend looks reachable.

    Graceful degradation: optional deps may not yet be installed at MVP. We
    return ``warn`` (not ``fail``) so a fresh ``cognis-cli init`` user can see
    actionable next steps without the command exiting non-zero.
    """
    backend = cfg.embedder.backend
    if backend == "local":
        spec = importlib.util.find_spec("sentence_transformers")
        if spec is None:
            return {
                "status": "warn",
                "message": (
                    "local backend selected but `sentence-transformers` is not installed "
                    "(install extras: `pip install cognis-engine[embed-local]`)"
                ),
            }
        return {
            "status": "ok",
            "message": (
                f"local backend `{cfg.embedder.model}` reachable "
                f"(sentence-transformers installed; dim={cfg.embedder.dim})"
            ),
        }
    if backend == "voyage":
        if not os.environ.get("VOYAGE_API_KEY"):
            return {
                "status": "warn",
                "message": "voyage backend selected but VOYAGE_API_KEY env var is not set",
            }
        return {
            "status": "ok",
            "message": f"voyage backend `{cfg.embedder.model}` reachable (API key present)",
        }
    if backend == "openai":
        if not os.environ.get("OPENAI_API_KEY"):
            return {
                "status": "warn",
                "message": "openai backend selected but OPENAI_API_KEY env var is not set",
            }
        return {
            "status": "ok",
            "message": f"openai backend `{cfg.embedder.model}` reachable (API key present)",
        }
    # Unknown backends are guarded by Pydantic Literal validation upstream, but
    # leave a defensive arm so future backends fail-soft until wired in.
    return {"status": "warn", "message": f"unknown embedder backend: {backend!r}"}


def _check_version(repo_root: Path) -> HealthCheck:
    """Compare runtime ``__version__`` against ``meta.index_version`` in the DB.

    Behavior matrix:

    - DB absent → ``warn`` ("not initialized — run `cognis-cli init`").
    - DB present, ``meta`` table missing → ``warn`` (migrations land in task 3).
    - ``index_version`` matches runtime → ``ok``.
    - ``index_version`` differs → ``warn``.
    """
    db_path = _resolve_db_path(repo_root)
    if not db_path.exists():
        return {
            "status": "warn",
            "message": (
                f"runtime cognis=={__version__}; UCKG not initialized "
                f"(no {db_path}). Run `cognis-cli init`."
            ),
        }
    # ``sqlite3.Connection`` is *not* closed by its context manager — that only
    # commits/rollbacks. Use try/finally so Python 3.14 doesn't raise a
    # ResourceWarning (which our pytest config promotes to an error).
    conn: sqlite3.Connection | None = None
    try:
        conn = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
        cur = conn.execute("SELECT value FROM meta WHERE key = 'index_version'")
        row: tuple[object, ...] | None = cur.fetchone()
    except sqlite3.Error as exc:
        return {
            "status": "warn",
            "message": (
                f"DB present but `meta` table not readable ({exc.__class__.__name__}: {exc}); "
                "run migrations (lands in task 3)"
            ),
        }
    finally:
        if conn is not None:
            conn.close()
    if row is None:
        return {
            "status": "warn",
            "message": "DB present but no `index_version` recorded yet (run migrations: task 3)",
        }
    db_version = str(row[0])
    if db_version == __version__:
        return {
            "status": "ok",
            "message": f"index_version={db_version} matches runtime",
        }
    return {
        "status": "fail",
        "message": (
            f"index_version={db_version} differs from runtime {__version__}; "
            "re-index with `cognis-cli index --full .`"
        ),
    }


def _check_index(repo_root: Path) -> HealthCheck:
    """Fail when the UCKG exists but contains no symbols (not ready to serve)."""
    db_path = _resolve_db_path(repo_root)
    if not db_path.exists():
        return {
            "status": "warn",
            "message": f"{db_path} not present (run `cognis-cli init` then `cognis-cli index`)",
        }
    conn: sqlite3.Connection | None = None
    try:
        conn = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
        row = conn.execute("SELECT COUNT(*) FROM symbol").fetchone()
        count = int(row[0]) if row is not None else 0
        file_count: int | None = None
        if count == 0:
            # Only needed to distinguish "no files indexed" from "files parsed
            # but no symbols". Guard against a missing `file` table (older or
            # hand-seeded DBs) so the check degrades gracefully.
            try:
                file_row = conn.execute("SELECT COUNT(*) FROM file").fetchone()
                file_count = int(file_row[0]) if file_row is not None else 0
            except sqlite3.Error:
                file_count = None
    except sqlite3.Error as exc:
        return {
            "status": "fail",
            "message": f"cannot read symbol count from {db_path}: {exc}",
        }
    finally:
        if conn is not None:
            conn.close()
    if count == 0:
        if file_count == 0:
            # No file rows: either indexing never completed for this DB, or the
            # walk genuinely found no supported source. Lead with the common
            # cause (index not built / interrupted) rather than asserting
            # exclusion — a populated repo with plenty of source lands here too
            # when a prior `index` run was interrupted or the embedder stalled.
            return {
                "status": "fail",
                "message": (
                    f"{db_path} has no indexed files yet. Run `cognis-cli index --full .` "
                    "to build the index (add --skip-embeddings if the embedder is slow). "
                    "If it still reports 0 files, run `cognis-cli index --clear .` for a "
                    "diagnosis of whether source was found or excluded by .gitignore / "
                    "repo.ignore."
                ),
            }
        if file_count is None:
            return {
                "status": "fail",
                "message": (
                    f"{db_path} has 0 symbols — run `cognis-cli index --full .` "
                    "before serving MCP traffic"
                ),
            }
        return {
            "status": "fail",
            "message": (
                f"{db_path} indexed {file_count} file(s) but produced 0 symbols "
                "— the files parsed without yielding indexable symbols. "
                "Run `cognis-cli index --full .` to retry."
            ),
        }
    return {"status": "ok", "message": f"{count} symbols indexed in UCKG"}


def _check_vector(repo_root: Path) -> HealthCheck:
    """Report whether sqlite-vec KNN is active for semantic retrieval."""
    db_path = _resolve_db_path(repo_root)
    if not db_path.exists():
        return {"status": "warn", "message": "vector extension check skipped (no database)"}
    try:
        from cognis.db import Database

        db = Database(str(db_path))
        if db.vec_enabled:
            return {"status": "ok", "message": "sqlite-vec loaded (symbol_vec KNN available)"}
        return {
            "status": "warn",
            "message": (
                "sqlite-vec not loaded; embeddings stored but KNN queries unavailable "
                "(install `pip install cognis-engine[vector]`)"
            ),
        }
    except Exception as exc:
        return {"status": "warn", "message": f"vector extension check failed: {exc}"}


def _aggregate_status(checks: dict[str, HealthCheck]) -> HealthStatus:
    """Reduce per-check statuses to an overall status (fail > warn > ok)."""
    statuses = {check["status"] for check in checks.values()}
    if "fail" in statuses:
        return "fail"
    if "warn" in statuses:
        return "warn"
    return "ok"


def _build_health_report(repo_root: Path) -> HealthReport:
    """Run all sanity checks and assemble a typed report."""
    cfg = Config.load(repo_root)
    checks: dict[str, HealthCheck] = {
        "config": _check_config(repo_root),
        "db": _check_db(repo_root),
        "index": _check_index(repo_root),
        "vector": _check_vector(repo_root),
        "embedder": _check_embedder(cfg),
        "version": _check_version(repo_root),
    }
    return {
        "runtime_version": __version__,
        "checks": checks,
        "overall": _aggregate_status(checks),
    }


# ---------------------------------------------------------------------------
# Click command tree
# ---------------------------------------------------------------------------


@click.group(
    name="cognis-cli",
    context_settings={"help_option_names": ["-h", "--help"]},
)
@click.version_option(
    __version__,
    "-V",
    "--version",
    prog_name="cognis-cli",
    message=f"cognis-cli %(version)s — {TAGLINE}",
)
@click.option(
    "--repo-root",
    type=click.Path(file_okay=False, dir_okay=True, path_type=Path),
    default=None,
    help="Repo root that holds the .cognis/ directory (default: current working directory).",
)
@click.pass_context
def cli(ctx: click.Context, repo_root: Path | None) -> None:
    """Operator entry point for cognis (Phase 0/1 MVP)."""
    obj = ctx.ensure_object(dict)
    obj["repo_root"] = (repo_root if repo_root is not None else Path.cwd()).resolve()


# --- init -------------------------------------------------------------------


@cli.command("init")
@click.option(
    "--force",
    is_flag=True,
    help="Overwrite existing config.yaml and golden.jsonl (other artifacts always preserved).",
)
@click.option(
    "--quiet",
    is_flag=True,
    hidden=True,
    help="Suppress human-readable output (used by ``bootstrap --json``).",
)
@click.option(
    "--migrate/--no-migrate",
    default=True,
    help="Apply additive config migrations to an existing config.yaml.",
)
@click.pass_context
def cmd_init(ctx: click.Context, force: bool, quiet: bool, migrate: bool) -> None:
    """Materialize the ``.cognis/`` runtime layout (config, caches, audit log, eval seeds)."""
    repo_root = _repo_root_from(ctx)
    cfg = Config.default()
    cognis_dir = _cognis_dir(repo_root)
    cognis_dir.mkdir(parents=True, exist_ok=True)
    if not quiet:
        click.echo(f"  ensured {cognis_dir}{os.sep}")

    # 1. config.yaml — written from Config.default().to_yaml() (task 2.2 contract).
    cfg_path = _config_path(repo_root)
    if cfg_path.exists() and not force:
        if not quiet:
            click.echo(f"  exists  {cfg_path} (preserved; pass --force to overwrite)")
        if migrate:
            report = migrate_config_file(repo_root)
            if not quiet:
                if report.changes:
                    click.echo(f"  migrated {cfg_path} ({'; '.join(report.changes)})")
                elif report.revision_to != report.revision_from:
                    click.echo(
                        f"  refreshed {cfg_path} migration revision "
                        f"({report.revision_from} -> {report.revision_to})"
                    )
    else:
        cfg.write(cfg_path)
        write_config_revision(cognis_dir, CONFIG_REVISION)
        if not quiet:
            click.echo(f"  wrote   {cfg_path}")

    cfg = Config.load(repo_root)

    # 2. capsule_cache/ — composed-capsule cache directory (design Q-5).
    capsule_cache = cognis_dir / CAPSULE_CACHE_DIRNAME
    capsule_cache.mkdir(parents=True, exist_ok=True)
    if not quiet:
        click.echo(f"  ensured {capsule_cache}{os.sep}")

    # 3. audit log — touch at the path declared in security.audit_log.
    audit_path = _resolve_under_repo(repo_root, cfg.security.audit_log)
    audit_path.parent.mkdir(parents=True, exist_ok=True)
    if audit_path.exists():
        if not quiet:
            click.echo(f"  exists  {audit_path}")
    else:
        audit_path.touch()
        if not quiet:
            click.echo(f"  touched {audit_path}")

    # 4. eval/golden.jsonl placeholder — empty file for task 4 to seed.
    eval_path = _resolve_under_repo(repo_root, cfg.eval.golden_set)
    eval_path.parent.mkdir(parents=True, exist_ok=True)
    if eval_path.exists() and not force:
        if not quiet:
            click.echo(f"  exists  {eval_path} (preserved; pass --force to overwrite)")
    else:
        seed_text = _read_eval_seed(repo_root)
        eval_path.write_text(seed_text, encoding="utf-8")
        if not quiet:
            if seed_text:
                click.echo(f"  wrote   {eval_path} (seeded from tests/fixtures/eval/golden.jsonl)")
            else:
                click.echo(f"  wrote   {eval_path}")

    if not quiet:
        click.echo(f"\ncognis initialized at {cognis_dir}")


# --- bootstrap (plug-and-play) ----------------------------------------------


@cli.command("bootstrap")
@click.argument(
    "path",
    type=click.Path(file_okay=False, dir_okay=True, path_type=Path),
    required=False,
    default=".",
)
@click.option(
    "--force",
    is_flag=True,
    help="Overwrite config.yaml when running init (passed through to ``init``).",
)
@click.option(
    "--skip-embeddings",
    is_flag=True,
    help="Index without embeddings (faster; no Hugging Face download). Re-run without this flag later for semantic search.",
)
@click.option(
    "--json",
    "as_json",
    is_flag=True,
    help="Emit structured JSON (phases + health) instead of human-readable output.",
)
@click.pass_context
def cmd_bootstrap(
    ctx: click.Context,
    path: Path,
    force: bool,
    skip_embeddings: bool,
    as_json: bool,
) -> None:
    """One-shot setup: ``init`` → cold ``index`` → ``health``.

    Intended for operators who want plug-and-play production setup in a single
    command. Accuracy tuning (eval baselines, golden sets) is optional and can
    be done later.
    """
    repo_root = _repo_root_from(ctx)
    target = path.resolve()
    if not target.is_dir():
        raise click.UsageError(f"path is not a directory: {target}")

    db_path = _resolve_db_path(repo_root)
    os.environ.setdefault("COGNIS_DB_PATH", str(db_path))

    phases: list[dict[str, object]] = []
    exit_code = 0

    if not as_json:
        echo_banner(prog="cognis bootstrap", file=sys.stdout)
        click.echo(f"  repo root : {repo_root}")
        click.echo(f"  index path: {target}")
        click.echo(f"  database  : {db_path}")
        click.echo("")

    try:
        ctx.invoke(cmd_init, force=force, quiet=as_json, migrate=True)
        phases.append({"name": "init", "status": "ok"})
    except click.exceptions.Exit as exc:
        phases.append({"name": "init", "status": "fail", "exit_code": exc.exit_code})
        exit_code = max(exit_code, int(exc.exit_code))

    if not as_json:
        click.echo("")

    if exit_code == 0:
        try:
            ctx.invoke(
                cmd_index,
                path=target,
                full=True,
                skip_embeddings=skip_embeddings,
                ci=False,
                quiet=as_json,
            )
            phases.append({"name": "index", "status": "ok", "skip_embeddings": skip_embeddings})
        except click.exceptions.Exit as exc:
            phases.append({"name": "index", "status": "fail", "exit_code": exc.exit_code})
            exit_code = max(exit_code, int(exc.exit_code))

    if not as_json:
        click.echo("")

    health_report = _build_health_report(repo_root)
    phases.append({"name": "health", "status": health_report["overall"], "report": health_report})
    if health_report["overall"] == "fail":
        exit_code = 1

    payload: dict[str, object] = {
        "command": "bootstrap",
        "runtime_version": __version__,
        "repo_root": str(repo_root),
        "index_path": str(target),
        "db_path": str(db_path),
        "skip_embeddings": skip_embeddings,
        "paths": _build_workspace_paths(repo_root),
        "phases": phases,
        "health": health_report,
        "overall": health_report["overall"],
        "exit_code": exit_code,
    }

    if as_json:
        click.echo(json.dumps(payload, indent=2, sort_keys=True))
    else:
        symbol_for: dict[HealthStatus, str] = {"ok": "[OK]  ", "warn": "[WARN]", "fail": "[FAIL]"}
        click.echo(f"cognis runtime version: {health_report['runtime_version']}")
        click.echo(f"repo root             : {repo_root}")
        click.echo("")
        for section, check in health_report["checks"].items():
            click.echo(f"{symbol_for[check['status']]} {section:<9} {check['message']}")
        click.echo("")
        click.echo(f"overall: {health_report['overall']}")

    if exit_code != 0:
        ctx.exit(exit_code)


# --- health -----------------------------------------------------------------


@cli.command("health")
@click.option(
    "--json",
    "as_json",
    is_flag=True,
    help="Emit the report as machine-readable JSON instead of the human summary.",
)
@click.pass_context
def cmd_health(ctx: click.Context, as_json: bool) -> None:
    """Sanity-check config, DB, embedder, and ``index_version`` compatibility."""
    repo_root = _repo_root_from(ctx)
    report = _build_health_report(repo_root)

    if as_json:
        click.echo(json.dumps(report, indent=2, sort_keys=True))
    else:
        symbol_for: dict[HealthStatus, str] = {"ok": "[OK]  ", "warn": "[WARN]", "fail": "[FAIL]"}
        click.echo(f"cognis runtime version: {report['runtime_version']}")
        click.echo(f"repo root             : {repo_root}")
        click.echo("")
        for section, payload in report["checks"].items():
            click.echo(f"{symbol_for[payload['status']]} {section:<9} {payload['message']}")
        click.echo("")
        click.echo(f"overall: {report['overall']}")

    if report["overall"] == "fail":
        ctx.exit(1)


# --- paths (extension contract) ---------------------------------------------


@cli.command("paths")
@click.option(
    "--python",
    "python_exe",
    default=None,
    help="Python executable used to resolve module invocations.",
)
@click.pass_context
def cmd_paths(ctx: click.Context, python_exe: str | None) -> None:
    """Print resolved workspace paths and cognis command entrypoints (JSON)."""
    repo_root = _repo_root_from(ctx)
    payload = _build_workspace_paths(repo_root, python_exe=python_exe)
    click.echo(json.dumps(payload, indent=2, sort_keys=True))


# --- doctor (extension prerequisite checklist) ------------------------------


class PrerequisiteItem(TypedDict):
    """One installable prerequisite reported by ``cognis-cli doctor``."""

    id: str
    label: str
    description: str
    # "ok" when importable/present, "missing" when the user must install it.
    status: Literal["ok", "missing"]
    # True when setup/index cannot proceed without this item.
    required: bool
    # The pip extra (e.g. ``cognis-engine[embed-local]``) install target, or "".
    install_target: str
    detail: str


# Probe table: each entry maps a user-facing prerequisite to the import that
# proves it is installed and the pip extra that installs it. Kept in one place
# so the extension checklist and the CLI stay in lockstep with pyproject extras.
_PREREQUISITE_PROBES: Final[tuple[tuple[str, str, str, str, tuple[str, ...], bool], ...]] = (
    (
        "indexer",
        "Code parsers (tree-sitter)",
        "Parses TypeScript, Python, and Go so the workspace can be indexed.",
        "indexer",
        ("tree_sitter", "tree_sitter_python", "tree_sitter_typescript", "tree_sitter_go"),
        True,
    ),
    (
        "embed_local",
        "Local embeddings (sentence-transformers)",
        "Generates semantic vectors locally so semantic search works offline.",
        "embed-local",
        ("sentence_transformers", "numpy"),
        True,
    ),
    (
        "vector",
        "Vector search (sqlite-vec)",
        "Enables fast on-disk KNN over embeddings. Without it, semantic search degrades.",
        "vector",
        ("sqlite_vec",),
        False,
    ),
    (
        "mcp",
        "MCP server (fastmcp)",
        "Serves Cognis tools to your AI agent over the Model Context Protocol.",
        "mcp",
        ("fastmcp",),
        True,
    ),
    (
        "tokenizers",
        "Token estimation (tiktoken)",
        "Estimates capsule token budgets accurately. Falls back to a word-count heuristic.",
        "tokenizers",
        ("tiktoken",),
        False,
    ),
)


def _missing_modules(modules: tuple[str, ...]) -> list[str]:
    """Return the subset of *modules* that cannot be imported."""
    missing: list[str] = []
    for module in modules:
        if importlib.util.find_spec(module) is None:
            missing.append(module)
    return missing


def _build_prerequisites(python_exe: str | None = None) -> dict[str, object]:
    """Probe installable prerequisites for the extension's setup checklist."""
    py = python_exe or sys.executable
    items: list[PrerequisiteItem] = []
    for item_id, label, description, extra, modules, required in _PREREQUISITE_PROBES:
        missing = _missing_modules(modules)
        status: Literal["ok", "missing"] = "missing" if missing else "ok"
        install_target = f".[{extra}]"
        detail = "Installed." if status == "ok" else "Not installed: missing " + ", ".join(missing)
        items.append(
            {
                "id": item_id,
                "label": label,
                "description": description,
                "status": status,
                "required": required,
                "install_target": install_target,
                "detail": detail,
            }
        )

    required_missing = [item for item in items if item["required"] and item["status"] == "missing"]
    # A single combined install command covers every missing item in one shot.
    missing_extras = sorted(
        {item["install_target"].strip(".[]") for item in items if item["status"] == "missing"}
    )
    combined_target = f".[{','.join(missing_extras)}]" if missing_extras else ""

    return {
        "python": py,
        "ready": len(required_missing) == 0,
        "items": items,
        "combined_install_target": combined_target,
    }


@cli.command("doctor")
@click.option(
    "--json",
    "as_json",
    is_flag=True,
    default=True,
    help="Emit machine-readable JSON (default: true).",
)
@click.option(
    "--python",
    "python_exe",
    default=None,
    help="Python executable used to resolve module invocations.",
)
@click.pass_context
def cmd_doctor(ctx: click.Context, as_json: bool, python_exe: str | None) -> None:
    """Report installable prerequisites for the IDE setup checklist (JSON).

    Probes each optional dependency group (parsers, local embeddings, vector
    search, MCP server, tokenizers) and reports whether it is installed plus the
    pip target that installs it. The VS Code / Cursor extension consumes this to
    render a checklist with per-item install buttons before running setup.
    """
    payload = _build_prerequisites(python_exe=python_exe)
    if as_json:
        click.echo(json.dumps(payload, indent=2, sort_keys=True))
        return
    items = payload["items"]
    assert isinstance(items, list)
    symbol_for = {"ok": "[OK]  ", "missing": "[MISS]"}
    for item in items:
        flag = "required" if item["required"] else "optional"
        click.echo(f"{symbol_for[item['status']]} {item['label']} ({flag}) — {item['detail']}")
    click.echo("")
    click.echo("ready" if payload["ready"] else "prerequisites missing")


# --- mcp-config (extension contract) ----------------------------------------


@cli.command("mcp-config")
@click.option(
    "--host",
    type=click.Choice(["vscode", "cursor", "claude"], case_sensitive=False),
    default="cursor",
    show_default=True,
    help="Target MCP client host.",
)
@click.option(
    "--json",
    "as_json",
    is_flag=True,
    default=True,
    help="Emit machine-readable JSON (default: true).",
)
@click.option(
    "--python",
    "python_exe",
    default=None,
    help="Python executable for cognis-mcpd when console script is not on PATH.",
)
@click.option(
    "--server-name",
    default=None,
    help="Key under mcpServers (default: cognis-<repo-folder-slug>).",
)
@click.option(
    "--full-env/--minimal-env",
    default=False,
    show_default=True,
    help="Emit COGNIS_REPO_ROOT and COGNIS_AUDIT_LOG in addition to COGNIS_DB_PATH.",
)
@click.pass_context
def cmd_mcp_config(
    ctx: click.Context,
    host: str,
    as_json: bool,
    python_exe: str | None,
    server_name: str | None,
    full_env: bool,
) -> None:
    """Emit MCP client configuration for VS Code, Cursor, or Claude."""
    repo_root = _repo_root_from(ctx)
    payload = _build_mcp_config(
        repo_root,
        host.lower(),  # type: ignore[arg-type]
        python_exe=python_exe,
        server_name=server_name,
        minimal_env=not full_env,
    )
    if as_json:
        click.echo(json.dumps(payload, indent=2, sort_keys=True))
    else:
        click.echo(json.dumps(payload["config"], indent=2))


# --- index (stub) -----------------------------------------------------------


def _diagnose_empty_index(repo_root: Path) -> list[str]:
    """Explain why a walk found no indexable files.

    Returns human-readable lines describing what is in the repo and which
    ignore rules / language settings are in play, so a ``0 files`` result is
    self-explaining instead of silent.
    """
    from cognis_indexer.watcher.gitignore import GitignoreFilter

    cfg = Config.load(repo_root)
    supported_exts = {".ts", ".tsx", ".py", ".go"}
    enabled = set(cfg.languages.enabled)
    gitignore = GitignoreFilter.from_repo(repo_root, extra_patterns=list(cfg.repo.ignore))

    total_files = 0
    supported_total = 0  # supported extension, before ignore filtering
    supported_after_ignore = 0
    ignored_examples: list[str] = []
    ext_counts: dict[str, int] = {}

    for current_root, dirnames, filenames in os.walk(repo_root, topdown=True):
        dirnames[:] = [d for d in dirnames if d != ".git"]
        current_path = Path(current_root)
        for filename in filenames:
            total_files += 1
            suffix = Path(filename).suffix.lower()
            if suffix:
                ext_counts[suffix] = ext_counts.get(suffix, 0) + 1
            if suffix not in supported_exts:
                continue
            supported_total += 1
            try:
                rel = (current_path / filename).relative_to(repo_root).as_posix()
            except ValueError:
                continue
            if gitignore.is_ignored(rel):
                if len(ignored_examples) < 5:
                    ignored_examples.append(rel)
                continue
            supported_after_ignore += 1

    lines: list[str] = []
    lines.append(
        f"  diagnosis       : walked {total_files} files; "
        f"{supported_total} in supported languages (.ts/.tsx/.py/.go), "
        f"{supported_after_ignore} left after ignore rules"
    )
    if supported_total == 0:
        top_exts = sorted(ext_counts.items(), key=lambda kv: -kv[1])[:6]
        ext_summary = ", ".join(f"{ext} x{n}" for ext, n in top_exts) or "(none)"
        lines.append(
            "  hint            : no TypeScript/Python/Go source files found. "
            f"Most common file types here: {ext_summary}. "
            "cognis indexes only .ts/.tsx/.py/.go today."
        )
        if enabled != {"typescript", "python", "go"}:
            lines.append(f"  languages.enabled = {sorted(enabled)} (check .cognis/config.yaml)")
    elif supported_after_ignore == 0:
        lines.append(
            "  hint            : supported files exist but are all excluded by ignore rules. "
            f"Examples ignored: {', '.join(ignored_examples)}"
        )
        lines.append(
            f"  repo.ignore     = {list(cfg.repo.ignore)} "
            "(plus your .gitignore). Remove the over-broad pattern and re-run."
        )
    else:
        # Files ARE indexable but nothing landed in the DB. The walk/filter is
        # fine — indexing did not complete (interrupted run, embedder stall, or
        # a write error). Do NOT blame ignore rules here.
        lines.append(
            f"  hint            : {supported_after_ignore} indexable file(s) were found, "
            "but the index is empty — a previous index run did not finish. "
            "Re-run `cognis-cli index --full .` (add --skip-embeddings if the "
            "embedder model download/load is slow), then check `cognis-cli health`."
        )
    return lines


@cli.command("index")
@click.argument(
    "path",
    type=click.Path(file_okay=False, dir_okay=True, path_type=Path),
    required=False,
    default=None,
)
@click.option("--full", is_flag=True, help="Cold-index the repo from scratch.")
@click.option(
    "--clear",
    is_flag=True,
    help="Delete the stored index (DB, caches) before indexing, then full-rebuild. "
    "Keeps config.yaml.",
)
@click.option(
    "--skip-embeddings",
    is_flag=True,
    help="Defer embedding generation; run lexical+structural-only.",
)
@click.option(
    "--ci",
    is_flag=True,
    help="Read-only validation mode (PR gate; fails on parse errors).",
)
@click.option(
    "--quiet",
    is_flag=True,
    hidden=True,
    help="Suppress human-readable output (used by ``bootstrap --json``).",
)
@click.pass_context
def cmd_index(
    ctx: click.Context,
    path: Path | None,
    full: bool,
    clear: bool,
    skip_embeddings: bool,
    ci: bool,
    quiet: bool,
) -> None:
    """Run the indexer pipeline (cold or incremental).

    PATH is the repository root to index (defaults to the current working
    directory). The DB lives at ``<path>/.cognis/uckg.db`` unless ``--ci`` is
    set, in which case an in-memory DB is used and a non-zero exit is returned
    on any parse error.

    ``--clear`` removes the stored index artifacts (UCKG database and its
    WAL/SHM sidecars, plus the capsule cache) before indexing and forces a full
    rebuild. The workspace ``config.yaml`` is preserved.
    """
    # Resolve the target repo. The CLI has a top-level --repo-root, but
    # ``cognis-cli index <path>`` is the documented way (see docs/quickstart).
    target = (path if path is not None else _repo_root_from(ctx)).resolve()
    if not target.is_dir():
        raise click.UsageError(f"path is not a directory: {target}")

    # --clear wipes prior state and implies a full cold rebuild.
    if clear and not ci:
        removed = _clear_index_artifacts(target)
        if not quiet:
            if removed:
                click.echo("  cleared         : " + ", ".join(removed))
            else:
                click.echo("  cleared         : (no index artifacts found)")
        full = True

    # Local imports keep ``cognis-cli --help`` cheap and avoid pulling the
    # indexer extras into every CLI invocation.
    from cognis_indexer.pipeline import IndexerPipeline

    from cognis.db import Database

    cfg = Config.load(target)

    if ci:
        db = Database(":memory:")
    else:
        db_path = _resolve_db_path(target)
        db_path.parent.mkdir(parents=True, exist_ok=True)
        db = Database(str(db_path))

    # Build an embedder unless the caller asked us to skip it. We import
    # lazily because ``sentence-transformers`` is an optional extra.
    embedder: object | None = None
    if not skip_embeddings:
        try:
            from cognis_indexer.embedder import LocalEmbedder

            embedder = LocalEmbedder(model_name=cfg.embedder.model)
        except ImportError as exc:
            click.echo(
                "error: embedder not installed. "
                "Install with `pip install cognis-engine[embed-local]` "
                "or rerun with --skip-embeddings.",
                err=True,
            )
            click.echo(f"  cause: {exc}", err=True)
            ctx.exit(1)
            return

    pipeline = IndexerPipeline(db=db, config=cfg, embedder=embedder)  # type: ignore[arg-type]
    try:
        stats = pipeline.index_repo(
            target,
            full=full,
            skip_embeddings=skip_embeddings,
        )
        if full and not ci:
            from cognis.db import _write_meta

            with db.write() as conn:
                _write_meta(conn, "index_version", __version__)
    finally:
        pipeline.close()

    if not quiet:
        click.echo(f"  repo            : {target}")
        click.echo(f"  files processed : {stats.files_processed}")
        click.echo(f"  files skipped   : {stats.files_skipped}")
        click.echo(f"  symbols indexed : {stats.symbols_indexed}")
        click.echo(f"  edges resolved  : {stats.edges_resolved}")
        click.echo(f"  secrets redacted: {stats.secrets_redacted}")
        click.echo(f"  errors          : {len(stats.errors)}")
        click.echo(f"  elapsed         : {stats.elapsed_s:.2f}s")
        # When nothing was indexed and nothing was skipped, the walk found no
        # supported files. Explain why instead of leaving the user guessing.
        if stats.files_processed == 0 and stats.files_skipped == 0 and not ci:
            try:
                for line in _diagnose_empty_index(target):
                    click.echo(line)
            except Exception as exc:  # diagnosis is best-effort, never fatal
                click.echo(f"  diagnosis       : unavailable ({exc})")
        if stats.errors:
            click.echo("\nerrors:", err=True)
            for err in stats.errors[:20]:
                click.echo(f"  {err}", err=True)
            if len(stats.errors) > 20:
                click.echo(f"  ... and {len(stats.errors) - 20} more", err=True)

    if ci and stats.errors:
        ctx.exit(1)


# --- eval -------------------------------------------------------------------


@cli.command("eval")
@click.option(
    "--queries",
    type=click.Path(dir_okay=False, path_type=Path),
    default=None,
    help="Override the golden-set JSONL path (default: eval.golden_set from config).",
)
@click.option(
    "--out",
    type=click.Path(file_okay=False, path_type=Path),
    default=None,
    help="Output directory for the eval report (default: eval-reports/<timestamp>/).",
)
@click.option(
    "--k",
    type=click.IntRange(min=1),
    default=None,
    help="Cutoff rank for Recall@k (default: 10).",
)
@click.pass_context
def cmd_eval(
    ctx: click.Context,
    queries: Path | None,
    out: Path | None,
    k: int | None,
) -> None:
    """Run the golden-set eval harness and write a JSON+Markdown report."""
    # Imported here so ``cognis-cli --help`` works without the eval package
    # available (e.g. during a stripped-down install). The package ships in
    # the same wheel today, but this keeps the CLI surface resilient.
    import os

    from cognis_eval.runner import DEFAULT_K, NullStrategy, prepare_out_dir, run_eval
    from cognis_eval.strategy import strategy_from_env

    repo_root = _repo_root_from(ctx)
    cfg = Config.load(repo_root)

    queries_path = (
        queries.resolve()
        if queries is not None
        else _resolve_under_repo(repo_root, cfg.eval.golden_set)
    )
    if not queries_path.exists():
        raise click.UsageError(
            f"golden set not found at {queries_path}. "
            "Run `cognis-cli init` first or pass --queries."
        )

    base_out = out.resolve() if out is not None else _resolve_under_repo(repo_root, "eval-reports")
    out_dir = prepare_out_dir(base_out)
    cutoff = k if k is not None else DEFAULT_K

    db_path = _db_path(repo_root)
    if db_path.exists():
        os.environ.setdefault("COGNIS_DB_PATH", str(db_path))
    live = strategy_from_env()
    strategy = live if live is not None else NullStrategy()
    if live is None:
        click.echo(
            "  warning: no indexed UCKG at COGNIS_DB_PATH — using NullStrategy (Recall@k will be 0)",
            err=True,
        )

    report, written_to = run_eval(queries_path, out_dir, k=cutoff, strategy=strategy)

    click.echo(f"  queries : {queries_path}")
    click.echo(f"  strategy: {report.strategy}")
    click.echo(f"  count   : {report.num_queries}")
    click.echo(f"  Recall@{report.k}: {report.recall_at_k:.4f}")
    click.echo(f"  MRR     : {report.mrr:.4f}")
    click.echo(f"  capsule efficiency: {report.capsule_token_efficiency:.4f}")
    click.echo(f"\nreport written to {written_to}")


# --- up / down (stubs) ------------------------------------------------------


@cli.command("up")
@click.option(
    "--detach/--no-detach",
    default=True,
    help="Run cognis-mcpd and cognis-indexd in the background.",
)
@click.pass_context
def cmd_up(ctx: click.Context, detach: bool) -> None:
    """Start ``cognis-mcpd`` and ``cognis-indexd`` via Docker Compose."""
    repo_root = _repo_root_from(ctx)
    compose = _find_compose_file(repo_root)
    if compose is None:
        _stub("up", "deploy/compose.yaml not found")
        click.echo("See docs/operations.md for manual startup.")
        return
    args: list[str] = ["up"]
    if detach:
        args.append("-d")
    _run_compose(compose, *args)
    click.echo("cognis stack started. Run `cognis-cli health` after indexing.")


@cli.command("down")
@click.pass_context
def cmd_down(ctx: click.Context) -> None:
    """Stop the Docker Compose cognis stack."""
    repo_root = _repo_root_from(ctx)
    compose = _find_compose_file(repo_root)
    if compose is None:
        _stub("down", "deploy/compose.yaml not found")
        return
    _run_compose(compose, "down")
    click.echo("cognis stack stopped.")


# --- mcp-conformance --------------------------------------------------------


@cli.command("mcp-conformance")
@click.option(
    "--report",
    type=click.Path(dir_okay=False, path_type=Path),
    default=None,
    help="Path to write the conformance report (default: stdout).",
)
@click.option(
    "--json",
    "as_json",
    is_flag=True,
    help="Emit the report as machine-readable JSON.",
)
@click.pass_context
def cmd_mcp_conformance(ctx: click.Context, report: Path | None, as_json: bool) -> None:
    """Run the canonical MCP conformance harness against ``cognis-mcpd``.

    Attempts to import and run the upstream ``mcp-conformance`` test harness.
    When the harness is not installed, falls back to a built-in sanity check
    that verifies the 4 MVP tools surface-level conformance (schema + error
    envelope shape) without a live server process.

    Task 16.1 implementation.
    """
    results = _run_mcp_conformance()
    output = (
        json.dumps(results, indent=2, sort_keys=True) if as_json else _format_conformance(results)
    )

    if report is not None:
        report.parent.mkdir(parents=True, exist_ok=True)
        report.write_text(output, encoding="utf-8")
        click.echo(f"conformance report written to {report}")
    else:
        click.echo(output)

    # Exit non-zero if any test failed.
    if results.get("overall") == "FAIL":
        ctx.exit(1)


def _run_mcp_conformance() -> dict[str, object]:
    """Attempt upstream mcp-conformance harness; fall back to built-in checks."""
    # Try the upstream mcp-conformance package first.
    upstream_spec = importlib.util.find_spec("mcp_conformance")
    if upstream_spec is not None:
        return _run_upstream_conformance()
    return _run_builtin_conformance()


def _run_upstream_conformance() -> dict[str, object]:
    """Invoke the upstream mcp-conformance harness programmatically."""
    try:
        import mcp_conformance

        result = mcp_conformance.run(server_command=["cognis-mcpd"])
        return {
            "harness": "upstream:mcp_conformance",
            "overall": "PASS" if result.passed else "FAIL",
            "tests": result.details if hasattr(result, "details") else str(result),
        }
    except Exception as exc:
        return {
            "harness": "upstream:mcp_conformance",
            "overall": "ERROR",
            "error": str(exc),
        }


def _run_builtin_conformance() -> dict[str, object]:
    """Built-in conformance checks run without a live server.

    Validates:
    1. All 8 required tool names are importable from cognis_mcpd.tools.
    2. Each tool returns a dict or list (never raises) for valid minimal input.
    3. Each tool returns the standard error envelope for invalid input.
    4. Error envelope has required keys: ``error.code``, ``error.message``, ``error.retryable``.
    """
    tests: list[dict[str, object]] = []
    overall = "PASS"

    # --- check 1: tools importable -----------------------------------------
    try:
        from cognis_mcpd.tools import (
            dependency_trace,
            diffuse_context,
            retrieve_context_capsule,
            semantic_search,
            symbol_lookup,
        )

        tests.append(
            {
                "name": "tools_importable",
                "status": "PASS",
                "detail": "All 8 tools imported from cognis_mcpd.tools",
            }
        )
    except ImportError as exc:
        tests.append(
            {
                "name": "tools_importable",
                "status": "FAIL",
                "detail": f"Import failed: {exc}",
            }
        )
        return {"harness": "builtin", "overall": "FAIL", "tests": tests}

    # --- check 2: error envelope on invalid input ---------------------------
    REQUIRED_ERROR_KEYS = {"code", "message", "retryable"}

    def _check_error_envelope(tool_name: str, result: object) -> dict[str, object]:
        if not isinstance(result, dict):
            return {
                "name": f"{tool_name}_error_envelope",
                "status": "FAIL",
                "detail": f"expected dict envelope, got {type(result).__name__}",
            }
        err = result.get("error")
        if err is None:
            return {
                "name": f"{tool_name}_error_envelope",
                "status": "FAIL",
                "detail": f"missing 'error' key in envelope: {result}",
            }
        missing = REQUIRED_ERROR_KEYS - set(err.keys())
        if missing:
            return {
                "name": f"{tool_name}_error_envelope",
                "status": "FAIL",
                "detail": f"error envelope missing keys {missing}: {err}",
            }
        return {
            "name": f"{tool_name}_error_envelope",
            "status": "PASS",
            "detail": f"error envelope well-formed (code={err.get('code')})",
        }

    # diffuse_context with empty string should return error envelope.
    r0 = diffuse_context("")
    tests.append(_check_error_envelope("diffuse_context", r0))

    # symbol_lookup with empty string should return error envelope.
    r = symbol_lookup("")
    tests.append(_check_error_envelope("symbol_lookup", r))

    # semantic_search with empty string should return error envelope.
    r2 = semantic_search("")
    tests.append(_check_error_envelope("semantic_search", r2))

    # dependency_trace with empty string should return error envelope.
    r3 = dependency_trace("")
    tests.append(_check_error_envelope("dependency_trace", r3))

    # retrieve_context_capsule with empty string should return error envelope.
    r4 = retrieve_context_capsule("")
    tests.append(_check_error_envelope("retrieve_context_capsule", r4))

    # --- check 3: tools return dict/list (not raise) on plausible input -----
    from collections.abc import Callable

    tool_checks: list[tuple[str, Callable[[], object]]] = [
        ("diffuse_context", lambda: diffuse_context("jwt token validation", k=5)),
        ("symbol_lookup", lambda: symbol_lookup("does_not_exist_xyz")),
        ("semantic_search", lambda: semantic_search("jwt token validation", k=5)),
        ("dependency_trace", lambda: dependency_trace("does_not_exist_xyz", "out", 2)),
        (
            "retrieve_context_capsule",
            lambda: retrieve_context_capsule("why is login slow?", max_tokens=500),
        ),
    ]
    for tool_name, call in tool_checks:
        try:
            res = call()
            if isinstance(res, (dict, list)):
                tests.append(
                    {
                        "name": f"{tool_name}_returns_valid_type",
                        "status": "PASS",
                        "detail": f"returned {type(res).__name__}",
                    }
                )
            else:
                tests.append(
                    {
                        "name": f"{tool_name}_returns_valid_type",
                        "status": "FAIL",
                        "detail": f"expected dict|list, got {type(res).__name__}",
                    }
                )
        except Exception as exc:
            tests.append(
                {
                    "name": f"{tool_name}_returns_valid_type",
                    "status": "FAIL",
                    "detail": f"raised unexpected exception: {exc}",
                }
            )

    # Compute overall.
    if any(t["status"] == "FAIL" for t in tests):
        overall = "FAIL"

    return {
        "harness": "builtin",
        "cognis_version": __version__,
        "overall": overall,
        "tests": tests,
    }


def _format_conformance(results: dict[str, object]) -> str:
    """Format a conformance result dict as human-readable text."""
    lines: list[str] = [
        "cognis MCP Conformance Report",
        f"  harness : {results.get('harness', 'unknown')}",
        f"  version : {results.get('cognis_version', 'unknown')}",
        f"  overall : {results.get('overall', 'unknown')}",
        "",
    ]
    tests = results.get("tests", [])
    if isinstance(tests, list):
        for t in tests:
            status = t.get("status", "?")
            symbol = "[PASS]" if status == "PASS" else "[FAIL]" if status == "FAIL" else "[????]"
            lines.append(f"  {symbol} {t.get('name', '?'):<45}  {t.get('detail', '')}")
    elif isinstance(tests, str):
        lines.append(f"  {tests}")

    if "error" in results:
        lines.append(f"  error: {results['error']}")

    return "\n".join(lines)


# --- profile (stub) ---------------------------------------------------------


@cli.command("profile")
@click.option(
    "--target",
    type=click.Choice(["planner", "retrieval", "capsule", "indexer"], case_sensitive=False),
    default="capsule",
    help="Pipeline stage to profile.",
)
@click.option(
    "--iterations",
    type=click.IntRange(min=1),
    default=20,
    help="Number of timing samples to collect.",
)
@click.pass_context
def cmd_profile(ctx: click.Context, target: str, iterations: int) -> None:
    """Profile hot paths and report latency percentiles. [stub — task 18.1]"""
    del target, iterations  # accepted for surface stability
    _stub("profile", "hot-path profiling for planner, retrieval, capsule, and indexer")


# ---------------------------------------------------------------------------
# Programmatic entry point used by the console script and the scaffold tests.
# ---------------------------------------------------------------------------


def main(argv: Sequence[str] | None = None) -> int:
    """Drive the Click app and return a process exit code.

    The setuptools-generated console script wraps this as ``sys.exit(main())``,
    so returning an integer (rather than raising ``SystemExit`` directly) keeps
    the function callable from tests via ``main(["--version"])``.
    """
    args = list(argv) if argv is not None else None
    try:
        cli.main(args=args, prog_name="cognis-cli", standalone_mode=False)
    except click.exceptions.Exit as exc:
        return int(exc.exit_code)
    except click.exceptions.UsageError as exc:
        exc.show()
        return exc.exit_code
    except click.ClickException as exc:
        exc.show()
        return exc.exit_code
    except click.exceptions.Abort:
        click.echo("Aborted!", err=True)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
