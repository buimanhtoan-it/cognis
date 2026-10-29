//! Writer stage of the indexer pipeline (Task 8.2).
//!
//! Persists one file's symbols and resolved edges through the
//! [`SymbolWriter`](cognis_store::SymbolWriter) trait. Rust mirror of
//! `cognis_indexer.writer.IndexWriter`'s per-file write path, scoped to the
//! symbol + edge surface the trait exposes (the `file` cache row + incremental
//! removal diff land with the watcher in Task 8.3; embeddings are the
//! embedder's surface).
//!
//! **Per-file transaction:** each [`SymbolWriter`] call runs under its own
//! `BEGIN IMMEDIATE` transaction in `cognis-store`, so a file's symbols commit
//! atomically and its edges commit atomically. Symbols are written **before**
//! edges so referenced rows exist first (the `edge` FK is application-layer, so
//! this is ordering hygiene rather than a hard constraint).
//!
//! **Secret scrub before persist (Requirement 9.3):** this stage persists
//! whatever the [`Enricher`](crate::enricher::Enricher) handed it. Callers MUST
//! enrich symbols before constructing a [`FileWritePayload`]; the redaction
//! therefore always happens upstream of any DB write.
//!
//! **`meta.dst_missing` convention:** when a symbol is removed,
//! [`IndexWriter::delete_symbols`] delegates to `SymbolWriter::delete_symbol`,
//! which deletes outbound edges and flags inbound edges `meta.dst_missing =
//! true` rather than erasing them — preserving the audit trail.

use std::time::{SystemTime, UNIX_EPOCH};

use cognis_core::{Edge, Result, Symbol};
use cognis_store::SymbolWriter;

/// Everything the Writer needs to persist one file's parse pass.
///
/// Symbols are expected to be **already enriched** (secrets scrubbed,
/// `untrusted_flags` set — Requirement 9.3) and edges already converted from
/// the resolver output (see [`crate::resolver::to_edges`]).
#[derive(Debug, Clone, Default)]
pub struct FileWritePayload {
    /// Enriched symbols extracted from this file in this pass.
    pub symbols: Vec<Symbol>,
    /// Resolved edges whose `src_id` belongs to this file.
    pub edges: Vec<Edge>,
}

impl FileWritePayload {
    /// Construct a payload from enriched symbols and resolved edges.
    pub fn new(symbols: Vec<Symbol>, edges: Vec<Edge>) -> Self {
        FileWritePayload { symbols, edges }
    }
}

/// Persists symbol/edge batches through any [`SymbolWriter`] backend.
///
/// Generic over the writer so it works against `cognis_store::Database` in
/// production and any in-memory `SymbolWriter` in tests.
#[derive(Debug)]
pub struct IndexWriter<W: SymbolWriter> {
    writer: W,
}

impl<W: SymbolWriter> IndexWriter<W> {
    /// Wrap a [`SymbolWriter`] backend.
    pub fn new(writer: W) -> Self {
        IndexWriter { writer }
    }

    /// Consume the writer, returning the underlying backend.
    pub fn into_inner(self) -> W {
        self.writer
    }

    /// Persist one file's payload: upsert symbols, then upsert edges.
    ///
    /// Each symbol's `updated_at` is stamped with the current epoch second at
    /// write time (mirror the Python writer's `now_epoch()` on every write), so
    /// re-indexing an unchanged file still advances the freshness marker.
    /// Returns early without touching the DB when the payload is empty.
    pub fn write_file(&mut self, payload: &FileWritePayload) -> Result<()> {
        if payload.symbols.is_empty() && payload.edges.is_empty() {
            return Ok(());
        }

        if !payload.symbols.is_empty() {
            let now = now_epoch();
            let mut stamped = payload.symbols.clone();
            for s in &mut stamped {
                s.updated_at = now;
            }
            self.writer.upsert_symbols(&stamped)?;
        }

        if !payload.edges.is_empty() {
            self.writer.upsert_edges(&payload.edges)?;
        }

        Ok(())
    }

