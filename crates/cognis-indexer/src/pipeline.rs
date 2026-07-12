//! End-to-end pipeline orchestrator + incremental indexing (Task 8.3).
//!
//! Wires the four stages landed in 8.1 / 8.2 into a runnable indexer and is the
//! native callable entry the `cognis-indexd` watcher's `process_batch` seam
//! plugs into:
//!
//! ```text
//! walk → parse (8.1) → enrich/scrub (8.2) → resolve edges (8.2) → write (8.2)
//! ```
//!
//! Rust mirror of `cognis_indexer.pipeline.IndexerPipeline`, scoped to the
//! symbol + edge surface (Requirement 9.2 parity). It exposes the same three
//! execution modes the Python pipeline does, plus a batch front-door for the
//! daemon:
//!
//! * [`IndexerPipeline::index_repo`] — cold/full walk of a repository.
//! * [`IndexerPipeline::index_changed_files`] — re-index a known set of paths,
//!   resolving cross-file edges against the union of those plus the symbols
//!   still resident in the DB (so an edited caller still resolves into an
//!   untouched callee).
//! * [`IndexerPipeline::remove_file`] — drop a deleted file's symbols through
//!   the writer's cascade (inbound edges kept + flagged `meta.dst_missing`).
//! * [`IndexerPipeline::index_batch`] — split one watcher batch into the
//!   changed vs. deleted paths and apply the right operation to each.
//!
//! ## Edge-resolution scope (the count-parity lever)
//!
//! Cross-file edges only exist when both endpoints are visible to the resolver
//! in the *same* call. [`index_repo`](IndexerPipeline::index_repo) collects
//! every parsed symbol across the walk before a single
//! [`resolve_edges`](crate::resolver::resolve_edges) pass, exactly like the
//! Python `index_repo`, so the two engines agree on the global edge set.
//!
//! ## Incremental diff
//!
//! Symbol ids are content-hash-derived, so editing a symbol's body changes its
//! id. On re-index of a file the pipeline upserts the new symbol set and then
//! deletes the file's *stale* ids (old − new) via the writer cascade — the
//! per-file diff the Python writer's `_write_file_sync` performs.
//!
//! ## Embeddings
//!
//! Embedding is orthogonal to symbol/edge parity and is the embedder's surface
//! (Task 6). The pipeline takes an **optional** embedder: when present it
//! reconciles the `symbol_vec` dimension up front (Requirement 2.3); when
//! absent (the daemon's default until a production backend is wired) lexical +
//! structural retrieval still work and semantic search is simply degraded.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use cognis_core::{Config, Result, Symbol};
use cognis_embed::Embedder;
use cognis_store::{Database, SymbolWriter};

use crate::enricher::Enricher;
use crate::parser::{artifact::extract_artifact, parse_source};
use crate::resolver::{resolve_edges, to_edges};
use crate::writer::{FileWritePayload, IndexWriter};

/// Extension → language label table the **walker** uses, mirroring the Python
/// `_LANG_BY_EXT` map (`pipeline.py`). This is intentionally narrower than the
/// parser's [`language_for_path`](crate::parser::language_for_path) (which also
/// accepts `.js`/`.jsx`/`.mjs`/…): the Python indexer only walks these six
/// extensions, so for symbol/edge **count** parity the Rust walker must enumerate
/// the same file set.
const LANG_BY_EXT: &[(&str, &str)] = &[
    ("ts", "typescript"),
    ("tsx", "typescript"),
    ("py", "python"),
    ("go", "go"),
    ("cs", "csharp"),
    ("java", "java"),
    ("rs", "rust"),
    ("c", "c"),
    ("h", "c"),
    ("cc", "cpp"),
    ("cpp", "cpp"),
    ("cxx", "cpp"),
    ("hpp", "cpp"),
    ("hh", "cpp"),
    ("hxx", "cpp"),
    ("rb", "ruby"),
    ("php", "php"),
    ("phtml", "php"),
];

/// Recognized non-code artifact kinds admitted by the second admission path.
///
/// This is a **population-only** feature: no new `SymbolKind`/`EdgeKind` and no
/// DB column. The kind only steers which artifact extractor a file is routed to
/// downstream (Task 2.x); the walker uses it purely as a presence signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactKind {
    Yaml,
    Toml,
    Sql,
    Html,
    Markdown,
}

/// Extension → [`ArtifactKind`] table the **artifact admission path** uses,
/// consulted only when [`detect_language`] misses. Matched case-insensitively
/// via `to_ascii_lowercase`, exactly as [`detect_language`] lowercases the
/// extension. No extension here appears in [`LANG_BY_EXT`], so the two tables
/// are disjoint and at most one path admits any file (Req 1.4).
const ARTIFACT_BY_EXT: &[(&str, ArtifactKind)] = &[
    ("md", ArtifactKind::Markdown),
    ("yaml", ArtifactKind::Yaml),
    ("yml", ArtifactKind::Yaml),
    ("toml", ArtifactKind::Toml),
    ("html", ArtifactKind::Html),
    ("htm", ArtifactKind::Html),
    ("sql", ArtifactKind::Sql),
];

