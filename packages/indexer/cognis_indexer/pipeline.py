"""End-to-end indexer pipeline orchestrator.

Wires together the existing phase 1 building blocks into a runnable indexer:

    walker → parser → enricher → embedder → resolver → writer (+ FTS sync)

This module is the missing glue between the per-stage components shipped by
tasks 6 (parsers), 7 (watcher), 8 (resolver), 9 (enricher), 10 (embedder), and
11 (writer). It exposes three execution modes used by ``cognis-cli index`` and
``cognis-indexd``:

* :meth:`IndexerPipeline.index_repo` — cold or incremental walk of a repository.
* :meth:`IndexerPipeline.index_changed_files` — process a known set of paths,
  resolving cross-file edges against the union of those plus all symbols still
  in the DB.
* :meth:`IndexerPipeline.index_file` — single-file convenience (no edges).
* :meth:`IndexerPipeline.remove_file` — apply the watcher's ``deleted`` events
  via the writer's cascade logic.

Design notes
------------

Idempotency (REQ-IDX-2): every file's ``sha256(file_bytes)[:16]`` is stored in
``file.content_hash``. When ``full=False`` and the on-disk hash matches the DB
row, the file is skipped entirely. ``full=True`` forces re-parse.

Edge resolution: cross-file edges only exist if both endpoints are visible to
the resolver in the same call. ``index_repo`` collects every parsed symbol
across the run before invoking
:func:`cognis_indexer.resolver.pipeline.resolve_edges`; ``index_changed_files``
augments the changed-file symbols with the rest of the DB so an edited symbol
can still resolve calls into untouched files.

Error containment: parser/enricher/embedder failures for a single file are
captured in :class:`IndexerStats.errors` and never bubble out. The pipeline
exits cleanly even on broken source files so a half-broken repo can still
index the healthy parts.

Async story: the writer exposes an ``await``-able interface but the underlying
SQLite work is synchronous. The pipeline is a sync API; the daemon wraps it in
``loop.run_in_executor`` to avoid blocking the watcher event loop.
"""

from __future__ import annotations

import contextlib
import hashlib
import logging
import os
import time
from collections.abc import Callable, Iterable, Iterator
from dataclasses import dataclass, field
from pathlib import Path
from typing import TYPE_CHECKING, Any

from cognis.config import Config
from cognis.db import Database, get_file, now_epoch
from cognis_retrieval.lexical import populate_fts

from cognis_indexer.embedder import Embedder, build_embedding_text
from cognis_indexer.enricher import EnrichedSymbol, Enricher
from cognis_indexer.parsers.base import LanguageParser, ParsedSymbol
from cognis_indexer.resolver.base import ResolvedEdge
from cognis_indexer.resolver.pipeline import resolve_edges
from cognis_indexer.watcher.gitignore import GitignoreFilter
from cognis_indexer.writer import FileWritePayload, IndexWriter

if TYPE_CHECKING:
    from cognis.models import SymbolNode

__all__ = ["IndexerPipeline", "IndexerStats"]

logger = logging.getLogger(__name__)
_EMBED_CHUNK_SIZE = max(1, int(os.environ.get("COGNIS_INDEX_EMBED_CHUNK_SIZE", "256")))
_SQL_PARAM_CHUNK = 500


# ---------------------------------------------------------------------------
# File-extension → language id table
# ---------------------------------------------------------------------------

_LANG_BY_EXT: dict[str, str] = {
    ".ts": "typescript",
    ".tsx": "typescript",
    ".py": "python",
    ".go": "go",
}
"""Map filename extension to the language id used by the parser registry."""


# ---------------------------------------------------------------------------
# IndexerStats
# ---------------------------------------------------------------------------


@dataclass
class IndexerStats:
    """Per-run counters produced by :class:`IndexerPipeline`.

    All counters are additive across files in a single run. Errors are
    accumulated as plain strings so callers can render them without depending
    on exception types.
    """

    files_processed: int = 0
    """Files that were parsed and written in this run."""

    files_skipped: int = 0
    """Files whose ``content_hash`` already matched the DB row (idempotency)."""

    symbols_indexed: int = 0
    """Total symbols persisted across all processed files."""

    edges_resolved: int = 0
    """Total resolved edges persisted across all processed files."""

    secrets_redacted: int = 0
    """Number of symbols whose enricher flagged ``"secret_redacted"``."""

    errors: list[str] = field(default_factory=list)
    """Human-readable error strings (one per file that failed)."""

    elapsed_s: float = 0.0
    """Wall-clock duration of the run in seconds."""

    def merge(self, other: IndexerStats) -> None:
        """Fold *other* into self in-place. Used to accumulate per-file results."""
        self.files_processed += other.files_processed
        self.files_skipped += other.files_skipped
        self.symbols_indexed += other.symbols_indexed
        self.edges_resolved += other.edges_resolved
        self.secrets_redacted += other.secrets_redacted
        self.errors.extend(other.errors)
        # ``elapsed_s`` is only set on the top-level run, not merged.


