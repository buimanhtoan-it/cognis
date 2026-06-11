"""``cognis-indexd`` — long-running incremental indexer daemon.

Wires the existing phase-1 building blocks into a runnable service:

- Cold-indexes the repo if the DB is empty (no ``file`` rows yet).
- Starts a :class:`~cognis_indexer.watcher.RepoWatcher` and drains its queue
  in 500 ms batches.
- Routes :class:`~cognis_indexer.watcher.FileChangeEvent` batches to
  :meth:`IndexerPipeline.index_changed_files` (or :meth:`remove_file` on a
  ``deleted`` event) via ``loop.run_in_executor`` so SQLite writes don't block
  the asyncio event loop.
- Routes :class:`~cognis_indexer.watcher.BranchChangeEvent` to a full
  incremental walk via :meth:`IndexerPipeline.index_repo` (idempotency makes
  this cheap when the branch barely changed).
- Handles ``SIGINT`` and ``SIGTERM`` cleanly so ``cognis-cli down`` and
  Ctrl-C both quit promptly without leaking the watcher thread.
"""

from __future__ import annotations

import argparse
import asyncio
import contextlib
import json
import logging
import os
import signal
import sys
import time
from collections.abc import Sequence
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from typing import TYPE_CHECKING, Any

from cognis.config import Config
from cognis.db import Database
from cognis_indexer.pipeline import IndexerPipeline
from cognis_indexer.watcher import (
    BranchChangeEvent,
    FileChangeEvent,
    RepoWatcher,
)

if TYPE_CHECKING:
    from cognis_indexer.embedder import Embedder
    from cognis_indexer.watcher.watcher import WatcherEvent

logger = logging.getLogger(__name__)

# How long to wait (seconds) collecting more events before dispatching a batch.
# Matches the design: 500 ms batching keeps the writer transaction count low
# under burst-edit workloads (e.g. an IDE save-all).
_BATCH_WINDOW_S: float = 0.5
_STATUS_POLL_S: float = 0.2
_STATUS_FILE_NAME = "indexd-status.json"


# ---------------------------------------------------------------------------
# Embedder construction
# ---------------------------------------------------------------------------


def _build_embedder(config: Config) -> Embedder | None:
    """Construct the configured embedder, or ``None`` if unavailable.

    The daemon never refuses to start because the embedder is missing — that
    would block lexical and structural retrieval too. Instead we log a warning
    and continue with ``embedder=None``.

    Backend selection is delegated to :func:`cognis_indexer.registry.build_embedder`
    so this daemon, ``cognis-mcpd``, ``cognis-cli``, and the eval harness all
    resolve ``config.embedder.backend`` through the same registry.
    """
    from cognis_indexer.registry import UnknownEmbedderBackendError, build_embedder

    try:
        return build_embedder(config.embedder)
    except UnknownEmbedderBackendError as exc:
        logger.warning("%s; continuing without semantic vectors", exc)
        return None
    except ImportError as exc:
        logger.warning(
            "embedder %s unavailable (%s); continuing without semantic vectors",
            config.embedder.backend,
            exc,
        )
        return None


# ---------------------------------------------------------------------------
# Cold-index check
# ---------------------------------------------------------------------------


def _db_is_empty(db: Database) -> bool:
    """Return True when the DB has no ``file`` rows (i.e. needs a cold index)."""
    conn = db.connect()
    row = conn.execute("SELECT COUNT(*) FROM file").fetchone()
    return int(row[0]) == 0


# ---------------------------------------------------------------------------
# Daemon main loop
# ---------------------------------------------------------------------------


def _resolve_db_path(repo_root: Path, db_path_override: Path | None = None) -> Path:
    """Return UCKG path: explicit override, then ``COGNIS_DB_PATH``, then default."""
    if db_path_override is not None:
        return db_path_override.expanduser().resolve()
    env = os.environ.get("COGNIS_DB_PATH")
    if env:
        return Path(env).expanduser().resolve()
    return (repo_root / ".cognis" / "uckg.db").resolve()


