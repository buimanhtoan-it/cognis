"""Cross-app end-to-end harness.

Cognis ships as several cooperating apps that talk over process boundaries:

    cognis-vscode (TypeScript)
        │  spawns `python -m cognis.cli.main ...`  (paths / init / bootstrap /
        │                                            index / health / mcp-config)
        │  spawns `python -m cognis_indexd.main ...` (live indexing daemon)
        ▼
    cognis-cli / cognis-indexd / cognis-mcpd (Python)
        │  share a UCKG SQLite DB + a JSON status file + MCP stdio protocol
        ▼
    AI agent / MCP host

Unit and integration tests mock one side of each boundary, so a drift in the
JSON shape exchanged between apps (e.g. the CLI renames a field the extension
reads, or the daemon changes its status phases) slips through. This harness
runs the *real* entrypoints as subprocesses, exactly the way the extension
invokes them, and returns their actual output so E2E tests can assert the
cross-app contract end to end.

Everything here is dependency-light: it shells out with ``sys.executable -m
<module>`` (the same module names the extension hard-codes in ``cli.ts`` /
``indexd.ts``), so it works whether or not the console scripts are on PATH.
"""

from __future__ import annotations

import contextlib
import json
import os
import subprocess
import sys
import threading
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any

# Module names the VS Code extension hard-codes. Keeping them here in one place
# means an E2E test fails loudly if a module is renamed without updating both
# the extension and these contracts.
CLI_MODULE = "cognis.cli.main"
INDEXD_MODULE = "cognis_indexd.main"
MCPD_MODULE = "cognis_mcpd.main"


@dataclass
class CliResult:
    """Outcome of a one-shot CLI invocation."""

    exit_code: int
    stdout: str
    stderr: str

    def json(self) -> Any:
        """Parse stdout as JSON, tolerating a leading human banner line.

        Mirrors the extension's ``runCliJson`` which slices from the first
        ``{`` — so this stays faithful to how the extension actually parses CLI
        output.
        """
        text = self.stdout.strip()
        brace = text.find("{")
        payload = text[brace:] if brace >= 0 else text
        return json.loads(payload)


def _base_env(extra: dict[str, str] | None = None) -> dict[str, str]:
    """Return a clean child env with cognis-specific overrides applied."""
    env = dict(os.environ)
    # Never let an ambient DB path from the developer's shell leak into the
    # subprocess and point it at the wrong workspace.
    env.pop("COGNIS_DB_PATH", None)
    env.pop("COGNIS_REPO_ROOT", None)
    env.pop("COGNIS_INDEXD_STATUS_PATH", None)
    env.setdefault("PYTHONUNBUFFERED", "1")
    # Keep the MCP server from loading a heavy local embedder during E2E.
    env.setdefault("COGNIS_MCP_WARM_SEMANTIC_ON_STARTUP", "0")
    if extra:
        env.update(extra)
    return env


def run_cli(
    repo_root: Path,
    args: list[str],
    *,
    env: dict[str, str] | None = None,
    timeout: float = 180.0,
) -> CliResult:
    """Invoke ``python -m cognis.cli.main --repo-root <root> <args>``.

    This is the exact shape ``cli.ts``'s ``runCli`` uses (module form, repo-root
    flag first), so the contract under test is the real one.
    """
    cmd = [sys.executable, "-m", CLI_MODULE, "--repo-root", str(repo_root), *args]
    proc = subprocess.run(
        cmd,
        cwd=str(repo_root),
        env=_base_env(env),
        capture_output=True,
        text=True,
        timeout=timeout,
    )
    return CliResult(exit_code=proc.returncode, stdout=proc.stdout, stderr=proc.stderr)


def run_cli_json(
    repo_root: Path,
    args: list[str],
    *,
    env: dict[str, str] | None = None,
    timeout: float = 180.0,
) -> Any:
    """Run a CLI command and return parsed JSON, raising on a non-zero exit."""
    result = run_cli(repo_root, args, env=env, timeout=timeout)
    if result.exit_code != 0:
        raise AssertionError(
            f"cognis CLI {args} failed ({result.exit_code}):\n"
            f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}"
        )
    return result.json()