/// Per-run counters (mirror `cognis_indexer.pipeline.IndexerStats`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IndexStats {
    /// Files parsed and written in this run.
    pub files_processed: usize,
    /// Files removed via the delete cascade in this run.
    pub files_removed: usize,
    /// Total symbols persisted across all processed files.
    pub symbols_indexed: usize,
    /// Total resolved edges persisted across all processed files.
    pub edges_resolved: usize,
    /// Symbols whose enricher flagged `secret_redacted`.
    pub secrets_redacted: usize,
    /// Human-readable, non-fatal per-file error strings.
    pub errors: Vec<String>,
}

impl IndexStats {
    fn merge(&mut self, other: IndexStats) {
        self.files_processed += other.files_processed;
        self.files_removed += other.files_removed;
        self.symbols_indexed += other.symbols_indexed;
        self.edges_resolved += other.edges_resolved;
        self.secrets_redacted += other.secrets_redacted;
        self.errors.extend(other.errors);
    }
}

/// One file's parse+enrich result, before cross-file edges are known.
struct FileResult {
    rel_path: String,
    symbols: Vec<Symbol>,
    secrets_redacted: usize,
}

/// End-to-end indexer orchestrator over a [`Database`].
///
/// Construction is cheap (no walk, no connection until the first write). Clone
/// the [`Database`] handle out via [`IndexerPipeline::database`] when a caller
/// needs to read counts back (the connection is shared per-thread).
pub struct IndexerPipeline {
    db: Database,
    config: Config,
    enricher: Enricher,
    embedder: Option<Box<dyn Embedder>>,
}

impl IndexerPipeline {
    /// Build a pipeline over an already-open [`Database`] with no embedder
    /// (lexical + structural only). Used by tests and the parity harness.
    pub fn new(db: Database, config: Config) -> Self {
        IndexerPipeline {
            db,
            config,
            enricher: Enricher::new(),
            embedder: None,
        }
    }

    /// Build a pipeline with an optional embedder. When an embedder is present
    /// the `symbol_vec` dimension is reconciled to it up front (Requirement
    /// 2.3) so a model swap to a new vector size recreates the table.
    pub fn with_embedder(
        db: Database,
        config: Config,
        embedder: Option<Box<dyn Embedder>>,
    ) -> Result<Self> {
        let mut db = db;
        if let Some(emb) = embedder.as_ref() {
            let dim = emb.embedding_dim();
            if dim > 0 {
                db.reconcile_embedding_dim(dim)?;
            }
        }
        Ok(IndexerPipeline {
            db,
            config,
            enricher: Enricher::new(),
            embedder,
        })
    }

    /// Open the database at `db_path` and build a pipeline.
    ///
    /// Convenience for the `cognis-indexd` daemon so it can construct the native
    /// pipeline from a path + config without depending on `cognis-store`
    /// directly. The embedder is built best-effort from `config.embedder`: a
    /// backend that is unavailable in this build (e.g. `onnx-local` without the
    /// feature, or the not-yet-ported `local` backend) degrades to *no
    /// embedder* rather than failing the daemon — lexical/structural indexing
    /// proceeds and semantic search is simply degraded.
    pub fn open(db_path: &Path, config: Config) -> Result<Self> {
        let db = Database::open(db_path)?;
        let embedder = cognis_embed::build_embedder(&config).ok();
        Self::with_embedder(db, config, embedder)
    }

    /// The underlying database handle (clones share the per-thread connection).
    pub fn database(&self) -> &Database {
        &self.db
    }

    // ------------------------------------------------------------------
    // Public entry points
    // ------------------------------------------------------------------

    /// Cold/full walk of `repo_root`: parse every supported file, resolve the
    /// global edge set in one pass, and write per file.
    ///
    /// `full` is accepted for API symmetry with the Python pipeline; this Rust
    /// slice always re-parses the walked set (idempotency-by-content-hash skip
    /// is the `file`-cache surface and is not yet wired here).
    pub fn index_repo(&mut self, repo_root: &Path, full: bool) -> Result<IndexStats> {
        let _ = full;
        let repo_root = canonical(repo_root);
        let mut stats = IndexStats::default();

        // Pass 1: parse + enrich every walked file.
        let mut results: Vec<FileResult> = Vec::new();
        for abs in self.walk_repo(&repo_root) {
            let Some(rel) = relativize(&abs, &repo_root) else {
                continue;
            };
            match self.parse_and_enrich(&abs, &rel) {
                Ok(Some(fr)) => results.push(fr),
                Ok(None) => {}
                Err(e) => stats.errors.push(format!("{rel}: {e}")),
            }
        }

        // Pass 2: one global edge-resolution pass over the union of all symbols.
        let all_symbols: Vec<Symbol> = results.iter().flat_map(|r| r.symbols.clone()).collect();
        let owned: BTreeSet<&str> = results.iter().map(|r| r.rel_path.as_str()).collect();
        let edges_by_file = resolve_grouped(&all_symbols, &owned);

        // Pass 3: write each file's payload under its own transaction.
        for fr in &results {
            let edges = edges_by_file.get(&fr.rel_path).cloned().unwrap_or_default();
            self.write_file_diff(&fr.rel_path, &fr.symbols, edges.clone())?;
            stats.files_processed += 1;
            stats.symbols_indexed += fr.symbols.len();
            stats.edges_resolved += edges.len();
            stats.secrets_redacted += fr.secrets_redacted;
        }

        Ok(stats)
    }