def _resolve_status_path(repo_root: Path) -> Path:
    """Return the daemon status JSON path used by IDE integrations."""
    env = os.environ.get("COGNIS_INDEXD_STATUS_PATH")
    if env:
        return Path(env).expanduser().resolve()
    return (repo_root / ".cognis" / _STATUS_FILE_NAME).resolve()


def _write_status_file(path: Path, payload: dict[str, Any]) -> None:
    """Write the daemon status snapshot atomically.

    The status file is polled concurrently by IDE integrations (the VS Code
    extension reads it on a timer). On Windows, ``os.replace`` raises
    ``PermissionError`` (WinError 5 / 32) if the destination is momentarily
    open by such a reader. That is a transient sharing violation, not a real
    failure, so retry the rename a few times with a short backoff before giving
    up. Crashing the writer here would take down the whole indexer daemon over
    a cosmetic status update.
    """
    path.parent.mkdir(parents=True, exist_ok=True)
    text = json.dumps(payload, indent=2, sort_keys=True) + "\n"
    tmp_path = path.with_name(f"{path.name}.tmp")
    tmp_path.write_text(text, encoding="utf-8")

    last_error: OSError | None = None
    for attempt in range(10):
        try:
            tmp_path.replace(path)
            return
        except PermissionError as exc:
            # A concurrent reader holds the destination open; back off briefly.
            last_error = exc
            time.sleep(0.02 * (attempt + 1))
    # Could not swap atomically after retries. Drop this snapshot rather than
    # crashing the daemon; the next tick will publish a fresh one.
    logger.debug("status file replace failed after retries: %s", last_error)
    with contextlib.suppress(OSError):
        tmp_path.unlink()


def _relative_paths(paths: Sequence[Path], repo_root: Path, *, limit: int = 8) -> list[str]:
    """Convert absolute paths to repo-relative strings for UI display."""
    rel_paths: list[str] = []
    for abs_path in paths[:limit]:
        try:
            rel_paths.append(abs_path.resolve().relative_to(repo_root).as_posix())
        except ValueError:
            rel_paths.append(abs_path.as_posix())
    return rel_paths


def _compose_status_payload(
    *,
    watcher: RepoWatcher | None,
    runtime_status: dict[str, Any],
) -> dict[str, Any]:
    """Merge runtime state with pending debounce paths for IDE consumption."""
    phase = str(runtime_status.get("phase", "starting"))
    message = str(runtime_status.get("message", "Starting live indexing…"))
    progress_percent = runtime_status.get("progress_percent")
    pending_files = watcher.pending_paths(limit=8) if watcher is not None else []
    pending_count = watcher.pending_count() if watcher is not None else 0
    inflight_files = [str(path) for path in runtime_status.get("inflight_files", [])]
    recent_files = [str(path) for path in runtime_status.get("recent_files", [])]

    if phase == "watching" and pending_count > 0 and not inflight_files:
        message = (
            f"Queued {pending_count} file change{'s' if pending_count != 1 else ''} for indexing."
        )
        progress_percent = 20.0

    return {
        "pid": os.getpid(),
        "active": bool(runtime_status.get("active", True)),
        "phase": phase,
        "message": message,
        "progress_percent": progress_percent,
        "pending_count": pending_count,
        "pending_files": pending_files,
        "inflight_count": len(inflight_files),
        "inflight_files": inflight_files,
        "recent_files": recent_files[:8],
        "last_error": runtime_status.get("last_error"),
        "updated_at": time.time(),
    }


async def _status_writer_loop(
    *,
    status_path: Path,
    watcher: RepoWatcher | None,
    runtime_status: dict[str, Any],
    stop_event: asyncio.Event,
) -> None:
    """Publish daemon status snapshots until *stop_event* is set."""
    last_serialized: str | None = None
    while True:
        payload = _compose_status_payload(watcher=watcher, runtime_status=runtime_status)
        comparable = dict(payload)
        comparable.pop("updated_at", None)
        serialized = json.dumps(comparable, sort_keys=True)
        if serialized != last_serialized:
            await asyncio.to_thread(_write_status_file, status_path, payload)
            last_serialized = serialized
        if stop_event.is_set():
            break
        await asyncio.sleep(_STATUS_POLL_S)