# ---------------------------------------------------------------------------
# Internal per-file intermediate result
# ---------------------------------------------------------------------------


@dataclass
class _FileResult:
    """Intermediate state for one file between parsing and writing.

    Holds everything the writer needs except cross-file edges, which are only
    available after the resolver runs on the full batch.
    """

    rel_path: str
    language: str
    file_size_bytes: int
    content_hash: str
    enriched: list[EnrichedSymbol]
    parse_status: str  # "ok" | "partial" | "failed"

    @property
    def parsed_symbols(self) -> list[ParsedSymbol]:
        """Return the (possibly redacted) parsed symbols ready for resolver/writer."""
        return [e.symbol for e in self.enriched]


# ---------------------------------------------------------------------------
# IndexerPipeline
# ---------------------------------------------------------------------------


class IndexerPipeline:
    """End-to-end orchestrator wiring the phase 1 building blocks.

    The pipeline is constructed once per process. ``embedder`` is optional —
    pass ``None`` to skip embedding (lexical and structural retrieval still
    work; only semantic search is degraded).

    Args:
        db: Open :class:`cognis.db.Database` handle. The pipeline does not
            close the DB; callers own its lifecycle.
        config: Loaded :class:`cognis.config.Config`. Used for
            ``repo.ignore`` patterns and language enable-list.
        embedder: Optional :class:`cognis_indexer.embedder.Embedder`. When
            ``None`` no vectors are produced.
    """

    def __init__(
        self,
        db: Database,
        config: Config,
        embedder: Embedder | None = None,
    ) -> None:
        self.db = db
        self.config = config
        self.embedder = embedder

        # When a real embedder is plugged in, align the DB's vector dimension to
        # it. A model swap to a different vector size recreates ``symbol_vec``
        # at the new dim; vectors are regenerated on this index pass.
        if embedder is not None:
            dim = getattr(embedder, "embedding_dim", None)
            if isinstance(dim, int) and dim > 0:
                db.reconcile_embedding_dim(dim)

        # Parsers are loaded lazily — instantiating tree-sitter parsers up
        # front is wasteful when a repo only contains one language.
        self._parsers: dict[str, LanguageParser] = {}
        self._enabled_languages: set[str] = set(config.languages.enabled)

        self.enricher = Enricher()
        self.writer = IndexWriter(db)

    # ------------------------------------------------------------------
    # Lifecycle
    # ------------------------------------------------------------------

    def close(self) -> None:
        """Release the writer's per-thread DB connection. Idempotent."""
        self.writer.close()

    # ------------------------------------------------------------------
    # Parser registry
    # ------------------------------------------------------------------

    def _get_parser(self, language: str) -> LanguageParser | None:
        """Return a parser for *language*, lazily constructing on first use.

        Returns ``None`` when the language is not in ``config.languages.enabled``
        OR when the underlying tree-sitter grammar is not installed (we do
        *not* let an ImportError abort the run — the file is simply skipped).
        """
        if language not in self._enabled_languages:
            return None

        cached = self._parsers.get(language)
        if cached is not None:
            return cached

        parser: LanguageParser | None = None
        try:
            if language == "python":
                from cognis_indexer.parsers.python import PythonParser

                parser = PythonParser()
            elif language == "typescript":
                from cognis_indexer.parsers.typescript import TypeScriptParser

                parser = TypeScriptParser()
            elif language == "go":
                from cognis_indexer.parsers.go import GoParser

                parser = GoParser()
            else:
                # Unknown language id — defensive arm; should never happen
                # because the ext map only emits known languages.
                return None
        except ImportError as exc:
            logger.warning("parser for %s unavailable: %s", language, exc)
            # Cache a sentinel-by-absence so we don't retry the import for
            # every single file of this language.
            self._enabled_languages.discard(language)
            return None

        self._parsers[language] = parser
        return parser

    # ------------------------------------------------------------------
    # File walking + filtering
    # ------------------------------------------------------------------

    @staticmethod
    def _detect_language(path: Path) -> str | None:
        """Return language id for *path* or ``None`` for unsupported types."""
        return _LANG_BY_EXT.get(path.suffix.lower())

    def _make_gitignore(self, repo_root: Path) -> GitignoreFilter:
        """Build a :class:`GitignoreFilter` from ``.gitignore`` + config patterns."""
        return GitignoreFilter.from_repo(
            repo_root,
            extra_patterns=list(self.config.repo.ignore),
        )

    def _walk_repo(self, repo_root: Path) -> Iterator[Path]:
        """Yield indexable files under *repo_root*.

        Excludes anything matching the gitignore filter (which always blocks
        ``.git/``) and anything whose extension isn't in ``_LANG_BY_EXT``.

        We use :func:`os.walk` with in-place sorting so iteration stays
        deterministic without materializing the entire repository tree in
        memory up front.
        """
        gitignore = self._make_gitignore(repo_root)

        for current_root, dirnames, filenames in os.walk(repo_root, topdown=True):
            dirnames.sort()
            filenames.sort()
            current_root_path = Path(current_root)

            kept_dirnames: list[str] = []
            for dirname in dirnames:
                dir_path = current_root_path / dirname
                try:
                    rel_dir = dir_path.relative_to(repo_root).as_posix()
                except ValueError:
                    continue
                if gitignore.is_ignored(rel_dir) or gitignore.is_ignored(f"{rel_dir}/"):
                    continue
                kept_dirnames.append(dirname)
            dirnames[:] = kept_dirnames

            for filename in filenames:
                path = current_root_path / filename
                try:
                    rel = path.relative_to(repo_root).as_posix()
                except ValueError:
                    continue

                if gitignore.is_ignored(rel):
                    continue

                if self._detect_language(path) is None:
                    continue

                yield path

    # ------------------------------------------------------------------
    # Single-file processing
    # ------------------------------------------------------------------

    @staticmethod
    def _compute_file_hash(file_bytes: bytes) -> str:
        """Return ``sha256(file_bytes)[:16]`` — used as ``file.content_hash``."""
        return hashlib.sha256(file_bytes).hexdigest()[:16]

    def _read_and_hash(self, abs_path: Path) -> tuple[bytes, str]:
        """Read *abs_path* and return ``(bytes, content_hash)``.

        Raises :class:`OSError` on read failure. Callers must catch.
        """
        file_bytes = abs_path.read_bytes()
        return file_bytes, self._compute_file_hash(file_bytes)

    def _process_file(
        self,
        abs_path: Path,
        repo_root: Path,
        *,
        full: bool,
    ) -> _FileResult | None:
        """Parse + enrich one file. Returns the intermediate result or ``None``.

        Returns ``None`` when the file is skipped (idempotency hit, unsupported
        language, or read/parse failure that has already been recorded).

        Raises :class:`_FileSkip` to signal "skipped due to content_hash match"
        — callers should bump ``files_skipped``. Other exceptions are captured
        as error strings and the file is skipped.
        """
        # Currently this private method is called from index_repo wrapper that
        # handles error capture; to keep the signature clean we return None on
        # any failure and the caller increments the right counter via the
        # outer wrapper.
        raise NotImplementedError  # see _process_file_capture below

    def _parse_and_enrich(
        self,
        abs_path: Path,
        repo_root: Path,
    ) -> _FileResult:
        """Parse and enrich *abs_path*. Raises on any I/O or parser error.

        Caller is responsible for catching exceptions and recording them in
        ``stats.errors``.
        """
        rel = abs_path.relative_to(repo_root).as_posix()
        language = self._detect_language(abs_path)
        if language is None:
            # Defensive — the walker already filtered these out.
            raise ValueError(f"unsupported file extension: {abs_path}")

        parser = self._get_parser(language)
        if parser is None:
            raise ValueError(f"parser for {language!r} is not available")

        file_bytes, content_hash = self._read_and_hash(abs_path)

        try:
            source = file_bytes.decode("utf-8")
        except UnicodeDecodeError:
            # Treat non-UTF-8 files as failed parses. We still want to write
            # a `file` row marking the parse_status.
            return _FileResult(
                rel_path=rel,
                language=language,
                file_size_bytes=len(file_bytes),
                content_hash=content_hash,
                enriched=[],
                parse_status="failed",
            )

        symbols = parser.parse(source, rel)
        enriched = [self.enricher.enrich(s) for s in symbols]

        parse_status = "ok"
        if not enriched and source.strip():
            # Source had content but parser found nothing — likely a partial
            # parse (tree-sitter recovers but produces no symbols of interest).
            parse_status = "partial"

        return _FileResult(
            rel_path=rel,
            language=language,
            file_size_bytes=len(file_bytes),
            content_hash=content_hash,
            enriched=enriched,
            parse_status=parse_status,
        )

    # ------------------------------------------------------------------
    # Embedding
    # ------------------------------------------------------------------

    def _embed_results(
        self,
        results: list[_FileResult],
        *,
        skip_embeddings: bool,
        progress: Callable[[int, int], None] | None = None,
    ) -> dict[str, object]:
        """Return ``{content_hash: np.ndarray}`` for every enriched symbol.

        When ``skip_embeddings`` is True or ``self.embedder is None`` the dict
        is empty. Embedding is batched once across the whole call set so the
        embedder's worker pool is fully utilised.

        ``progress(done, total)``, when supplied, is invoked after each embedded
        chunk so a caller (e.g. the indexd daemon) can publish live "embeddings
        X/N" progress — embedding is the dominant cost of a cold index, so this
        is what turns a multi-minute opaque wait into a moving bar.

        We key by ``content_hash`` rather than ``symbol_id`` because identical
        bodies (e.g. duplicate stub functions) share the same vector — this
        also feeds the embedder's LRU cache (CP-6).
        """
        if skip_embeddings or self.embedder is None:
            return {}
        # Bind to a local so the type narrows to ``Embedder`` (non-None) inside
        # the nested ``_flush_chunk`` closure — mypy cannot narrow a mutable
        # instance attribute across a function boundary.
        embedder = self.embedder

        # Collect unique (content_hash → embedding text) pairs. Iterate
        # symbols once to build the mapping; keep insertion order so the
        # batch is deterministic for tests.
        text_by_hash: dict[str, str] = {}
        for fr in results:
            for enriched in fr.enriched:
                ch = enriched.symbol.content_hash
                if ch in text_by_hash:
                    continue
                text_by_hash[ch] = build_embedding_text(enriched)

        if not text_by_hash:
            return {}

        total = len(text_by_hash)
        embeddings: dict[str, object] = {}
        chunk_hashes: list[str] = []
        chunk_texts: list[str] = []

        def _flush_chunk() -> bool:
            if not chunk_hashes:
                return True
            try:
                vectors = embedder.embed_batch(chunk_texts)
            except Exception as exc:
                # An embedder failure should not fail the whole index — degrade
                # to "no vectors" and let lexical/structural retrieval still work.
                logger.warning("embedder failed; continuing without vectors: %s", exc)
                return False
            for i, content_hash in enumerate(chunk_hashes):
                embeddings[content_hash] = vectors[i]
            chunk_hashes.clear()
            chunk_texts.clear()
            if progress is not None:
                with contextlib.suppress(Exception):
                    progress(len(embeddings), total)
            return True

        for content_hash, text in text_by_hash.items():
            chunk_hashes.append(content_hash)
            chunk_texts.append(text)
            if len(chunk_hashes) >= _EMBED_CHUNK_SIZE and not _flush_chunk():
                return {}

        if not _flush_chunk():
            return {}

        return embeddings

    # ------------------------------------------------------------------
    # Edge grouping
    # ------------------------------------------------------------------

    @staticmethod
    def _group_edges_by_src_file(
        edges: list[ResolvedEdge],
        symbol_to_file: dict[str, str],
    ) -> dict[str, list[ResolvedEdge]]:
        """Group *edges* by the file path of their ``src_id``.

        Edges whose ``src_id`` is unknown (resolver produced an edge for a
        symbol that isn't in our file set) are dropped — without an owner
        file payload we'd never write them, and they'd lose the cascade-on-
        re-parse property anyway.
        """
        by_file: dict[str, list[ResolvedEdge]] = {}
        for edge in edges:
            src_file = symbol_to_file.get(edge.src_id)
            if src_file is None:
                continue
            by_file.setdefault(src_file, []).append(edge)
        return by_file

    # ------------------------------------------------------------------
    # Public entry points
    # ------------------------------------------------------------------

    def index_file(self, abs_path: Path, repo_root: Path) -> IndexerStats:
        """Parse + enrich + embed + write a single file. No edge resolution.

        Cross-file edges require the resolver to see every symbol in scope at
        the same time; this method only sees the one file, so the writer is
        called with an empty edges list.

        Errors are captured in :attr:`IndexerStats.errors` rather than raised.
        """
        stats = IndexerStats()
        start = time.monotonic()

        try:
            fr = self._parse_and_enrich(abs_path, repo_root)
        except Exception as exc:
            stats.errors.append(f"{abs_path}: {exc.__class__.__name__}: {exc}")
            stats.elapsed_s = time.monotonic() - start
            return stats

        embeddings = self._embed_results([fr], skip_embeddings=False)
        self._write_one(fr, edges=[], embeddings=embeddings)

        stats.files_processed = 1
        stats.symbols_indexed = len(fr.enriched)
        stats.secrets_redacted = sum(
            1 for e in fr.enriched if "secret_redacted" in e.untrusted_flags
        )
        stats.elapsed_s = time.monotonic() - start
        return stats

    def index_repo(
        self,
        repo_root: Path,
        *,
        full: bool = False,
        skip_embeddings: bool = False,
        embed_progress: Callable[[int, int], None] | None = None,
    ) -> IndexerStats:
        """Cold or incremental walk of *repo_root*.

        Args:
            repo_root: Repository root path. Will be resolved to absolute.
            full: When True, every supported file is re-parsed even if its
                ``content_hash`` matches the existing DB row. When False
                (default), unchanged files are skipped (REQ-IDX-2).
            skip_embeddings: When True, do not load the embedder or compute
                vectors. Lexical + structural retrieval remain functional.

        Returns:
            :class:`IndexerStats` with counters and any non-fatal errors.
        """
        repo_root = repo_root.resolve()
        stats = IndexerStats()
        start = time.monotonic()
        # Per-phase wall time, so the dominant cost in a cold index (the
        # fresh-user wait) is measurable instead of a black box.
        phase_s: dict[str, float] = {}

        # Pass 1: parse + enrich every file we plan to write. We collect
        # results in memory because the resolver needs them all at once.
        results: list[_FileResult] = []
        skipped_paths: list[str] = []

        _t = time.monotonic()
        for abs_path in self._walk_repo(repo_root):
            try:
                rel = abs_path.relative_to(repo_root).as_posix()
            except ValueError:
                continue

            try:
                file_bytes, content_hash = self._read_and_hash(abs_path)
            except OSError as exc:
                stats.errors.append(f"{rel}: {exc.__class__.__name__}: {exc}")
                continue

            # Idempotency check (REQ-IDX-2).
            if not full:
                existing = get_file(self.db, rel)
                if existing is not None and existing.content_hash == content_hash:
                    stats.files_skipped += 1
                    skipped_paths.append(rel)
                    continue

            try:
                fr = self._parse_and_enrich_from_bytes(
                    abs_path=abs_path,
                    rel=rel,
                    file_bytes=file_bytes,
                    content_hash=content_hash,
                )
            except Exception as exc:
                stats.errors.append(f"{rel}: {exc.__class__.__name__}: {exc}")
                continue

            results.append(fr)
        phase_s["parse_enrich"] = time.monotonic() - _t

        # Pass 2: cross-file edge resolution over the union of newly-parsed
        # symbols and surviving DB symbols (covers the case where a changed
        # file calls into an unchanged file).
        _t = time.monotonic()
        all_symbols = self._collect_resolver_input(results, skipped_paths, repo_root)
        edges = resolve_edges(all_symbols, repo_root=str(repo_root))

        # Group edges by their src_id's file path; we only need entries for
        # files we're writing in this run.
        owned_files = {fr.rel_path for fr in results}
        symbol_to_file = {s.id: s.file_path for s in all_symbols}
        edges_by_file = self._group_edges_by_src_file(edges, symbol_to_file)
        phase_s["resolve_edges"] = time.monotonic() - _t

        # Pass 3: embed everything in one batch.
        _t = time.monotonic()
        embeddings = self._embed_results(
            results, skip_embeddings=skip_embeddings, progress=embed_progress
        )
        phase_s["embed"] = time.monotonic() - _t

        # Pass 4: write each file's payload. The writer's per-file transaction
        # gives us atomic upsert + cascade.
        _t = time.monotonic()
        for fr in results:
            file_edges = edges_by_file.get(fr.rel_path, []) if fr.rel_path in owned_files else []
            self._write_one(fr, edges=file_edges, embeddings=embeddings)
            stats.files_processed += 1
            stats.symbols_indexed += len(fr.enriched)
            stats.edges_resolved += len(file_edges)
            stats.secrets_redacted += sum(
                1 for e in fr.enriched if "secret_redacted" in e.untrusted_flags
            )

        stats.elapsed_s = time.monotonic() - start
        phase_s["write"] = stats.elapsed_s - sum(phase_s.values())
        # Structured per-phase breakdown of a cold/full index: the basis for
        # deciding where to spend latency-reduction effort (the fresh-user wait).
        if results:
            logger.info(
                "indexed %d files / %d symbols / %d edges in %.1fs "
                "(parse_enrich=%.1fs resolve_edges=%.1fs embed=%.1fs write=%.1fs)",
                stats.files_processed,
                stats.symbols_indexed,
                stats.edges_resolved,
                stats.elapsed_s,
                phase_s.get("parse_enrich", 0.0),
                phase_s.get("resolve_edges", 0.0),
                phase_s.get("embed", 0.0),
                max(0.0, phase_s.get("write", 0.0)),
            )

        # A full/cold index re-materializes the whole DB from the current
        # runtime, so stamp ``meta.index_version`` here — the single source of
        # truth for the ``health`` version check. Both entrypoints reach a full
        # index through this method (CLI ``index --full/--clear`` and the
        # ``cognis-indexd`` cold rebuild's two phases), so neither can drift.
        # Previously only the CLI wrote it; a daemon-built index stayed pinned
        # to a stale ``index_version`` after an upgrade, so the version check
        # failed forever. Worse, the extension's auto-manage treats a failing
        # version check as "needs rebuild" and keeps forcing ``--full-rebuild``,
        # so the stale index drove an endless rebuild loop. Incremental walks
        # (``full=False``) intentionally leave the stamp untouched.
        if full:
            from cognis import __version__
            from cognis.db import _write_meta

            with self.db.write() as conn:
                _write_meta(conn, "index_version", __version__)

        return stats

    def index_changed_files(
        self,
        paths: list[Path],
        repo_root: Path,
    ) -> IndexerStats:
        """Re-parse and re-write *paths*, re-resolving cross-file edges.

        The resolver runs over the union of these paths' new symbols plus all
        symbols currently in the DB belonging to *other* files — so a callee
        in an unchanged file is still visible to callers in changed files.
        """
        repo_root = repo_root.resolve()
        stats = IndexerStats()
        start = time.monotonic()

        results: list[_FileResult] = []
        for abs_path in paths:
            try:
                rel = abs_path.resolve().relative_to(repo_root).as_posix()
            except ValueError:
                stats.errors.append(f"{abs_path}: outside repo root {repo_root}")
                continue

            if self._detect_language(abs_path) is None:
                continue

            try:
                fr = self._parse_and_enrich(abs_path.resolve(), repo_root)
            except Exception as exc:
                stats.errors.append(f"{rel}: {exc.__class__.__name__}: {exc}")
                continue

            results.append(fr)

        if not results:
            stats.elapsed_s = time.monotonic() - start
            return stats

        # Build the resolver input set: new symbols from changed files +
        # existing symbols from all *other* files in the DB.
        changed_files = {fr.rel_path for fr in results}
        all_symbols = self._collect_resolver_input(
            results,
            skipped_paths=[],
            repo_root=repo_root,
            exclude_files=changed_files,
        )
        edges = resolve_edges(all_symbols, repo_root=str(repo_root))

        symbol_to_file = {s.id: s.file_path for s in all_symbols}
        edges_by_file = self._group_edges_by_src_file(edges, symbol_to_file)

        embeddings = self._embed_results(results, skip_embeddings=False)

        for fr in results:
            file_edges = edges_by_file.get(fr.rel_path, [])
            self._write_one(fr, edges=file_edges, embeddings=embeddings)
            stats.files_processed += 1
            stats.symbols_indexed += len(fr.enriched)
            stats.edges_resolved += len(file_edges)
            stats.secrets_redacted += sum(
                1 for e in fr.enriched if "secret_redacted" in e.untrusted_flags
            )

        stats.elapsed_s = time.monotonic() - start
        return stats

    def remove_file(self, abs_path: Path, repo_root: Path) -> None:
        """Remove all symbols for *abs_path* via the writer's cascade logic.

        This is the hook for the watcher's ``deleted`` event. Idempotent —
        deleting a file that was never indexed is a no-op.
        """
        try:
            rel = abs_path.resolve().relative_to(repo_root.resolve()).as_posix()
        except ValueError:
            logger.warning("remove_file: %s is outside repo %s", abs_path, repo_root)
            return
        self.writer._delete_file_sync(rel)

    # ------------------------------------------------------------------
    # Helpers used by the public entry points
    # ------------------------------------------------------------------

    def _parse_and_enrich_from_bytes(
        self,
        *,
        abs_path: Path,
        rel: str,
        file_bytes: bytes,
        content_hash: str,
    ) -> _FileResult:
        """Variant of :meth:`_parse_and_enrich` that re-uses bytes already read.

        The walker reads the file once to compute ``content_hash`` for the
        idempotency check; this method skips the second read.
        """
        language = self._detect_language(abs_path)
        if language is None:
            raise ValueError(f"unsupported file extension: {abs_path}")

        parser = self._get_parser(language)
        if parser is None:
            raise ValueError(f"parser for {language!r} is not available")

        try:
            source = file_bytes.decode("utf-8")
        except UnicodeDecodeError:
            return _FileResult(
                rel_path=rel,
                language=language,
                file_size_bytes=len(file_bytes),
                content_hash=content_hash,
                enriched=[],
                parse_status="failed",
            )

        symbols = parser.parse(source, rel)
        enriched = [self.enricher.enrich(s) for s in symbols]

        parse_status = "ok"
        if not enriched and source.strip():
            parse_status = "partial"

        return _FileResult(
            rel_path=rel,
            language=language,
            file_size_bytes=len(file_bytes),
            content_hash=content_hash,
            enriched=enriched,
            parse_status=parse_status,
        )

    def _collect_resolver_input(
        self,
        results: list[_FileResult],
        skipped_paths: list[str],
        repo_root: Path,
        *,
        exclude_files: set[str] | None = None,
    ) -> list[ParsedSymbol]:
        """Return every :class:`ParsedSymbol` the resolver should see.

        Combines:

        - All freshly-parsed symbols from this run.
        - DB-resident symbols from files we did *not* re-parse (so callers in
          a changed file can still resolve to callees in an unchanged file).

        ``exclude_files`` is used by :meth:`index_changed_files` to ensure we
        don't include the *old* DB version of a file we just re-parsed
        (otherwise the resolver sees both old and new symbols for the same
        file and produces duplicate edges).

        Skipped files (idempotency hits) are also pulled from the DB so the
        resolver has the full picture.

        We hydrate :class:`SymbolNode` rows from the DB into a minimal
        :class:`ParsedSymbol` shape — the resolver only reads ``id``,
        ``name``, ``file_path``, ``language``, and ``body_excerpt``, so we
        copy those across. Any DB row missing ``body_excerpt`` contributes
        only as a *target* for resolution, not as a *source*.
        """
        del repo_root  # unused; signature kept for symmetry with future work

        # Symbols freshly parsed in this run.
        new_symbols: list[ParsedSymbol] = []
        new_files: set[str] = set()
        for fr in results:
            new_files.add(fr.rel_path)
            new_symbols.extend(fr.parsed_symbols)

        # Files we want to also include from the DB.
        skip_set = set(skipped_paths)
        excluded = exclude_files or set()

        # Full re-index / cold-index path: the resolver already sees every file
        # we intend to index, so rehydrating the old DB only duplicates memory.
        if not skip_set and not excluded:
            return new_symbols

        # Pull DB symbols that belong to files we did NOT re-parse this run.
        db_symbols: list[ParsedSymbol] = []
        conn = self.db.connect()
        select_sql = (
            "SELECT id, kind, name, qualified_name, language, module, file_path, "
            "line_start, line_end, body_excerpt FROM symbol"
        )

        if skip_set:
            wanted_files = sorted(skip_set - new_files - excluded)
            for start in range(0, len(wanted_files), _SQL_PARAM_CHUNK):
                file_chunk = wanted_files[start : start + _SQL_PARAM_CHUNK]
                if not file_chunk:
                    continue
                placeholders = ", ".join("?" * len(file_chunk))
                rows = conn.execute(
                    f"{select_sql} WHERE file_path IN ({placeholders})",
                    file_chunk,
                )
                for row in rows:
                    db_symbols.append(_resolver_row_to_parsed(row))
            return new_symbols + db_symbols

        if excluded:
            excluded_list = sorted(excluded)
            if len(excluded_list) <= _SQL_PARAM_CHUNK:
                placeholders = ", ".join("?" * len(excluded_list))
                rows = conn.execute(
                    f"{select_sql} WHERE file_path NOT IN ({placeholders})",
                    excluded_list,
                )
            else:
                rows = conn.execute(select_sql)
            for row in rows:
                parsed = _resolver_row_to_parsed(row)
                if parsed.file_path in excluded:
                    continue
                if parsed.file_path in new_files:
                    continue
                db_symbols.append(parsed)
            return new_symbols + db_symbols

        return new_symbols + db_symbols

    def _write_one(
        self,
        fr: _FileResult,
        *,
        edges: list[ResolvedEdge],
        embeddings: dict[str, object],
    ) -> None:
        """Build a :class:`FileWritePayload` for *fr* and call the writer.

        Also keeps ``symbol_fts`` in sync so lexical retrieval always reflects
        the current symbol set.
        """
        # Flatten enricher output into the writer payload's parallel lists.
        symbols = fr.parsed_symbols
        from cognis.models import SymbolAttribute as _SymAttr

        attributes: list[_SymAttr] = []
        for enriched in fr.enriched:
            attributes.extend(enriched.attributes)

        # Filter embeddings to just the content_hashes present in this file —
        # passing the global dict works too (the writer only updates rows that
        # match), but a smaller dict is cheaper in the BLOB executemany call.
        # We forward the global dict directly to avoid an extra dict copy; the
        # writer's _upsert_embeddings handles missing entries gracefully.
        #
        # Cast to the writer's expected `dict[str, np.ndarray]` shape. We've
        # only put ndarrays into it via _embed_results, so the cast is sound.
        # numpy is an optional dep (the ``embed-local`` extra); ``embeddings``
        # can only be non-empty when it is installed, so import it lazily and
        # skip the cast entirely on a lexical+structural-only install.
        emb_dict: dict[str, Any] = {}
        if embeddings:
            from numpy import ndarray  # local import: numpy is an optional dep

            for symbol in symbols:
                vec = embeddings.get(symbol.content_hash)
                if isinstance(vec, ndarray):
                    emb_dict[symbol.content_hash] = vec

        payload = FileWritePayload(
            file_path=fr.rel_path,
            language=fr.language,
            file_size_bytes=fr.file_size_bytes,
            content_hash=fr.content_hash,
            parsed_at=now_epoch(),
            parse_status=fr.parse_status,
            symbols=symbols,
            edges=edges,
            attributes=attributes,
            embeddings=emb_dict,
        )

        # Use the writer's sync core directly: avoids spinning a fresh asyncio
        # loop for each file. The async public method is a thin wrapper over
        # this; there is no functional difference.
        removed_symbol_ids = self.writer._write_file_sync(payload)

        # Keep FTS in sync. ``populate_fts`` upserts rows keyed by ``id`` so
        # this is safe even when symbols change content_hash on re-parse —
        # the old FTS row is overwritten under the same id... wait, the id
        # is content_hash-derived, so an old row with the previous id is no
        # longer addressable. The writer's symbol cascade already deleted it
        # from ``symbol``, but ``symbol_fts`` is contentless and is not a
        # FK target — we'd accumulate orphan FTS rows.
        #
        # Solution: clear FTS rows that match the file path before
        # re-populating. We do this directly on the connection instead of
        # adding a new helper to the retrieval layer.
        symbol_node_view = [_parsed_to_node(s) for s in symbols] if symbols else []
        _refresh_fts_for_file(
            self.db,
            fr.rel_path,
            symbol_node_view,
            removed_symbol_ids=removed_symbol_ids,
        )


