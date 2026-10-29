//! File-cache-driven planning for one atomic index publication.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use cognis_core::{CognisError, Config, Edge, EdgeKind, ParseStatus, Result, Symbol, SymbolKind};
use cognis_store::{Database, Direction, FileCacheRow, PublishBatch, PublishReason, QueuedSymbol};
use regex::Regex;
use sha2::{Digest, Sha256};

use crate::enricher::Enricher;
use crate::parser::{artifact::extract_artifact, parse_source};
use crate::pipeline::{
    absolutize, admitted_rel_paths, canonical, detect_artifact, detect_language, relativize,
};
use crate::resolver::{normalize_ident, resolve_edges, to_edges};

const REFERENCE_CANDIDATE_CAP: usize = 256;

/// The source-tree view to compare against the durable file cache.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanRequest {
    /// A watcher batch containing source paths that may have changed or disappeared.
    Touched(Vec<PathBuf>),
    /// Diff the complete admitted tree against every cached path.
    Reconcile,
    /// Reparse the complete admitted tree, ignoring cache equality.
    Full,
}

/// Builds one complete [`PublishBatch`] without mutating the database.
pub struct ChangePlanner<'a> {
    db: &'a Database,
    config: &'a Config,
    repo_root: &'a Path,
    enricher: Enricher,
}

impl<'a> ChangePlanner<'a> {
    pub fn new(db: &'a Database, config: &'a Config, repo_root: &'a Path) -> Self {
        Self {
            db,
            config,
            repo_root,
            enricher: Enricher::new(),
        }
    }

    /// Plan exactly one atomic publication from the current tree and cache state.
    pub fn plan(&self, request: PlanRequest) -> Result<PublishBatch> {
        let root = canonical(self.repo_root);
        let reason = match &request {
            PlanRequest::Touched(_) => PublishReason::Incremental,
            PlanRequest::Reconcile => PublishReason::Reconcile,
            PlanRequest::Full => PublishReason::Full,
        };
        let full = matches!(request, PlanRequest::Full);
        let mut cache = BTreeMap::new();
        let mut candidates = BTreeSet::new();
        let mut deleted = BTreeSet::new();
        match request {
            PlanRequest::Touched(paths) => {
                // A watcher edit must not walk the repository or scan File_Cache.
                // Admission and cache reads are both restricted to touched paths.
                for path in paths {
                    let abs = absolutize(&path, &root);
                    let Some(rel) = relativize(&abs, &root) else {
                        continue;
                    };
                    if let Some(row) = self.read_cached_file(&rel)? {
                        cache.insert(rel.clone(), row);
                    }
                    if is_admitted(&abs, &root, self.config) {
                        candidates.insert(rel);
                    } else if cache.contains_key(&rel) {
                        deleted.insert(rel);
                    }
                }
            }
            PlanRequest::Reconcile | PlanRequest::Full => {
                cache = self.read_file_cache()?;
                let admitted: BTreeSet<String> =
                    admitted_rel_paths(&root, self.config).into_iter().collect();
                candidates.extend(admitted.iter().cloned());
                deleted.extend(
                    cache
                        .keys()
                        .filter(|path| !admitted.contains(*path))
                        .cloned(),
                );
            }
        }

        let mut changed = BTreeMap::new();
        for rel in candidates {
            let abs = root.join(Path::new(&rel));
            let bytes = match read_exact_bytes(&abs) {
                Ok(bytes) => bytes,
                Err(_) => {
                    // The path was admitted moments ago but is now unreadable
                    // (rename source, missed delete, or a race). Drop its cache
                    // row with the rest of the batch instead of aborting.
                    if cache.contains_key(&rel) {
                        deleted.insert(rel);
                    }
                    continue;
                }
            };
            let hash = exact_byte_hash(&bytes);
            let old_symbols = self.db.symbols_in_file(&rel)?;
            let legitimately_symbol_less =
                std::str::from_utf8(&bytes).is_ok_and(|source| source.trim().is_empty());
            let symbol_state_ok = !old_symbols.is_empty() || legitimately_symbol_less;
            let unchanged = !full
                && cache.get(&rel).is_some_and(|row| {
                    row.content_hash == hash
                        && row.parse_status == ParseStatus::Ok
                        && symbol_state_ok
                });
            if !unchanged {
                changed.insert(rel, self.parse_file(&abs, &bytes, hash, old_symbols)?);
            }
        }

        let trigger_ids = changed
            .values()
            .flat_map(|file| file.old_symbols.iter().map(|symbol| symbol.id.clone()))
            .chain(
                changed
                    .values()
                    .flat_map(|file| file.symbols.iter().map(|symbol| symbol.id.clone())),
            )
            .chain(
                deleted
                    .iter()
                    .map(|path| self.db.symbol_ids_in_file(path))
                    .collect::<Result<Vec<_>>>()?
                    .into_iter()
                    .flatten(),
            )
            .collect::<BTreeSet<_>>();

        self.add_affected_dependents(&root, &mut changed, &trigger_ids)?;
        self.add_reference_dependents(&root, &mut changed, &trigger_ids)?;
        self.build_batch(reason, changed, deleted)
    }