async def run_daemon(
    repo_root: Path,
    config: Config,
    *,
    db_path_override: Path | None = None,
    force_full_rebuild: bool = False,
) -> int:
    """Run the indexer daemon until cancelled.

    Returns ``0`` on a clean shutdown. Any exception inside the main loop is
    logged and returned as ``1`` — process supervisors (systemd, k8s) can
    treat that as a restart signal.
    """
    repo_root = repo_root.resolve()
    if not repo_root.is_dir():
        logger.error("repo root does not exist or is not a directory: %s", repo_root)
        return 1

    db_path = _resolve_db_path(repo_root, db_path_override)
    status_path = _resolve_status_path(repo_root)
    db_path.parent.mkdir(parents=True, exist_ok=True)
    os.environ.setdefault("COGNIS_DB_PATH", str(db_path))
    db = Database(str(db_path))

    embedder = _build_embedder(config)
    pipeline = IndexerPipeline(db=db, config=config, embedder=embedder)
    runtime_status: dict[str, Any] = {
        "active": True,
        "phase": "starting",
        "message": "Starting live indexing daemon…",
        "progress_percent": 5.0,
        "inflight_files": [],
        "recent_files": [],
        "last_error": None,
    }
    await asyncio.to_thread(
        _write_status_file,
        status_path,
        _compose_status_payload(watcher=None, runtime_status=runtime_status),
    )

    loop = asyncio.get_running_loop()

    # All pipeline DB work runs on a single dedicated worker thread. This
    # matches the design's "dedicated writer" intent (serialized SQLite writes,
    # no cross-thread connection sharing) and — crucially — gives us a thread
    # whose cached connection we can close deterministically on shutdown,
    # instead of leaking it into asyncio's shared default executor pool.
    index_executor = ThreadPoolExecutor(max_workers=1, thread_name_prefix="cognis-indexd-writer")

    def _close_executor_db_connection() -> None:
        """Release the worker thread's cached SQLite connection (runs on it)."""
        db.close_thread_connection()

    # Run a full rebuild when explicitly requested, or cold-index when the DB
    # is empty. Done synchronously inside an executor so the event loop can
    # serve a SIGINT during the (potentially long) walk.
    if force_full_rebuild or _db_is_empty(db):
        was_empty = _db_is_empty(db)
        runtime_status.update(
            phase="cold_index" if was_empty else "rebuild",
            message=(
                "Building initial index for this workspace…"
                if was_empty
                else "Rebuilding the semantic index for this workspace…"
            ),
            progress_percent=15.0,
            inflight_files=[],
            recent_files=[],
            last_error=None,
        )
        await asyncio.to_thread(
            _write_status_file,
            status_path,
            _compose_status_payload(watcher=None, runtime_status=runtime_status),
        )
        logger.info(
            "%s — running full index of %s",
            "full rebuild requested" if force_full_rebuild else "DB empty",
            repo_root,
        )
        # Two-phase cold index so the workspace becomes searchable in seconds
        # instead of waiting minutes for embeddings.
        #
        # Phase A: index lexical + structural data with embeddings SKIPPED. This
        # is fast (seconds) and commits every file/symbol, so health flips to
        # "ok" and lexical/structural search works immediately. Embedding the
        # whole repo up front (Pass 3 of index_repo embeds *all* symbols before
        # any write) is what previously left the DB empty — and health reporting
        # "0 files / fail" — for the entire multi-minute embed on a real repo.
        lexical_stats = await loop.run_in_executor(
            index_executor,
            lambda: pipeline.index_repo(repo_root, full=True, skip_embeddings=True),
        )
        logger.info(
            "lexical index complete: files=%d symbols=%d edges=%d errors=%d in %.2fs",
            lexical_stats.files_processed,
            lexical_stats.symbols_indexed,
            lexical_stats.edges_resolved,
            len(lexical_stats.errors),
            lexical_stats.elapsed_s,
        )

        # Phase B: backfill semantic embeddings. The index is already queryable;
        # this only upgrades semantic search. Skipped automatically when no
        # embedder is configured/available (index_repo no-ops the embed step).
        embedder_available = getattr(pipeline, "embedder", None) is not None
        if embedder_available:
            runtime_status.update(
                phase="embedding",
                message="Index ready — generating semantic embeddings in the background…",
                progress_percent=70.0,
                inflight_files=[],
                recent_files=[],
                last_error=None,
            )
            await asyncio.to_thread(
                _write_status_file,
                status_path,
                _compose_status_payload(watcher=None, runtime_status=runtime_status),
            )

            # Live embedding progress: embedding is the dominant cost of a cold
            # index, so move the bar from 70→100% as vectors are generated
            # instead of sitting at a static 70% for minutes. The callback runs
            # on the executor thread and only mutates primitive status fields;
            # the status-writer loop publishes them on its 0.2s tick.
            def _on_embed_progress(done: int, total: int) -> None:
                pct = 70.0 + 30.0 * (done / total) if total > 0 else 70.0
                runtime_status.update(
                    phase="embedding",
                    message=f"Generating semantic embeddings… {done}/{total} symbols (search already works)",
                    progress_percent=round(pct, 1),
                )
                # The continuous status-writer loop is not running yet during
                # cold index, so publish directly from this executor thread
                # (atomic write) — otherwise the bar would sit static at 70%.
                _write_status_file(
                    status_path,
                    _compose_status_payload(watcher=None, runtime_status=runtime_status),
                )

            embed_stats = await loop.run_in_executor(
                index_executor,
                lambda: pipeline.index_repo(
                    repo_root,
                    full=True,
                    skip_embeddings=False,
                    embed_progress=_on_embed_progress,
                ),
            )
            logger.info(
                "embedding backfill complete: files=%d symbols=%d in %.2fs",
                embed_stats.files_processed,
                embed_stats.symbols_indexed,
                embed_stats.elapsed_s,
            )
    else:
        # On daemon restart with a populated DB, run a quick incremental walk
        # to catch any changes that landed while the daemon was down.
        runtime_status.update(
            phase="sweep",
            message="Syncing index with the current workspace state…",
            progress_percent=25.0,
            inflight_files=[],
            recent_files=[],
            last_error=None,
        )
        await asyncio.to_thread(
            _write_status_file,
            status_path,
            _compose_status_payload(watcher=None, runtime_status=runtime_status),
        )
        logger.info("DB populated — running incremental sweep")
        sweep_stats = await loop.run_in_executor(
            index_executor, lambda: pipeline.index_repo(repo_root, full=False)
        )
        logger.info(
            "sweep complete: processed=%d skipped=%d errors=%d in %.2fs",
            sweep_stats.files_processed,
            sweep_stats.files_skipped,
            len(sweep_stats.errors),
            sweep_stats.elapsed_s,
        )

    # Start the watcher.
    queue: asyncio.Queue[WatcherEvent] = asyncio.Queue()
    watcher = RepoWatcher(repo_root=repo_root, config=config, queue=queue)
    await watcher.start()
    runtime_status.update(
        phase="watching",
        message="Watching for file changes.",
        progress_percent=100.0,
        inflight_files=[],
        last_error=None,
    )
    logger.info("watcher started; entering event loop")

    # Install signal handlers that just cancel the main task. Some platforms
    # (Windows) don't allow ``add_signal_handler``; fall back to the default
    # KeyboardInterrupt path there.
    stop_event = asyncio.Event()
    status_task = asyncio.create_task(
        _status_writer_loop(
            status_path=status_path,
            watcher=watcher,
            runtime_status=runtime_status,
            stop_event=stop_event,
        )
    )

    def _request_stop() -> None:
        logger.info("shutdown signal received")
        stop_event.set()

    for sig_name in ("SIGINT", "SIGTERM"):
        sig = getattr(signal, sig_name, None)
        if sig is None:
            continue
        with contextlib.suppress(NotImplementedError):
            # Windows ProactorEventLoop doesn't support add_signal_handler.
            # Ctrl-C still raises KeyboardInterrupt and breaks out of the
            # ``await queue.get()``; we catch that below.
            loop.add_signal_handler(sig, _request_stop)

    exit_code = 0
    try:
        while not stop_event.is_set():
            try:
                event = await asyncio.wait_for(queue.get(), timeout=1.0)
            except TimeoutError:
                continue
            except KeyboardInterrupt:
                _request_stop()
                break

            await _handle_event_batch(
                first=event,
                queue=queue,
                pipeline=pipeline,
                repo_root=repo_root,
                loop=loop,
                runtime_status=runtime_status,
                executor=index_executor,
            )
    except Exception:
        runtime_status.update(
            phase="error",
            message="Live indexing crashed.",
            progress_percent=0.0,
            active=False,
            inflight_files=[],
            last_error="daemon loop crashed",
        )
        logger.exception("daemon loop crashed")
        exit_code = 1
    finally:
        logger.info("stopping watcher")
        runtime_status.update(
            active=False,
            phase="stopped",
            message="Live indexing stopped.",
            progress_percent=0.0,
            inflight_files=[],
        )
        stop_event.set()
        await status_task
        await watcher.stop()
        # ``pipeline.close()`` releases the writer's cached connection on *this*
        # (loop) thread — the same handle ``_db_is_empty`` opened above.
        pipeline.close()
        # The pipeline's actual DB writes ran on the dedicated worker thread, so
        # close that thread's cached connection on the thread itself, then drain
        # the executor. Without this the connection would only be finalized at
        # interpreter exit, leaking a file handle (and tripping SQLite's
        # same-thread close guard) on a long-lived host.
        with contextlib.suppress(Exception):
            await loop.run_in_executor(index_executor, _close_executor_db_connection)
        index_executor.shutdown(wait=True)
        db.close_thread_connection()

    return exit_code