# ---------------------------------------------------------------------------
# Helpers — ParsedSymbol/SymbolNode interconversion for the resolver input set
# ---------------------------------------------------------------------------


def _node_to_parsed(node: SymbolNode) -> ParsedSymbol:
    """Project a DB :class:`SymbolNode` back to the :class:`ParsedSymbol` shape.

    Used when pulling existing symbols from the DB into the resolver input
    so cross-file edges can still resolve.
    """
    return ParsedSymbol(
        id=node.id,
        kind=node.kind,
        name=node.name,
        qualified_name=node.qualified_name,
        language=node.language,
        module=node.module,
        file_path=node.file_path,
        line_start=node.line_start,
        line_end=node.line_end,
        signature=node.signature,
        docstring=node.docstring,
        content_hash=node.content_hash,
        body_excerpt=node.body_excerpt,
        untrusted_flags=list(node.untrusted_flags),
    )


def _resolver_row_to_parsed(row: Any) -> ParsedSymbol:
    """Build a minimal ParsedSymbol-shaped view for resolver input hydration."""
    return ParsedSymbol(
        id=str(row["id"]),
        kind=row["kind"],
        name=str(row["name"]),
        qualified_name=str(row["qualified_name"]),
        language=str(row["language"]),
        module=str(row["module"]),
        file_path=str(row["file_path"]),
        line_start=int(row["line_start"]),
        line_end=int(row["line_end"]),
        signature=None,
        docstring=None,
        content_hash="",
        body_excerpt=row["body_excerpt"],
        untrusted_flags=[],
    )