    /// Re-index `paths`, resolving cross-file edges against the union of their
    /// new symbols plus the DB symbols of every *other* file.
    pub fn index_changed_files(
        &mut self,
        paths: &[PathBuf],
        repo_root: &Path,
    ) -> Result<IndexStats> {
        let repo_root = canonical(repo_root);
        let mut stats = IndexStats::default();

        // Parse + enrich the changed files (filter to supported source files).
        let mut results: Vec<FileResult> = Vec::new();
        for path in paths {
            let abs = absolutize(path, &repo_root);
            let Some(rel) = relativize(&abs, &repo_root) else {
                continue;
            };
            // Admit the same two disjoint sets the cold walk admits (design
            // §"Data flow"): Code_Files (unchanged) plus, when the artifact gate
            // is open, Artifact_Files — so incremental re-index routes artifact
            // symbols through the identical parse→enrich→write path as a full
            // index (Req 10.1). Code admission is untouched: a Code_File is still
            // admitted whenever `detect_language` hits, regardless of the
            // artifact gate.
            let is_code = detect_language(&abs, &self.config).is_some();
            let is_artifact =
                artifacts_enabled(&self.config) && detect_artifact(&abs, &self.config).is_some();
            if !is_code && !is_artifact {
                continue;
            }
            match self.parse_and_enrich(&abs, &rel) {
                Ok(Some(fr)) => results.push(fr),
                Ok(None) => {}
                Err(e) => stats.errors.push(format!("{rel}: {e}")),
            }
        }
        if results.is_empty() {
            return Ok(stats);
        }

        // Resolver input: new changed-file symbols + DB symbols of other files.
        let changed: BTreeSet<String> = results.iter().map(|r| r.rel_path.clone()).collect();
        let mut all_symbols: Vec<Symbol> = results.iter().flat_map(|r| r.symbols.clone()).collect();
        for sym in self.db.list_symbols()? {
            if !changed.contains(&sym.file_path) {
                all_symbols.push(sym);
            }
        }

        // Only write edges whose src belongs to a changed file.
        let owned: BTreeSet<&str> = changed.iter().map(String::as_str).collect();
        let edges_by_file = resolve_grouped(&all_symbols, &owned);

        for fr in &results {
            let edges = edges_by_file.get(&fr.rel_path).cloned().unwrap_or_default();
            self.write_file_diff(&fr.rel_path, &fr.symbols, edges.clone())?;
            stats.files_processed += 1;
            stats.symbols_indexed += fr.symbols.len();
            stats.edges_resolved += edges.len();
            stats.secrets_redacted += fr.secrets_redacted;
        }

        Ok(stats)
    }

    /// Remove every symbol belonging to `abs_path` through the writer cascade.
    /// Idempotent: removing a file that was never indexed is a no-op.
    pub fn remove_file(&mut self, abs_path: &Path, repo_root: &Path) -> Result<IndexStats> {
        let repo_root = canonical(repo_root);
        let abs = absolutize(abs_path, &repo_root);
        let mut stats = IndexStats::default();
        let Some(rel) = relativize(&abs, &repo_root) else {
            return Ok(stats);
        };

        let ids: Vec<String> = self
            .db
            .list_symbols()?
            .into_iter()
            .filter(|s| s.file_path == rel)
            .map(|s| s.id)
            .collect();
        if ids.is_empty() {
            return Ok(stats);
        }

        let mut writer = IndexWriter::new(self.db.clone());
        writer.delete_symbols(&ids)?;
        stats.files_removed = 1;
        Ok(stats)
    }

    /// Apply one debounced watcher batch: existing source paths are re-indexed
    /// together (one cross-file resolution pass), missing paths are removed.
    pub fn index_batch(&mut self, repo_root: &Path, paths: &[PathBuf]) -> Result<IndexStats> {
        let mut changed: Vec<PathBuf> = Vec::new();
        let mut removed: Vec<PathBuf> = Vec::new();
        for p in paths {
            if p.exists() {
                changed.push(p.clone());
            } else {
                removed.push(p.clone());
            }
        }

        let mut stats = IndexStats::default();
        if !changed.is_empty() {
            stats.merge(self.index_changed_files(&changed, repo_root)?);
        }
        for path in &removed {
            stats.merge(self.remove_file(path, repo_root)?);
        }
        Ok(stats)
    }

    // ------------------------------------------------------------------
    // Internals
    // ------------------------------------------------------------------