async def _handle_event_batch(
    *,
    first: WatcherEvent,
    queue: asyncio.Queue[WatcherEvent],
    pipeline: IndexerPipeline,
    repo_root: Path,
    loop: asyncio.AbstractEventLoop,
    runtime_status: dict[str, Any],
    executor: ThreadPoolExecutor,
) -> None:
    """Drain *queue* for up to ``_BATCH_WINDOW_S`` seconds then dispatch.

    Branch-change events bypass batching: a ref switch can invalidate a large
    portion of the index, so we kick off a re-walk immediately.

    All pipeline DB work is dispatched onto *executor* — the daemon's single
    dedicated writer thread — so SQLite writes stay serialized on one thread
    whose connection is closed deterministically at shutdown.
    """
    if isinstance(first, BranchChangeEvent):
        runtime_status.update(
            phase="branch_change",
            message=f"Branch changed to {first.new_ref}. Refreshing index…",
            progress_percent=35.0,
            inflight_files=[],
            last_error=None,
        )
        logger.info("branch change %s -> %s; re-walking repo", first.old_ref, first.new_ref)
        await loop.run_in_executor(executor, lambda: pipeline.index_repo(repo_root, full=False))
        runtime_status.update(
            phase="watching",
            message="Watching for file changes.",
            progress_percent=100.0,
            inflight_files=[],
        )
        return

    # Collect file events for the batch window.
    file_events: list[FileChangeEvent] = []
    branch_events: list[BranchChangeEvent] = []
    if isinstance(first, FileChangeEvent):
        file_events.append(first)

    deadline = loop.time() + _BATCH_WINDOW_S
    while True:
        remaining = deadline - loop.time()
        if remaining <= 0:
            break
        try:
            event = await asyncio.wait_for(queue.get(), timeout=remaining)
        except TimeoutError:
            break
        if isinstance(event, BranchChangeEvent):
            branch_events.append(event)
        elif isinstance(event, FileChangeEvent):
            file_events.append(event)

    # Dispatch deletes, then re-indexes. The pipeline handles unknown paths
    # gracefully via the resolver/walker filters, but separating the two paths
    # keeps the writer's per-file transaction semantics tight.
    deletes: list[Path] = []
    changes: list[Path] = []
    seen: set[str] = set()
    for fe in file_events:
        if fe.path in seen:
            continue
        seen.add(fe.path)
        abs_path = (repo_root / fe.path).resolve()
        if fe.kind == "deleted":
            deletes.append(abs_path)
        else:
            changes.append(abs_path)

    for abs_path in deletes:
        runtime_status.update(
            phase="incremental",
            message=(
                f"Removing {len(deletes)} deleted file{'s' if len(deletes) != 1 else ''} "
                "from the index…"
            ),
            progress_percent=45.0,
            inflight_files=_relative_paths(deletes, repo_root),
            recent_files=_relative_paths(deletes, repo_root),
            last_error=None,
        )
        await loop.run_in_executor(executor, pipeline.remove_file, abs_path, repo_root)

    if changes:
        change_paths = _relative_paths(changes, repo_root)
        runtime_status.update(
            phase="incremental",
            message=(f"Indexing {len(changes)} changed file{'s' if len(changes) != 1 else ''}…"),
            progress_percent=65.0,
            inflight_files=change_paths,
            recent_files=change_paths,
            last_error=None,
        )
        stats = await loop.run_in_executor(
            executor, lambda: pipeline.index_changed_files(changes, repo_root)
        )
        logger.info(
            "incremental: files=%d symbols=%d edges=%d errors=%d",
            stats.files_processed,
            stats.symbols_indexed,
            stats.edges_resolved,
            len(stats.errors),
        )
        runtime_status.update(
            recent_files=change_paths,
            inflight_files=[],
            progress_percent=90.0 if branch_events else 100.0,
        )

    # If a branch change snuck into the batch window, re-walk now.
    if branch_events:
        runtime_status.update(
            phase="branch_change",
            message="Branch change detected during indexing. Refreshing workspace…",
            progress_percent=80.0,
            inflight_files=[],
            last_error=None,
        )
        logger.info("branch change observed during batch window; re-walking")
        await loop.run_in_executor(executor, lambda: pipeline.index_repo(repo_root, full=False))
    runtime_status.update(
        phase="watching",
        message="Watching for file changes.",
        progress_percent=100.0,
        inflight_files=[],
    )