def _parsed_to_node(sym: ParsedSymbol) -> SymbolNode:
    """Minimal :class:`ParsedSymbol` → :class:`SymbolNode` copy for FTS sync."""
    from cognis.models import SymbolNode as _Node

    return _Node(
        id=sym.id,
        kind=sym.kind,
        name=sym.name,
        qualified_name=sym.qualified_name,
        language=sym.language,
        module=sym.module,
        file_path=sym.file_path,
        line_start=sym.line_start,
        line_end=sym.line_end,
        signature=sym.signature,
        docstring=sym.docstring,
        content_hash=sym.content_hash,
        body_excerpt=sym.body_excerpt,
        semantic_summary=None,
        risk_score=0.0,
        ambiguous=False,
        untrusted_flags=list(sym.untrusted_flags),
        updated_at=now_epoch(),
    )


def _refresh_fts_for_file(
    db: Database,
    file_path: str,
    symbols: Iterable[SymbolNode],
    *,
    removed_symbol_ids: set[str] | None = None,
) -> None:
    """Clear and repopulate ``symbol_fts`` rows for *file_path*.

    The FTS table is contentless and has no FK to ``symbol``, so it doesn't
    cascade-delete when a symbol disappears. Use the writer-reported removed
    ids to keep this update proportional to the file being refreshed instead of
    scanning the entire FTS table for global orphans on every file write.
    """
    del file_path  # kept for call-site clarity
    syms = list(symbols)
    delete_ids = sorted(removed_symbol_ids or set())

    if delete_ids:
        with db.write() as conn:
            conn.executemany(
                "DELETE FROM symbol_fts WHERE id = ?",
                [(symbol_id,) for symbol_id in delete_ids],
            )

    # Step 2: upsert new rows for this file. ``populate_fts`` opens its own
    # write transaction, so we exit the previous block before calling it.
    if syms:
        populate_fts(db, syms)