    /// Remove each id in `ids`, applying the delete cascade (outbound edges
    /// deleted, inbound edges flagged `meta.dst_missing = true`). Used by the
    /// incremental path when a file's symbol set shrinks between passes.
    pub fn delete_symbols(&mut self, ids: &[String]) -> Result<()> {
        for id in ids {
            self.writer.delete_symbol(id)?;
        }
        Ok(())
    }
}

/// Current Unix epoch in whole seconds (mirror `cognis.db.now_epoch`).
fn now_epoch() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enricher::Enricher;
    use crate::parser::parse_source;
    use crate::resolver::{resolve_edges, to_edges};
    use cognis_store::Database;

    fn mem_db() -> Database {
        // Per-thread in-memory DB; a test runs on one thread so the cached
        // connection is reused across reader/writer handles.
        Database::open(":memory:").expect("open in-memory db")
    }

    #[test]
    fn writes_symbols_and_edges_per_file() {
        let out = parse_source(
            "m.py",
            "def helper():\n    return 1\n\ndef caller():\n    return helper()\n",
        );
        let enricher = Enricher::new();
        let symbols: Vec<Symbol> = out
            .symbols
            .iter()
            .map(|s| enricher.enrich(s).symbol)
            .collect();
        let edges = to_edges(&resolve_edges(&symbols));
        assert!(!edges.is_empty(), "expected a caller->helper edge");

        let db = mem_db();
        let mut writer = IndexWriter::new(db.clone());
        writer
            .write_file(&FileWritePayload::new(symbols.clone(), edges.clone()))
            .expect("write_file");

        // Symbols + edges are now persisted and readable.
        assert_eq!(db.list_symbols().unwrap().len(), symbols.len());
        assert_eq!(db.list_edges().unwrap().len(), edges.len());
        // updated_at was stamped (non-zero) at write time.
        assert!(db.list_symbols().unwrap().iter().all(|s| s.updated_at > 0));
    }

    #[test]
    fn redacted_body_is_what_lands_in_db() {
        let src = "def connect():\n    password = \"hunter2secret\"\n    return password\n";
        let out = parse_source("m.py", src);
        let enricher = Enricher::new();
        let symbols: Vec<Symbol> = out
            .symbols
            .iter()
            .map(|s| enricher.enrich(s).symbol)
            .collect();

        let db = mem_db();
        let mut writer = IndexWriter::new(db.clone());
        writer
            .write_file(&FileWritePayload::new(symbols, Vec::new()))
            .expect("write_file");

        // Requirement 9.3: the secret never reaches the DB.
        for s in db.list_symbols().unwrap() {
            let body = s.body_excerpt.unwrap_or_default();
            assert!(!body.contains("hunter2secret"));
        }
    }

    #[test]
    fn delete_symbol_flags_inbound_edge_dst_missing() {
        let out = parse_source(
            "m.py",
            "def helper():\n    return 1\n\ndef caller():\n    return helper()\n",
        );
        let symbols: Vec<Symbol> = out.symbols.clone();
        let edges = to_edges(&resolve_edges(&symbols));
        let helper_id = symbols
            .iter()
            .find(|s| s.name == "helper")
            .unwrap()
            .id
            .clone();

        let db = mem_db();
        let mut writer = IndexWriter::new(db.clone());
        writer
            .write_file(&FileWritePayload::new(symbols, edges))
            .expect("write");

        writer
            .delete_symbols(std::slice::from_ref(&helper_id))
            .expect("delete");

        // Inbound edge to helper is kept but flagged dst_missing.
        let inbound: Vec<Edge> = db
            .list_edges()
            .unwrap()
            .into_iter()
            .filter(|e| e.dst_id == helper_id)
            .collect();
        assert!(!inbound.is_empty(), "inbound edge should be preserved");
        assert!(inbound.iter().all(|e| e.dst_missing()));
    }

    #[test]
    fn empty_payload_is_noop() {
        let db = mem_db();
        let mut writer = IndexWriter::new(db.clone());
        writer
            .write_file(&FileWritePayload::default())
            .expect("noop");
        assert_eq!(db.list_symbols().unwrap().len(), 0);
    }
}