# ---------------------------------------------------------------------------
# CLI entry point
# ---------------------------------------------------------------------------


def _build_argparser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="cognis-indexd",
        description="Long-running incremental indexer daemon for cognis.",
    )
    parser.add_argument(
        "--repo-root",
        type=Path,
        default=Path.cwd(),
        help="Repository root to watch (default: current working directory).",
    )
    parser.add_argument(
        "--config",
        type=Path,
        default=None,
        help="Optional explicit path to .cognis/config.yaml (overrides repo-root lookup).",
    )
    parser.add_argument(
        "--log-level",
        default="INFO",
        choices=["DEBUG", "INFO", "WARNING", "ERROR"],
        help="Logging level (default: INFO).",
    )
    parser.add_argument(
        "--db-path",
        type=Path,
        default=None,
        help="UCKG database path (default: COGNIS_DB_PATH or <repo>/.cognis/uckg.db).",
    )
    parser.add_argument(
        "--full-rebuild",
        action="store_true",
        help="Force a full index rebuild before starting the watcher.",
    )
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    """CLI entry point invoked by the ``cognis-indexd`` console script."""
    args = _build_argparser().parse_args(argv)

    logging.basicConfig(
        level=args.log_level,
        format="%(asctime)s %(levelname)-7s %(name)s: %(message)s",
    )
    from cognis.branding import echo_banner

    echo_banner(prog="cognis-indexd")

    if args.config is not None:
        config = Config.from_yaml(args.config)
    else:
        config = Config.load(args.repo_root)

    try:
        return asyncio.run(
            run_daemon(
                args.repo_root,
                config,
                db_path_override=args.db_path,
                force_full_rebuild=args.full_rebuild,
            )
        )
    except KeyboardInterrupt:
        logger.info("interrupted; exiting")
        return 0


if __name__ == "__main__":
    sys.exit(main())