    /// Parse + enrich one file. Returns `Ok(None)` when the file produced no
    /// symbols (empty file), `Err` on read/decode failure.
    ///
    /// After decoding UTF-8, the file is routed to one of two stages, mirroring
    /// the walker's fixed-order two-path admission (design §"Data flow"): a
    /// Code_File (`detect_language` hits) goes to [`parse_source`]; an
    /// Artifact_File (`detect_artifact` hits, artifact gate open) goes to
    /// [`extract_artifact`] with its resolved [`ArtifactKind`]. Both return the
    /// same [`ParseOutput`] shape, so enrich/embed/write below stay identical
    /// regardless of file type (Req 10.1). The existing non-UTF-8 skip
    /// (`String::from_utf8` → `Ok(None)`) covers both paths (Req 1.7).
    fn parse_and_enrich(&self, abs: &Path, rel: &str) -> Result<Option<FileResult>> {
        let bytes = std::fs::read(abs)
            .map_err(|e| cognis_core::CognisError::Store(format!("read {}: {e}", abs.display())))?;
        // Non-UTF-8 files are treated as having no indexable symbols (the Python
        // pipeline records a `failed` parse_status for them); we skip rather
        // than abort the batch (Requirement 1.7 / legacy 9.4). This single skip
        // guards both the code and artifact routes below.
        let Ok(source) = String::from_utf8(bytes) else {
            return Ok(None);
        };

        // Dispatch on the same predicates the walker admits with, in the same
        // fixed order (code first, then artifact) so the routing is exclusive
        // and code files are always parsed exactly as before. A file that is
        // neither (only reachable if a caller hands `parse_and_enrich` an
        // unadmitted path directly) degrades to the textual `parse_source`
        // fallback, preserving pre-feature behavior.
        let parsed = if detect_language(abs, &self.config).is_some() {
            parse_source(rel, &source)
        } else if let Some(kind) = detect_artifact(abs, &self.config) {
            extract_artifact(kind, rel, &source)
        } else {
            parse_source(rel, &source)
        };
        if parsed.symbols.is_empty() {
            return Ok(None);
        }

        let mut symbols = Vec::with_capacity(parsed.symbols.len());
        let mut secrets_redacted = 0usize;
        for sym in &parsed.symbols {
            let enriched = self.enricher.enrich(sym);
            if enriched
                .untrusted_flags
                .iter()
                .any(|f| f == "secret_redacted")
            {
                secrets_redacted += 1;
            }
            symbols.push(enriched.symbol);
        }

        Ok(Some(FileResult {
            rel_path: rel.to_string(),
            symbols,
            secrets_redacted,
        }))
    }

    /// Write one file's symbols + edges, then delete the file's stale symbol
    /// ids (old − new) so a removed/edited symbol does not linger.
    fn write_file_diff(
        &mut self,
        rel_path: &str,
        symbols: &[Symbol],
        edges: Vec<cognis_core::Edge>,
    ) -> Result<()> {
        // Stale ids = ids previously persisted for this file that are not in the
        // new symbol set (ids are content-hash-derived, so an edit produces a
        // new id and orphans the old one).
        let new_ids: BTreeSet<&str> = symbols.iter().map(|s| s.id.as_str()).collect();
        let stale: Vec<String> = self
            .db
            .list_symbols()?
            .into_iter()
            .filter(|s| s.file_path == rel_path && !new_ids.contains(s.id.as_str()))
            .map(|s| s.id)
            .collect();

        let mut writer = IndexWriter::new(self.db.clone());
        writer.write_file(&FileWritePayload::new(symbols.to_vec(), edges))?;
        if !stale.is_empty() {
            writer.delete_symbols(&stale)?;
        }
        // Persist embeddings for the file's symbols when an embedder is wired.
        // Semantic search only returns hits once vectors exist in `symbol_vec`;
        // this is the index-time half of that pipeline (the query-time half is
        // the MCP server embedding the query + `vec_search`). Best-effort: an
        // embedder failure degrades to lexical/structural rather than aborting
        // the (already-committed) symbol/edge write.
        self.embed_and_persist(symbols)?;
        Ok(())
    }

    /// Embed each symbol's text and upsert the vectors into `symbol_vec`.
    ///
    /// No-op when no embedder is configured. The embedding input mirrors the
    /// lexical/semantic surface — qualified name + signature + docstring + body
    /// excerpt — so a natural-language query embeds into the same space as the
    /// indexed symbols. A batch embed failure is swallowed (semantic degrades
    /// to empty) so indexing never fails on the orthogonal embedding step.
    fn embed_and_persist(&mut self, symbols: &[Symbol]) -> Result<()> {
        let Some(embedder) = self.embedder.as_ref() else {
            return Ok(());
        };
        if symbols.is_empty() {
            return Ok(());
        }
        let texts: Vec<String> = symbols.iter().map(embedding_text).collect();
        let vectors = match embedder.embed_batch(&texts) {
            Ok(v) if v.len() == symbols.len() => v,
            // Length mismatch or backend error: skip embeddings this pass, keep
            // the lexical/structural index intact (graceful degradation).
            _ => return Ok(()),
        };
        let rows: Vec<(String, Vec<f32>)> =
            symbols.iter().map(|s| s.id.clone()).zip(vectors).collect();
        self.db.upsert_embeddings(&rows)
    }

    /// Walk `repo_root` yielding absolute paths of indexable source files,
    /// honouring `config.repo.ignore` (+ always `.git` / `.cognis`). Mirrors the
    /// Python walker's directory pruning; `.gitignore` patterns are not
    /// replicated (covered by the documented parity tolerance).
    pub fn walk_repo(&self, repo_root: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let mut ignored: BTreeSet<String> = self.config.repo.ignore.iter().cloned().collect();
        ignored.insert(cognis_core::config::CONFIG_DIR_NAME.to_string());
        ignored.insert(".git".to_string());
        walk_dir(repo_root, &ignored, &self.config, &mut out);
        out.sort();
        out
    }
}