class IndexdProcess:
    """A live ``cognis-indexd`` daemon subprocess for E2E tests.

    Use as a context manager so the daemon is always torn down, even on assert
    failures::

        with IndexdProcess(repo_root, db_path, status_path, full_rebuild=True) as d:
            d.wait_for_phase("watching")
        # daemon stopped here
    """

    def __init__(
        self,
        repo_root: Path,
        db_path: Path,
        status_path: Path,
        *,
        full_rebuild: bool = False,
        env: dict[str, str] | None = None,
    ) -> None:
        self.repo_root = repo_root
        self.db_path = db_path
        self.status_path = status_path
        self.full_rebuild = full_rebuild
        self._env_extra = env
        self.proc: subprocess.Popen[str] | None = None

    def __enter__(self) -> IndexdProcess:
        args = [
            sys.executable,
            "-m",
            INDEXD_MODULE,
            "--repo-root",
            str(self.repo_root),
            "--db-path",
            str(self.db_path),
        ]
        if self.full_rebuild:
            args.append("--full-rebuild")
        env = _base_env(self._env_extra)
        env["COGNIS_DB_PATH"] = str(self.db_path)
        env["COGNIS_INDEXD_STATUS_PATH"] = str(self.status_path)
        self.proc = subprocess.Popen(
            args,
            cwd=str(self.repo_root),
            env=env,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
        )
        # Drain the daemon's stdout in the background so (a) its pipe buffer
        # never fills and stalls the process, and (b) the pipe is consumed so
        # Python's strict ResourceWarning filter stays quiet at teardown.
        self._drained: list[str] = []
        self._drain_thread = threading.Thread(target=self._drain_output, daemon=True)
        self._drain_thread.start()
        return self

    def _drain_output(self) -> None:
        stdout = self.proc.stdout if self.proc else None
        if stdout is None:
            return
        try:
            for line in stdout:
                self._drained.append(line)
        except (ValueError, OSError):
            # Stream closed during shutdown — expected.
            pass

    def read_status(self) -> dict[str, Any] | None:
        """Return the parsed daemon status file, or None if not yet written."""
        if not self.status_path.exists():
            return None
        try:
            raw = self.status_path.read_text(encoding="utf-8").strip()
        except OSError:
            return None
        if not raw:
            return None
        try:
            return json.loads(raw)
        except json.JSONDecodeError:
            return None

    def wait_for_phase(self, phase: str, *, timeout: float = 60.0) -> dict[str, Any]:
        """Block until the status file reports *phase*; return that snapshot."""
        deadline = time.monotonic() + timeout
        last: dict[str, Any] | None = None
        while time.monotonic() < deadline:
            if self.proc is not None and self.proc.poll() is not None:
                output = "".join(getattr(self, "_drained", []))[-2000:]
                raise AssertionError(
                    f"indexd exited early ({self.proc.returncode}) before reaching "
                    f"phase {phase!r}; last status={last}\n--- daemon output ---\n{output}"
                )
            last = self.read_status()
            if last is not None and last.get("phase") == phase:
                return last
            time.sleep(0.15)
        output = "".join(getattr(self, "_drained", []))[-2000:]
        raise AssertionError(
            f"indexd never reached phase {phase!r} within {timeout}s; "
            f"last status={last}\n--- daemon output ---\n{output}"
        )

    def stop(self, *, timeout: float = 30.0) -> int:
        """Terminate the daemon and return its exit code."""
        if self.proc is None:
            return 0
        if self.proc.poll() is None:
            self.proc.terminate()
            try:
                self.proc.wait(timeout=timeout)
            except subprocess.TimeoutExpired:
                self.proc.kill()
                self.proc.wait(timeout=timeout)
        # Join the drain thread and close the stdout pipe so no file handle
        # leaks into pytest's strict ResourceWarning filter.
        drain = getattr(self, "_drain_thread", None)
        if drain is not None:
            drain.join(timeout=5.0)
        if self.proc.stdout is not None:
            with contextlib.suppress(OSError):
                self.proc.stdout.close()
        return self.proc.returncode if self.proc.returncode is not None else 0

    def __exit__(self, *exc: object) -> None:
        self.stop()


def write_sample_repo(repo_root: Path) -> None:
    """Materialize a tiny multi-language repo for indexing.

    Kept intentionally small but real: a Python and a TypeScript file with an
    obvious symbol each, plus a cross-file call so edge resolution has work to
    do. This is what a "fresh user" workspace effectively looks like to the
    indexer.
    """
    (repo_root / "src").mkdir(parents=True, exist_ok=True)
    (repo_root / "src" / "auth.py").write_text(
        "def authenticate(token):\n"
        "    return verify(token)\n"
        "\n\n"
        "def verify(token):\n"
        "    return bool(token)\n",
        encoding="utf-8",
    )
    (repo_root / "src" / "app.ts").write_text(
        "export function createApp(): string {\n"
        "  return greet('world');\n"
        "}\n"
        "\n"
        "export function greet(name: string): string {\n"
        "  return `hello ${name}`;\n"
        "}\n",
        encoding="utf-8",
    )