    fn read_cached_file(&self, path: &str) -> Result<Option<CachedFile>> {
        let conn = self.db.connect()?;
        let result = conn.query_row(
            "SELECT path, language, size_bytes, content_hash, parsed_at, parse_status \
             FROM file WHERE path = ?1",
            [path],
            cached_file_from_row,
        );
        match result {
            Ok(row) => Ok(Some(row)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(error) => Err(store_error(error)),
        }
    }

    fn read_file_cache(&self) -> Result<BTreeMap<String, CachedFile>> {
        let conn = self.db.connect()?;
        let mut stmt = conn
            .prepare(
                "SELECT path, language, size_bytes, content_hash, parsed_at, parse_status \
                 FROM file ORDER BY path",
            )
            .map_err(store_error)?;
        let rows = stmt
            .query_map([], cached_file_from_row)
            .map_err(store_error)?;
        let mut cache = BTreeMap::new();
        for row in rows {
            let row = row.map_err(store_error)?;
            cache.insert(row.path.clone(), row);
        }
        Ok(cache)
    }

    fn parse_file(
        &self,
        abs: &Path,
        bytes: &[u8],
        hash: String,
        old_symbols: Vec<Symbol>,
    ) -> Result<PlannedFile> {
        let rel = relativize(abs, &canonical(self.repo_root)).ok_or_else(|| {
            CognisError::Store(format!("path is outside repository: {}", abs.display()))
        })?;
        let size_bytes = i64::try_from(bytes.len()).map_err(store_error)?;
        let parsed_at = unix_timestamp()?;
        let Some(source) = std::str::from_utf8(bytes).ok().map(str::to_owned) else {
            // Requirement 10.7: invalid UTF-8 records Failed, drops derived
            // rows, and lets the rest of the batch continue.
            return Ok(PlannedFile {
                row: FileCacheRow {
                    path: rel,
                    language: admitted_language(abs, self.config),
                    size_bytes,
                    content_hash: hash,
                    parsed_at,
                    parse_status: ParseStatus::Failed,
                },
                symbols: Vec::new(),
                old_symbols,
            });
        };
        let parsed = if detect_language(abs, self.config).is_some() {
            parse_source(&rel, &source)
        } else if let Some(kind) = detect_artifact(abs, self.config) {
            extract_artifact(kind, &rel, &source)
        } else {
            parse_source(&rel, &source)
        };
        let language = parsed
            .language
            .map(str::to_string)
            .unwrap_or_else(|| admitted_language(abs, self.config));
        // Requirement 8.7 / 10.7: a tree-sitter failure keeps a file row with
        // Failed status and no derived symbols. Partial recoveries keep theirs.
        let symbols = if parsed.status == ParseStatus::Failed {
            Vec::new()
        } else {
            parsed
                .symbols
                .iter()
                .map(|symbol| self.enricher.enrich(symbol).symbol)
                .collect::<Vec<_>>()
        };
        Ok(PlannedFile {
            row: FileCacheRow {
                path: rel,
                language,
                size_bytes,
                content_hash: hash,
                parsed_at,
                parse_status: parsed.status,
            },
            symbols,
            old_symbols,
        })
    }
    fn add_affected_dependents(
        &self,
        root: &Path,
        changed: &mut BTreeMap<String, PlannedFile>,
        trigger_ids: &BTreeSet<String>,
    ) -> Result<()> {
        let ids = trigger_ids.iter().cloned().collect::<Vec<_>>();
        if ids.is_empty() {
            return Ok(());
        }
        let inbound = self.db.edges_incident(&ids, Direction::In)?;
        let source_ids = inbound
            .into_iter()
            .map(|edge| edge.src_id)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        for symbol in self.db.hydrate_ids(&source_ids)? {
            let abs = root.join(Path::new(&symbol.file_path));
            if changed.contains_key(&symbol.file_path) || !is_admitted(&abs, root, self.config) {
                continue;
            }
            let bytes = read_exact_bytes(&abs)?;
            let hash = exact_byte_hash(&bytes);
            let old_symbols = self.db.symbols_in_file(&symbol.file_path)?;
            let file = self.parse_file(&abs, &bytes, hash, old_symbols)?;
            changed.insert(symbol.file_path, file);
        }
        Ok(())
    }

    fn add_reference_dependents(
        &self,
        root: &Path,
        changed: &mut BTreeMap<String, PlannedFile>,
        trigger_ids: &BTreeSet<String>,
    ) -> Result<()> {
        let initial_paths = changed.keys().cloned().collect::<BTreeSet<_>>();
        let changed_symbols = changed
            .values()
            .flat_map(|file| file.old_symbols.iter().chain(file.symbols.iter()).cloned())
            .filter(|symbol| trigger_ids.contains(&symbol.id))
            .collect::<Vec<_>>();
        let reference_values = changed_symbols
            .iter()
            .flat_map(symbol_destination_values)
            .filter(|value| !value.trim().is_empty())
            .collect::<BTreeSet<_>>();
        let mut candidate_ids = BTreeSet::new();
        for literal in &reference_values {
            candidate_ids.extend(
                self.db
                    .symbols_by_names(std::slice::from_ref(literal))?
                    .into_iter()
                    .map(|symbol| symbol.id),
            );
            candidate_ids.extend(
                self.db
                    .symbols_by_qualified_names(std::slice::from_ref(literal))?
                    .into_iter()
                    .map(|symbol| symbol.id),
            );
            let query = fts_literal(literal);
            if query.is_empty() {
                continue;
            }
            candidate_ids.extend(
                self.db
                    .fts_search(&query, REFERENCE_CANDIDATE_CAP)?
                    .into_iter()
                    .map(|hit| hit.symbol_id),
            );
        }
        for symbol in self
            .db
            .hydrate_ids(&candidate_ids.into_iter().collect::<Vec<_>>())?
        {
            let abs = root.join(Path::new(&symbol.file_path));
            if changed.contains_key(&symbol.file_path)
                || !symbol_references_any(&symbol, &reference_values)
                || initial_paths.contains(&symbol.file_path)
                || !is_admitted(&abs, root, self.config)
            {
                continue;
            }
            let bytes = read_exact_bytes(&abs)?;
            let hash = exact_byte_hash(&bytes);
            let old_symbols = self.db.symbols_in_file(&symbol.file_path)?;
            let file = self.parse_file(&abs, &bytes, hash, old_symbols)?;
            changed.insert(symbol.file_path, file);
        }
        Ok(())
    }

    fn build_batch(
        &self,
        reason: PublishReason,
        changed: BTreeMap<String, PlannedFile>,
        deleted: BTreeSet<String>,
    ) -> Result<PublishBatch> {
        let mut batch = PublishBatch::empty(reason);
        batch.file_deletes.extend(deleted.iter().cloned());

        let deleted_ids = deleted
            .iter()
            .map(|path| self.db.symbol_ids_in_file(path))
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .flatten()
            .collect::<BTreeSet<_>>();
        let old_symbols = changed
            .values()
            .flat_map(|file| file.old_symbols.iter().cloned())
            .chain(
                deleted
                    .iter()
                    .map(|path| self.db.symbols_in_file(path))
                    .collect::<Result<Vec<_>>>()?
                    .into_iter()
                    .flatten(),
            )
            .collect::<Vec<_>>();

        let new_symbols = changed
            .values()
            .flat_map(|file| file.symbols.iter().cloned())
            .collect::<Vec<_>>();
        let new_ids = new_symbols
            .iter()
            .map(|symbol| symbol.id.as_str())
            .collect::<BTreeSet<_>>();
        let stale_ids = old_symbols
            .iter()
            .map(|symbol| symbol.id.clone())
            .chain(deleted_ids)
            .filter(|id| !new_ids.contains(id.as_str()))
            .collect::<BTreeSet<_>>();

        batch.symbol_upserts = new_symbols.clone();
        batch.symbol_deletes = stale_ids.iter().cloned().collect();
        batch.vector_deletes = stale_ids.iter().cloned().collect();
        batch.queue_remove = stale_ids.iter().cloned().collect();
        batch.file_rows = changed.values().map(|file| file.row.clone()).collect();
        batch.queue_enqueue = new_symbols
            .iter()
            .map(|symbol| QueuedSymbol {
                symbol_id: symbol.id.clone(),
                file_path: symbol.file_path.clone(),
            })
            .collect();

        let old_ids = old_symbols
            .iter()
            .map(|symbol| symbol.id.clone())
            .collect::<Vec<_>>();
        batch.edge_deletes = self
            .db
            .edges_incident(&old_ids, Direction::Out)?
            .into_iter()
            .map(edge_delete_key)
            .collect();

        let owned = changed.keys().cloned().collect::<BTreeSet<_>>();
        let excluded = owned
            .iter()
            .cloned()
            .chain(deleted.iter().cloned())
            .collect::<BTreeSet<_>>();
        let resolver_symbols = self.bounded_resolver_symbols(&new_symbols, &excluded)?;
        batch.edge_upserts = to_edges(&resolve_edges(&resolver_symbols))
            .into_iter()
            .filter(|edge| {
                resolver_symbols
                    .iter()
                    .find(|symbol| symbol.id == edge.src_id)
                    .is_some_and(|symbol| owned.contains(&symbol.file_path))
            })
            .collect();
        let replacement_edges = batch
            .edge_upserts
            .iter()
            .cloned()
            .map(edge_delete_key)
            .collect::<BTreeSet<_>>();
        batch
            .edge_deletes
            .retain(|edge| !replacement_edges.contains(edge));
        sort_batch(&mut batch);
        Ok(batch)
    }

    fn bounded_resolver_symbols(
        &self,
        new_symbols: &[Symbol],
        excluded_paths: &BTreeSet<String>,
    ) -> Result<Vec<Symbol>> {
        let refs = collect_references(new_symbols);
        let mut candidates = self.db.symbols_by_names(
            &refs
                .names
                .iter()
                .cloned()
                .chain(refs.literals.iter().cloned())
                .collect::<Vec<_>>(),
        )?;
        candidates.extend(self.db.symbols_by_qualified_names(
            &refs.qualified_names.iter().cloned().collect::<Vec<_>>(),
        )?);

        // Integration joins use values embedded in source text rather than a
        // dedicated column. FTS narrows those lookups and the fixed cap keeps
        // the planner independent of repository size.
        for literal in &refs.literals {
            let query = fts_literal(literal);
            if query.is_empty() {
                continue;
            }
            let ids = self
                .db
                .fts_search(&query, REFERENCE_CANDIDATE_CAP)?
                .into_iter()
                .map(|hit| hit.symbol_id)
                .collect::<Vec<_>>();
            candidates.extend(self.db.hydrate_ids(&ids)?);
        }

        let mut by_id = BTreeMap::new();
        for symbol in candidates {
            if !excluded_paths.contains(&symbol.file_path) {
                by_id.insert(symbol.id.clone(), symbol);
            }
        }
        for symbol in new_symbols {
            by_id.insert(symbol.id.clone(), symbol.clone());
        }
        Ok(by_id.into_values().collect())
    }
}

#[derive(Debug, Clone)]
struct CachedFile {
    path: String,
    #[allow(dead_code)]
    language: String,
    #[allow(dead_code)]
    size_bytes: i64,
    content_hash: String,
    #[allow(dead_code)]
    parsed_at: i64,
    parse_status: ParseStatus,
}

#[derive(Debug, Clone)]
struct PlannedFile {
    row: FileCacheRow,
    symbols: Vec<Symbol>,
    old_symbols: Vec<Symbol>,
}

#[derive(Debug, Default)]
struct References {
    names: BTreeSet<String>,
    qualified_names: BTreeSet<String>,
    literals: BTreeSet<String>,
}
fn cached_file_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<CachedFile> {
    let status: String = row.get(5)?;
    Ok(CachedFile {
        path: row.get(0)?,
        language: row.get(1)?,
        size_bytes: row.get(2)?,
        content_hash: row.get(3)?,
        parsed_at: row.get(4)?,
        parse_status: parse_status(&status),
    })
}

fn is_admitted(abs: &Path, root: &Path, config: &Config) -> bool {
    if !std::fs::symlink_metadata(abs).is_ok_and(|metadata| metadata.file_type().is_file()) {
        return false;
    }
    let Some(rel) = relativize(abs, root) else {
        return false;
    };
    let ignored = config
        .repo
        .ignore
        .iter()
        .map(String::as_str)
        .chain([cognis_core::config::CONFIG_DIR_NAME, ".git"]);
    if rel
        .split('/')
        .any(|component| ignored.clone().any(|name| component == name))
    {
        return false;
    }
    detect_language(abs, config).is_some()
        || (config.artifact.enabled && detect_artifact(abs, config).is_some())
}

fn symbol_references_any(symbol: &Symbol, values: &BTreeSet<String>) -> bool {
    let haystack = [symbol.signature.as_deref(), symbol.body_excerpt.as_deref()]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join("\n");
    values.iter().any(|value| {
        haystack.contains(value)
            || normalize_ident(&symbol.name) == normalize_ident(value)
            || symbol.name == *value
    })
}

fn symbol_destination_values(symbol: &Symbol) -> BTreeSet<String> {
    let mut values = BTreeSet::from([symbol.name.clone(), symbol.qualified_name.clone()]);
    let string_re =
        Regex::new(r#""([^"\\]*)"|'([^'\\]*)'|`([^`\\]*)`"#).expect("valid string regex");
    for text in [symbol.signature.as_deref(), symbol.body_excerpt.as_deref()]
        .into_iter()
        .flatten()
    {
        for capture in string_re.captures_iter(text) {
            if let Some(value) = capture
                .get(1)
                .or_else(|| capture.get(2))
                .or_else(|| capture.get(3))
            {
                if !value.as_str().is_empty() {
                    values.insert(value.as_str().to_string());
                }
            }
        }
    }
    if matches!(symbol.kind, SymbolKind::Var | SymbolKind::Const)
        && matches!(symbol.language.as_str(), "yaml" | "toml")
    {
        let cleaned = symbol
            .name
            .split('.')
            .map(|segment| segment.split('[').next().unwrap_or(segment))
            .filter(|segment| !segment.trim().is_empty())
            .collect::<Vec<_>>()
            .join(".");
        if !cleaned.is_empty() {
            values.insert(cleaned.clone());
            if let Some(leaf) = cleaned.rsplit('.').next() {
                values.insert(leaf.to_string());
            }
        }
    }
    if symbol.kind == SymbolKind::Route || symbol.language == "sql" {
        let normalized = normalize_ident(&symbol.name);
        if !normalized.is_empty() {
            values.insert(normalized);
        }
    }
    values
}

fn collect_references(symbols: &[Symbol]) -> References {
    let call_re = Regex::new(r"\b([A-Za-z_][A-Za-z0-9_]*)\s*\(").expect("valid call regex");
    let ident_re = Regex::new(r"[A-Za-z_][A-Za-z0-9_]*").expect("valid identifier regex");
    let string_re =
        Regex::new(r#""([^"\\]*)"|'([^'\\]*)'|`([^`\\]*)`"#).expect("valid string regex");
    let mut refs = References::default();
    for symbol in symbols {
        refs.qualified_names.insert(symbol.qualified_name.clone());
        for text in [symbol.signature.as_deref(), symbol.body_excerpt.as_deref()]
            .into_iter()
            .flatten()
        {
            for capture in call_re.captures_iter(text) {
                if let Some(name) = capture.get(1) {
                    refs.names.insert(name.as_str().to_string());
                }
            }
            for capture in string_re.captures_iter(text) {
                if let Some(value) = capture
                    .get(1)
                    .or_else(|| capture.get(2))
                    .or_else(|| capture.get(3))
                {
                    if !value.as_str().is_empty() {
                        refs.literals.insert(value.as_str().to_string());
                    }
                }
            }
            let upper = text.to_ascii_uppercase();
            if [
                "SELECT", "INSERT", "UPDATE", "DELETE", "UPSERT", "MERGE", "REPLACE",
            ]
            .iter()
            .any(|verb| upper.contains(verb))
            {
                for token in ident_re.find_iter(text) {
                    let normalized = normalize_ident(token.as_str());
                    if !normalized.is_empty() {
                        refs.names.insert(normalized);
                    }
                }
            }
        }
        if matches!(symbol.kind, SymbolKind::Class | SymbolKind::Interface)
            && matches!(symbol.language.as_str(), "java" | "csharp")
        {
            let text = symbol
                .body_excerpt
                .as_deref()
                .or(symbol.signature.as_deref())
                .unwrap_or("");
            for token in ident_re.find_iter(text) {
                refs.names.insert(token.as_str().to_string());
            }
        }
        refs.names.insert(symbol.name.clone());
    }
    refs
}

fn exact_byte_hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn admitted_language(abs: &Path, config: &Config) -> String {
    if let Some(label) = detect_language(abs, config) {
        return label.to_string();
    }
    if let Some(kind) = detect_artifact(abs, config) {
        return format!("{kind:?}").to_ascii_lowercase();
    }
    "text".to_string()
}

fn read_exact_bytes(path: &Path) -> Result<Vec<u8>> {
    std::fs::read(path)
        .map_err(|error| CognisError::Store(format!("read {}: {error}", path.display())))
}

fn parse_status(value: &str) -> ParseStatus {
    match value {
        "ok" | "success" => ParseStatus::Ok,
        "partial" => ParseStatus::Partial,
        _ => ParseStatus::Failed,
    }
}

fn unix_timestamp() -> Result<i64> {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(store_error)?
        .as_secs();
    i64::try_from(seconds).map_err(store_error)
}

fn edge_delete_key(edge: Edge) -> (String, String, String) {
    (edge.src_id, edge.dst_id, edge_kind_string(edge.kind))
}

fn edge_kind_string(kind: EdgeKind) -> String {
    match kind {
        EdgeKind::Calls => "calls",
        EdgeKind::Imports => "imports",
        EdgeKind::Inherits => "inherits",
        EdgeKind::Implements => "implements",
        EdgeKind::Reads => "reads",
        EdgeKind::Writes => "writes",
        EdgeKind::RoutesTo => "routes_to",
        EdgeKind::Tests => "tests",
    }
    .to_string()
}

fn fts_literal(value: &str) -> String {
    let escaped = value.replace('"', "\"\"");
    if escaped.trim().is_empty() {
        String::new()
    } else {
        format!("\"{escaped}\"")
    }
}

fn sort_batch(batch: &mut PublishBatch) {
    batch
        .symbol_upserts
        .sort_by(|left, right| left.id.cmp(&right.id));
    batch.symbol_deletes.sort();
    batch.symbol_deletes.dedup();
    batch.edge_upserts.sort_by(|left, right| {
        (&left.src_id, &left.dst_id, edge_kind_string(left.kind)).cmp(&(
            &right.src_id,
            &right.dst_id,
            edge_kind_string(right.kind),
        ))
    });
    batch.edge_deletes.sort();
    batch.edge_deletes.dedup();
    batch.vector_deletes.sort();
    batch.vector_deletes.dedup();
    batch
        .file_rows
        .sort_by(|left, right| left.path.cmp(&right.path));
    batch.file_deletes.sort();
    batch.file_deletes.dedup();
    batch
        .queue_enqueue
        .sort_by(|left, right| left.symbol_id.cmp(&right.symbol_id));
    batch.queue_remove.sort();
    batch.queue_remove.dedup();
}

fn store_error(error: impl std::fmt::Display) -> CognisError {
    CognisError::Store(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use cognis_store::{database_digest, IndexPublisher};

    fn workspace(tag: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "cognis-planner-{tag}-{}-{}",
            std::process::id(),
            unix_timestamp().unwrap()
        ));
        std::fs::create_dir_all(path.join("src")).unwrap();
        path
    }

    #[test]
    fn exact_hash_changes_for_cosmetic_byte_change() {
        assert_ne!(
            exact_byte_hash(b"fn f() {}\n"),
            exact_byte_hash(b"fn f() { }\n")
        );
        assert_eq!(
            exact_byte_hash(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn reconcile_skips_only_hash_success_with_derived_state() {
        let root = workspace("skip");
        std::fs::write(root.join("src/a.py"), "def a():\n    return 1\n").unwrap();
        let db = Database::open(root.join("index.db")).unwrap();
        let config = Config::default();
        let planner = ChangePlanner::new(&db, &config, &root);
        let first = planner.plan(PlanRequest::Reconcile).unwrap();
        assert_eq!(first.file_rows.len(), 1);
        let mut writable = db.clone();
        writable.publish(&first).unwrap();

        let unchanged = planner.plan(PlanRequest::Reconcile).unwrap();
        assert!(unchanged.file_rows.is_empty());

        let conn = db.connect().unwrap();
        conn.execute("DELETE FROM symbol WHERE file_path = 'src/a.py'", [])
            .unwrap();
        let repairing = planner.plan(PlanRequest::Reconcile).unwrap();
        assert_eq!(repairing.file_rows[0].path, "src/a.py");
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn touched_target_edit_pulls_inbound_dependent_into_same_batch() {
        let root = workspace("dependent");
        std::fs::write(
            root.join("src/caller.py"),
            "def caller():\n    return target()\n",
        )
        .unwrap();
        std::fs::write(root.join("src/target.py"), "def target():\n    return 1\n").unwrap();
        let db = Database::open(root.join("index.db")).unwrap();
        let config = Config::default();
        let planner = ChangePlanner::new(&db, &config, &root);
        let initial = planner.plan(PlanRequest::Reconcile).unwrap();
        let old_target = initial
            .symbol_upserts
            .iter()
            .find(|symbol| symbol.name == "target")
            .unwrap()
            .id
            .clone();
        assert!(initial
            .edge_upserts
            .iter()
            .any(|edge| edge.dst_id == old_target));
        let mut writable = db.clone();
        writable.publish(&initial).unwrap();

        std::fs::write(root.join("src/target.py"), "def target():\n    return 2\n").unwrap();
        let edit = planner
            .plan(PlanRequest::Touched(vec![root.join("src/target.py")]))
            .unwrap();
        assert_eq!(
            edit.file_rows
                .iter()
                .map(|row| row.path.as_str())
                .collect::<Vec<_>>(),
            vec!["src/caller.py", "src/target.py"]
        );
        assert!(edit.symbol_deletes.contains(&old_target));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn reconcile_deletes_absent_cache_paths_and_full_reparses_current_paths() {
        let root = workspace("modes");
        std::fs::write(root.join("src/a.py"), "def a():\n    return 1\n").unwrap();
        let db = Database::open(root.join("index.db")).unwrap();
        let config = Config::default();
        let planner = ChangePlanner::new(&db, &config, &root);
        let initial = planner.plan(PlanRequest::Reconcile).unwrap();
        let mut writable = db.clone();
        writable.publish(&initial).unwrap();
        std::fs::remove_file(root.join("src/a.py")).unwrap();
        let removed = planner.plan(PlanRequest::Reconcile).unwrap();
        assert_eq!(removed.file_deletes, vec!["src/a.py"]);

        std::fs::write(root.join("src/b.py"), "def b():\n    return 2\n").unwrap();
        let full = planner.plan(PlanRequest::Full).unwrap();
        assert_eq!(
            full.file_rows
                .iter()
                .map(|row| row.path.as_str())
                .collect::<Vec<_>>(),
            vec!["src/b.py"]
        );
        std::fs::remove_dir_all(root).ok();
    }

    fn publish(db: &Database, batch: &PublishBatch) {
        let mut writable = db.clone();
        writable.publish(batch).unwrap();
    }

    fn file_parse_status(db: &Database, path: &str) -> ParseStatus {
        let conn = db.connect().unwrap();
        let status: String = conn
            .query_row(
                "SELECT parse_status FROM file WHERE path = ?1",
                [path],
                |row| row.get(0),
            )
            .unwrap();
        parse_status(&status)
    }

    fn file_row_count(db: &Database, path: &str) -> i64 {
        let conn = db.connect().unwrap();
        conn.query_row("SELECT COUNT(*) FROM file WHERE path = ?1", [path], |row| {
            row.get(0)
        })
        .unwrap()
    }

    fn outbound_edge_count(db: &Database, symbol_ids: &[String]) -> usize {
        if symbol_ids.is_empty() {
            return 0;
        }
        db.edges_incident(symbol_ids, Direction::Out).unwrap().len()
    }

    fn clean_rebuild(root: &Path) -> Database {
        let db = Database::open(std::env::temp_dir().join(format!(
            "cognis-planner-rebuild-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )))
        .unwrap();
        let config = Config::default();
        let batch = ChangePlanner::new(&db, &config, root)
            .plan(PlanRequest::Full)
            .unwrap();
        publish(&db, &batch);
        db
    }

    #[test]
    fn utf8_failure_records_failed_drops_derived_rows_and_continues_sibling() {
        let root = workspace("utf8");
        std::fs::write(root.join("src/ok.py"), "def ok():\n    return 1\n").unwrap();
        std::fs::write(root.join("src/bad.py"), "def bad():\n    return 2\n").unwrap();
        let db = Database::open(root.join("index.db")).unwrap();
        let config = Config::default();
        let planner = ChangePlanner::new(&db, &config, &root);
        publish(&db, &planner.plan(PlanRequest::Reconcile).unwrap());
        let stale_ids = db
            .symbols_in_file("src/bad.py")
            .unwrap()
            .into_iter()
            .map(|symbol| symbol.id)
            .collect::<Vec<_>>();
        assert!(!stale_ids.is_empty());

        std::fs::write(root.join("src/bad.py"), [0xff, 0xfe, 0x80]).unwrap();
        let batch = planner.plan(PlanRequest::Reconcile).unwrap();
        assert!(batch
            .file_rows
            .iter()
            .any(|row| row.path == "src/ok.py" || row.path == "src/bad.py"));
        let bad = batch
            .file_rows
            .iter()
            .find(|row| row.path == "src/bad.py")
            .unwrap();
        assert_eq!(bad.parse_status, ParseStatus::Failed);
        assert!(!batch
            .symbol_upserts
            .iter()
            .any(|s| s.file_path == "src/bad.py"));
        for id in &stale_ids {
            assert!(batch.symbol_deletes.contains(id));
        }
        publish(&db, &batch);

        assert_eq!(file_parse_status(&db, "src/bad.py"), ParseStatus::Failed);
        assert!(db.symbols_in_file("src/bad.py").unwrap().is_empty());
        assert_eq!(outbound_edge_count(&db, &stale_ids), 0);
        assert!(!db.symbols_in_file("src/ok.py").unwrap().is_empty());
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn tree_sitter_failure_records_failed_and_drops_fallback_symbols() {
        let root = workspace("tsfail");
        std::fs::write(root.join("src/ok.py"), "def ok():\n    return 1\n").unwrap();
        std::fs::write(
            root.join("src/Broken.cs"),
            "@@@ <<< not valid code >>> ;;; ((( ",
        )
        .unwrap();
        let db = Database::open(root.join("index.db")).unwrap();
        let config = Config::default();
        let planner = ChangePlanner::new(&db, &config, &root);
        let batch = planner.plan(PlanRequest::Reconcile).unwrap();
        let broken = batch
            .file_rows
            .iter()
            .find(|row| row.path == "src/Broken.cs")
            .expect("failed parse still writes a file row");
        assert_eq!(broken.parse_status, ParseStatus::Failed);
        assert!(!batch
            .symbol_upserts
            .iter()
            .any(|symbol| symbol.file_path == "src/Broken.cs"));
        assert!(batch
            .symbol_upserts
            .iter()
            .any(|symbol| symbol.file_path == "src/ok.py"));
        publish(&db, &batch);
        assert_eq!(file_parse_status(&db, "src/Broken.cs"), ParseStatus::Failed);
        assert!(db.symbols_in_file("src/Broken.cs").unwrap().is_empty());
        assert!(!db.symbols_in_file("src/ok.py").unwrap().is_empty());
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn empty_file_retains_file_row_and_removes_derived_rows() {
        let root = workspace("empty");
        std::fs::write(root.join("src/a.py"), "def a():\n    return 1\n").unwrap();
        let db = Database::open(root.join("index.db")).unwrap();
        let config = Config::default();
        let planner = ChangePlanner::new(&db, &config, &root);
        publish(&db, &planner.plan(PlanRequest::Reconcile).unwrap());
        let stale_ids = db
            .symbols_in_file("src/a.py")
            .unwrap()
            .into_iter()
            .map(|symbol| symbol.id)
            .collect::<Vec<_>>();
        assert!(!stale_ids.is_empty());

        std::fs::write(root.join("src/a.py"), "").unwrap();
        let batch = planner
            .plan(PlanRequest::Touched(vec![root.join("src/a.py")]))
            .unwrap();
        assert_eq!(batch.file_rows.len(), 1);
        assert_eq!(batch.file_rows[0].path, "src/a.py");
        assert_eq!(batch.file_rows[0].size_bytes, 0);
        assert_eq!(batch.file_rows[0].parse_status, ParseStatus::Ok);
        assert!(batch.symbol_upserts.is_empty());
        for id in &stale_ids {
            assert!(batch.symbol_deletes.contains(id));
        }
        publish(&db, &batch);
        assert_eq!(file_row_count(&db, "src/a.py"), 1);
        assert!(db.symbols_in_file("src/a.py").unwrap().is_empty());
        assert_eq!(outbound_edge_count(&db, &stale_ids), 0);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn rename_removes_old_path_and_creates_new_path_in_same_batch() {
        let root = workspace("rename");
        std::fs::write(root.join("src/old.py"), "def renamed():\n    return 1\n").unwrap();
        let db = Database::open(root.join("index.db")).unwrap();
        let config = Config::default();
        let planner = ChangePlanner::new(&db, &config, &root);
        publish(&db, &planner.plan(PlanRequest::Reconcile).unwrap());
        let old_ids = db
            .symbols_in_file("src/old.py")
            .unwrap()
            .into_iter()
            .map(|symbol| symbol.id)
            .collect::<Vec<_>>();

        std::fs::rename(root.join("src/old.py"), root.join("src/new.py")).unwrap();
        let batch = planner
            .plan(PlanRequest::Touched(vec![
                root.join("src/old.py"),
                root.join("src/new.py"),
            ]))
            .unwrap();
        assert_eq!(batch.file_deletes, vec!["src/old.py".to_string()]);
        assert_eq!(
            batch
                .file_rows
                .iter()
                .map(|row| row.path.as_str())
                .collect::<Vec<_>>(),
            vec!["src/new.py"]
        );
        for id in &old_ids {
            assert!(batch.symbol_deletes.contains(id));
        }
        assert!(batch
            .symbol_upserts
            .iter()
            .any(|symbol| symbol.file_path == "src/new.py"));
        publish(&db, &batch);
        assert_eq!(file_row_count(&db, "src/old.py"), 0);
        assert_eq!(file_row_count(&db, "src/new.py"), 1);
        assert!(db.symbols_in_file("src/old.py").unwrap().is_empty());
        assert!(!db.symbols_in_file("src/new.py").unwrap().is_empty());
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn readd_identical_content_matches_clean_rebuild_digest() {
        let root = workspace("readd");
        let source = "def restored():\n    return 1\n";
        std::fs::write(root.join("src/a.py"), source).unwrap();
        let db = Database::open(root.join("index.db")).unwrap();
        let config = Config::default();
        let planner = ChangePlanner::new(&db, &config, &root);
        publish(&db, &planner.plan(PlanRequest::Reconcile).unwrap());

        std::fs::remove_file(root.join("src/a.py")).unwrap();
        publish(
            &db,
            &planner
                .plan(PlanRequest::Touched(vec![root.join("src/a.py")]))
                .unwrap(),
        );
        std::fs::write(root.join("src/a.py"), source).unwrap();
        publish(
            &db,
            &planner
                .plan(PlanRequest::Touched(vec![root.join("src/a.py")]))
                .unwrap(),
        );

        let clean = clean_rebuild(&root);
        assert_eq!(
            database_digest(&db).unwrap(),
            database_digest(&clean).unwrap()
        );
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn full_rescan_missed_delete_matches_clean_rebuild_digest() {
        let root = workspace("missed");
        std::fs::write(root.join("src/keep.py"), "def keep():\n    return 1\n").unwrap();
        std::fs::write(root.join("src/gone.py"), "def gone():\n    return 2\n").unwrap();
        let db = Database::open(root.join("index.db")).unwrap();
        let config = Config::default();
        let planner = ChangePlanner::new(&db, &config, &root);
        publish(&db, &planner.plan(PlanRequest::Reconcile).unwrap());

        std::fs::remove_file(root.join("src/gone.py")).unwrap();
        publish(&db, &planner.plan(PlanRequest::Full).unwrap());

        let clean = clean_rebuild(&root);
        assert_eq!(
            database_digest(&db).unwrap(),
            database_digest(&clean).unwrap()
        );
        assert_eq!(file_row_count(&db, "src/gone.py"), 0);
        assert_eq!(file_row_count(&db, "src/keep.py"), 1);
        std::fs::remove_dir_all(root).ok();
    }
}