/// Recursive directory walk with in-order traversal and ignore-dir pruning.
fn walk_dir(dir: &Path, ignored: &BTreeSet<String>, config: &Config, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_dir() {
            if ignored.contains(&name) {
                continue;
            }
            walk_dir(&path, ignored, config, out);
        } else if ft.is_file() {
            // Two disjoint admission paths, resolved in a fixed order so at most
            // one admits any file (Req 1.4): the Code_File path first, then the
            // artifact path only when a code language misses. When artifacts are
            // disabled this degenerates to the pre-feature Code_File-only
            // admission (Req 1.8), leaving the Code_File set byte-identical
            // whether or not artifacts are enabled (Req 1.5). Short-circuit `||`
            // preserves that ordering; the two arms admit identically so a
            // single push covers both.
            let admitted = detect_language(&path, config).is_some()
                || (artifacts_enabled(config) && detect_artifact(&path, config).is_some());
            if admitted {
                out.push(path);
            }
        }
    }
}

/// Test/introspection accessor (Property 1, non-code-artifact-coverage): the
/// repo-relative, forward-slash paths the walker admits for indexing under
/// `config`, produced by the **real** [`walk_dir`] admission logic (both
/// disjoint arms) and the same `relativize` the pipeline uses — but without the
/// parse/enrich/write stages or a database.
///
/// Exposed so the admission-exclusivity property test can observe routing
/// directly (which files are admitted, and — by extension — through which
/// path) instead of inferring it from persisted symbols, which the still-in-
/// progress artifact extractors would confound. Only the ignore-set assembly is
/// repeated from [`IndexerPipeline::walk_repo`]; the admission decision itself
/// ([`detect_language`] / [`artifacts_enabled`] / [`detect_artifact`]) is fully
/// reused, so this never duplicates the routing logic under test.
#[doc(hidden)]
pub fn admitted_rel_paths(repo_root: &Path, config: &Config) -> Vec<String> {
    let root = canonical(repo_root);
    let mut ignored: BTreeSet<String> = config.repo.ignore.iter().cloned().collect();
    ignored.insert(cognis_core::config::CONFIG_DIR_NAME.to_string());
    ignored.insert(".git".to_string());
    let mut out = Vec::new();
    walk_dir(&root, &ignored, config, &mut out);
    out.into_iter()
        .filter_map(|abs| relativize(&abs, &root))
        .collect()
}

/// Resolve the language label for a path via [`LANG_BY_EXT`], gated by
/// `config.languages.enabled`. `None` for unsupported / disabled languages.
fn detect_language(path: &Path, config: &Config) -> Option<&'static str> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())?
        .to_ascii_lowercase();
    let label = LANG_BY_EXT
        .iter()
        .find(|(e, _)| *e == ext)
        .map(|(_, label)| *label)?;
    if config.languages.enabled.iter().any(|l| l == label) {
        Some(label)
    } else {
        None
    }
}

/// Whether the artifact admission gate is open, resolved **independently** of
/// `config.languages.enabled` (Req 1.6): toggling a code language never changes
/// this, and toggling this never changes [`detect_language`].
fn artifacts_enabled(config: &Config) -> bool {
    config.artifact.enabled
}

/// Resolve the [`ArtifactKind`] for a path, or `None`.
///
/// A file is an artifact when either (a) its extension (matched
/// case-insensitively via `to_ascii_lowercase`, mirroring [`detect_language`])
/// is in [`ARTIFACT_BY_EXT`] (Req 1.1), or (b) its file name matches a
/// configured deploy/CI descriptor pattern (Req 1.2). Descriptor-only matches
/// (a name with no recognized artifact extension, e.g. `Dockerfile`) are
/// classified [`ArtifactKind::Yaml`], the common deploy/CI descriptor shape.
///
/// This never consults `config.languages` and never overlaps [`LANG_BY_EXT`],
/// so the artifact path and the Code_File path stay mutually exclusive (Req
/// 1.4). The caller gates the whole path behind [`artifacts_enabled`].
fn detect_artifact(path: &Path, config: &Config) -> Option<ArtifactKind> {
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        let ext = ext.to_ascii_lowercase();
        if let Some((_, kind)) = ARTIFACT_BY_EXT.iter().find(|(e, _)| *e == ext) {
            return Some(*kind);
        }
    }

    // Deploy/CI descriptor name match (Req 1.2): case-insensitive substring
    // match against each configured pattern, so a pattern like `dockerfile`
    // admits `Dockerfile` / `service.dockerfile`.
    let name = path
        .file_name()
        .and_then(|n| n.to_str())?
        .to_ascii_lowercase();
    if config
        .artifact
        .ci_descriptors
        .iter()
        .any(|pat| !pat.is_empty() && name.contains(&pat.to_ascii_lowercase()))
    {
        return Some(ArtifactKind::Yaml);
    }

    None
}

/// Run the global resolver over `symbols` and group the resulting edges by the
/// repo-relative file path of each edge's `src_id`, keeping only sources that
/// belong to an `owned` (being-written) file.
fn resolve_grouped(
    symbols: &[Symbol],
    owned: &BTreeSet<&str>,
) -> BTreeMap<String, Vec<cognis_core::Edge>> {
    let id_to_file: HashMap<&str, &str> = symbols
        .iter()
        .map(|s| (s.id.as_str(), s.file_path.as_str()))
        .collect();

    let resolved = resolve_edges(symbols);
    let edges = to_edges(&resolved);

    let mut by_file: BTreeMap<String, Vec<cognis_core::Edge>> = BTreeMap::new();
    for edge in edges {
        if let Some(file) = id_to_file.get(edge.src_id.as_str()) {
            if owned.contains(file) {
                by_file.entry((*file).to_string()).or_default().push(edge);
            }
        }
    }
    by_file
}

/// Canonicalize a path, falling back to the input when canonicalization fails
/// (e.g. the path does not exist yet on a delete event).
fn canonical(p: &Path) -> PathBuf {
    p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
}

/// The text embedded for a symbol — qualified name + signature + docstring +
/// body excerpt, joined by newlines (empty parts dropped). Mirrors the fields
/// the lexical FTS row carries, so a natural-language query lands in the same
/// space as the indexed symbols.
fn embedding_text(s: &Symbol) -> String {
    let mut parts: Vec<&str> = vec![s.qualified_name.as_str()];
    if let Some(sig) = s.signature.as_deref() {
        parts.push(sig);
    }
    if let Some(doc) = s.docstring.as_deref() {
        parts.push(doc);
    }
    if let Some(body) = s.body_excerpt.as_deref() {
        parts.push(body);
    }
    parts
        .into_iter()
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Make `path` absolute against the (already-canonical) `repo_root`: absolute
/// inputs are taken as-is, relative inputs are joined under the root. Unlike
/// [`canonical`], this never touches the filesystem, so it works for deleted
/// files (a delete event carries a path that no longer exists).
fn absolutize(path: &Path, repo_root: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        repo_root.join(path)
    }
}

/// Strip the Windows `\\?\` verbatim prefix so a canonicalized `repo_root`
/// (which carries the prefix) and a raw event path (which may not) compare
/// consistently. A no-op on non-verbatim paths.
fn strip_verbatim(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        Some(rest) => PathBuf::from(rest),
        None => p.to_path_buf(),
    }
}

/// Repo-relative, forward-slash path for `abs` under `repo_root`, tolerant of a
/// verbatim-prefix mismatch between the two. `None` when `abs` is not under the
/// root.
///
/// Callers always pass an already-`canonical` `repo_root` (symlinks resolved,
/// e.g. macOS `/var -> /private/var`; Windows verbatim / 8.3 short names). A
/// cold walk yields paths that already sit under that canonical root, so the
/// textual fast path hits. But an incremental / watcher event can carry the
/// *un*-resolved form of the same path, so a pure textual strip would miss and
/// silently drop the file (stale symbols never replaced, deletes never
/// applied). When the fast path misses we resolve against the filesystem to
/// match: canonicalize the path directly if it still exists, or canonicalize
/// its (still-present) parent dir and re-attach the file name for a delete
/// event whose path no longer exists.
fn relativize(abs: &Path, repo_root: &Path) -> Option<String> {
    fn strip(abs: &Path, repo_root: &Path) -> Option<String> {
        let a = strip_verbatim(abs);
        let r = strip_verbatim(repo_root);
        a.strip_prefix(&r)
            .ok()
            .map(|rel| rel.to_string_lossy().replace('\\', "/"))
    }

    if let Some(rel) = strip(abs, repo_root) {
        return Some(rel);
    }
    if let Ok(canon) = abs.canonicalize() {
        if let Some(rel) = strip(&canon, repo_root) {
            return Some(rel);
        }
    }
    if let (Some(parent), Some(name)) = (abs.parent(), abs.file_name()) {
        if let Ok(canon_parent) = parent.canonicalize() {
            return strip(&canon_parent.join(name), repo_root);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use cognis_store::SymbolStore;

    /// A tiny deterministic embedder for the wiring tests: a bag-of-letters
    /// vector (26-d, one bucket per ascii letter, L2-normalised). No model, no
    /// I/O, offline — distinct-enough that cosine ranking is meaningful, so the
    /// index→persist→vec_search seam can be asserted without the ONNX backend.
    #[derive(Debug)]
    struct BagOfLettersEmbedder;

    impl Embedder for BagOfLettersEmbedder {
        fn embedding_dim(&self) -> usize {
            26
        }
        fn embed_text(&self, text: &str) -> Result<Vec<f32>> {
            let mut v = vec![0.0f32; 26];
            for c in text.to_ascii_lowercase().chars() {
                if c.is_ascii_lowercase() {
                    v[(c as u8 - b'a') as usize] += 1.0;
                }
            }
            let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
            if norm > 0.0 {
                for x in &mut v {
                    *x /= norm;
                }
            }
            Ok(v)
        }
        fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            texts.iter().map(|t| self.embed_text(t)).collect()
        }
    }

    fn mem_db() -> Database {
        Database::open(":memory:").expect("open in-memory db")
    }

    fn unique_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cognis-indexer-pipeline-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn index_repo_persists_symbols_and_edges() {
        let repo = unique_dir("cold");
        std::fs::write(
            repo.join("a.py"),
            "def helper():\n    return 1\n\ndef caller():\n    return helper()\n",
        )
        .unwrap();
        std::fs::write(repo.join("b.py"), "def standalone():\n    return 2\n").unwrap();

        let db = mem_db();
        let mut pipe = IndexerPipeline::new(db.clone(), Config::default());
        let stats = pipe.index_repo(&repo, true).unwrap();

        assert_eq!(stats.files_processed, 2);
        assert!(stats.symbols_indexed >= 3);
        assert!(stats.edges_resolved >= 1, "expected caller->helper edge");
        assert_eq!(db.count("symbol").unwrap() as usize, stats.symbols_indexed);
        assert_eq!(db.count("edge").unwrap() as usize, stats.edges_resolved);

        std::fs::remove_dir_all(&repo).ok();
    }

    #[test]
    fn incremental_reindex_replaces_stale_symbols() {
        let repo = unique_dir("incr");
        let file = repo.join("m.py");
        std::fs::write(&file, "def alpha():\n    return 1\n").unwrap();

        let db = mem_db();
        let mut pipe = IndexerPipeline::new(db.clone(), Config::default());
        pipe.index_repo(&repo, true).unwrap();
        let before: Vec<String> = db
            .list_symbols()
            .unwrap()
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert!(before.contains(&"alpha".to_string()));

        // Edit the file: alpha -> beta. Re-index just this file.
        std::fs::write(&file, "def beta():\n    return 2\n").unwrap();
        pipe.index_changed_files(std::slice::from_ref(&file), &repo)
            .unwrap();

        let after: Vec<String> = db
            .list_symbols()
            .unwrap()
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert!(after.contains(&"beta".to_string()), "new symbol present");
        assert!(
            !after.contains(&"alpha".to_string()),
            "stale symbol removed"
        );

        std::fs::remove_dir_all(&repo).ok();
    }

    #[test]
    fn remove_file_drops_symbols_and_flags_inbound_edges() {
        let repo = unique_dir("rm");
        std::fs::write(repo.join("dep.py"), "def target():\n    return 1\n").unwrap();
        std::fs::write(repo.join("use.py"), "def user():\n    return target()\n").unwrap();

        let db = mem_db();
        let mut pipe = IndexerPipeline::new(db.clone(), Config::default());
        pipe.index_repo(&repo, true).unwrap();

        // Remove dep.py: its symbols go, inbound edges stay flagged dst_missing.
        let stats = pipe.remove_file(&repo.join("dep.py"), &repo).unwrap();
        assert_eq!(stats.files_removed, 1);

        let remaining_files: BTreeSet<String> = db
            .list_symbols()
            .unwrap()
            .into_iter()
            .map(|s| s.file_path)
            .collect();
        assert!(!remaining_files.iter().any(|f| f.ends_with("dep.py")));
        // Any edge that still points into a dep.py symbol is flagged dst_missing.
        for edge in db.list_edges().unwrap() {
            let dst_in_dep = !db
                .list_symbols()
                .unwrap()
                .iter()
                .any(|s| s.id == edge.dst_id);
            if dst_in_dep {
                assert!(edge.dst_missing());
            }
        }

        std::fs::remove_dir_all(&repo).ok();
    }

    #[test]
    fn index_batch_splits_changed_and_deleted() {
        let repo = unique_dir("batch");
        let keep = repo.join("keep.py");
        let gone = repo.join("gone.py");
        std::fs::write(&keep, "def k():\n    return 1\n").unwrap();
        std::fs::write(&gone, "def g():\n    return 2\n").unwrap();

        let db = mem_db();
        let mut pipe = IndexerPipeline::new(db.clone(), Config::default());
        pipe.index_repo(&repo, true).unwrap();
        assert_eq!(db.count("symbol").unwrap(), 2);

        // Delete gone.py on disk; edit keep.py. One batch carries both.
        std::fs::remove_file(&gone).unwrap();
        std::fs::write(&keep, "def k():\n    return 11\n").unwrap();
        let stats = pipe
            .index_batch(&repo, &[keep.clone(), gone.clone()])
            .unwrap();
        assert_eq!(stats.files_processed, 1);
        assert_eq!(stats.files_removed, 1);

        let names: BTreeSet<String> = db
            .list_symbols()
            .unwrap()
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert!(names.contains("k"));
        assert!(!names.contains("g"));

        std::fs::remove_dir_all(&repo).ok();
    }

    #[test]
    fn indexing_with_embedder_populates_symbol_vec_and_is_searchable() {
        let repo = unique_dir("embed");
        std::fs::write(
            repo.join("auth.py"),
            "def authenticate(token):\n    \"\"\"verify the password then start a session\"\"\"\n    return verify(token)\n",
        )
        .unwrap();
        std::fs::write(
            repo.join("math_utils.py"),
            "def add_numbers(a, b):\n    \"\"\"sum two integers\"\"\"\n    return a + b\n",
        )
        .unwrap();

        let db = mem_db();
        let mut pipe = IndexerPipeline::with_embedder(
            db.clone(),
            Config::default(),
            Some(Box::new(BagOfLettersEmbedder)),
        )
        .unwrap();
        let stats = pipe.index_repo(&repo, true).unwrap();
        assert!(stats.symbols_indexed >= 2);

        // Index-time half: every indexed symbol got a persisted vector.
        assert_eq!(
            db.vec_row_count().unwrap(),
            stats.symbols_indexed,
            "every symbol should have an embedding row"
        );

        // Query-time half: embed a query and confirm vec_search ranks the
        // semantically-closest symbol first (same embedder as index time).
        let emb = BagOfLettersEmbedder;
        let q = emb.embed_text("authenticate token session").unwrap();
        let hits = db.vec_search(&q, 5).unwrap();
        assert!(!hits.is_empty(), "semantic search returned nothing");
        assert!(
            hits[0].symbol_id.contains("authenticate"),
            "closest hit should be authenticate, got {:?}",
            hits.iter().map(|h| &h.symbol_id).collect::<Vec<_>>()
        );

        std::fs::remove_dir_all(&repo).ok();
    }

    #[test]
    fn no_embedder_leaves_symbol_vec_empty() {
        let repo = unique_dir("noembed");
        std::fs::write(repo.join("a.py"), "def alpha():\n    return 1\n").unwrap();
        let db = mem_db();
        let mut pipe = IndexerPipeline::new(db.clone(), Config::default());
        pipe.index_repo(&repo, true).unwrap();
        assert_eq!(db.vec_row_count().unwrap(), 0, "no embedder → no vectors");
        std::fs::remove_dir_all(&repo).ok();
    }

    #[test]
    fn walker_skips_ignored_dirs_and_unsupported_exts() {
        let repo = unique_dir("walk");
        std::fs::create_dir_all(repo.join("node_modules")).unwrap();
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::write(repo.join("node_modules").join("x.py"), "def x(): pass\n").unwrap();
        std::fs::write(repo.join(".git").join("y.py"), "def y(): pass\n").unwrap();
        std::fs::write(repo.join("keep.py"), "def keep(): pass\n").unwrap();
        // `.png` is neither a code language nor an artifact extension: always skipped.
        std::fs::write(repo.join("logo.png"), "not text\n").unwrap();
        // `readme.md` is admitted through the artifact path (enabled by default).
        std::fs::write(repo.join("readme.md"), "# not code\n").unwrap();

        let pipe = IndexerPipeline::new(mem_db(), Config::default());
        let walked = pipe.walk_repo(&canonical(&repo));
        let names: Vec<String> = walked
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        // Sorted by `walk_repo`: the code file plus the artifact, ignored dirs
        // pruned and the truly-unsupported `.png` dropped.
        assert_eq!(names, vec!["keep.py".to_string(), "readme.md".to_string()]);

        std::fs::remove_dir_all(&repo).ok();
    }

    #[test]
    fn artifact_admission_disabled_degenerates_to_code_only() {
        let repo = unique_dir("walk-artifacts-off");
        std::fs::write(repo.join("keep.py"), "def keep(): pass\n").unwrap();
        std::fs::write(repo.join("readme.md"), "# not code\n").unwrap();
        std::fs::write(repo.join("conf.yaml"), "a: 1\n").unwrap();

        let mut config = Config::default();
        config.artifact.enabled = false;
        let pipe = IndexerPipeline::new(mem_db(), config);
        let walked = pipe.walk_repo(&canonical(&repo));
        let names: Vec<String> = walked
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        // Artifacts disabled: pre-feature admission, exactly the Code_File set (Req 1.8).
        assert_eq!(names, vec!["keep.py".to_string()]);

        std::fs::remove_dir_all(&repo).ok();
    }

    #[test]
    fn detect_artifact_matches_extensions_case_insensitively() {
        let config = Config::default();
        assert_eq!(
            detect_artifact(Path::new("Notes.MD"), &config),
            Some(ArtifactKind::Markdown)
        );
        assert_eq!(
            detect_artifact(Path::new("app.YAML"), &config),
            Some(ArtifactKind::Yaml)
        );
        assert_eq!(
            detect_artifact(Path::new("app.yml"), &config),
            Some(ArtifactKind::Yaml)
        );
        assert_eq!(
            detect_artifact(Path::new("Cargo.toml"), &config),
            Some(ArtifactKind::Toml)
        );
        assert_eq!(
            detect_artifact(Path::new("index.HTM"), &config),
            Some(ArtifactKind::Html)
        );
        assert_eq!(
            detect_artifact(Path::new("page.html"), &config),
            Some(ArtifactKind::Html)
        );
        assert_eq!(
            detect_artifact(Path::new("schema.sql"), &config),
            Some(ArtifactKind::Sql)
        );
        // A code extension is never an artifact (disjoint tables, Req 1.4).
        assert_eq!(detect_artifact(Path::new("main.py"), &config), None);
        assert_eq!(detect_artifact(Path::new("logo.png"), &config), None);
    }

    #[test]
    fn detect_artifact_matches_ci_descriptor_names() {
        let mut config = Config::default();
        config.artifact.ci_descriptors = vec!["Dockerfile".to_string()];
        // Descriptor-only name (no artifact extension) → classified Yaml (Req 1.2).
        assert_eq!(
            detect_artifact(Path::new("Dockerfile"), &config),
            Some(ArtifactKind::Yaml)
        );
        assert_eq!(
            detect_artifact(Path::new("service.dockerfile"), &config),
            Some(ArtifactKind::Yaml)
        );
        // No descriptor configured → plain unknown name is not an artifact.
        let bare = Config::default();
        assert_eq!(detect_artifact(Path::new("Dockerfile"), &bare), None);
    }
}
