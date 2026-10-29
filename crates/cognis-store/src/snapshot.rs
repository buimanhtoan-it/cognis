//! `GenerationSnapshot` — one pinned read per tool call.
//!
//! Spec: `.kiro/specs/cognis-one-toggle-engine` (Requirements 13.1–13.4, 9.x).
//!
//! SQLite in WAL mode — which `Database::open_new_connection` already sets —
//! gives a reader holding an open read transaction a stable view of the database
//! as of the last commit before its first read. New commits by the writer stay
//! invisible until that transaction ends. That is the whole mechanism behind
//! `Pinned_Generation`: open one deferred read transaction per tool call, read
//! `generation.source_generation` as the **first** statement (which is what
//! actually materialises the snapshot), and every later read in the call is
//! automatically consistent with it. No MVCC layer of our own is required.
//!

use std::rc::Rc;

use cognis_core::{Edge, Hit, Result, Symbol};
use rusqlite::Connection;

use crate::{
    build_code_graph_on_connection, edges_incident_on_connection, fts_search_on_connection,
    resolve_symbol_on_connection, store_err, symbol_ids_in_file_on_connection,
    symbols_by_values_on_connection, symbols_in_file_on_connection, vec_row_count_on_connection,
    vec_search_on_connection, CodeGraph, Database, SymbolValueColumn, VectorSpaceFingerprint,
};

/// Which side of an edge a traversal follows.
///
/// The MCP `dependency_trace` tool takes `direction` as a string today
/// (`"out" | "in" | "both"` in `crates/cognis-mcp/src/store_engine.rs`). This
/// enum is the typed form the bounded `edges_incident` query takes; the string
/// conversions exist so the tool's wire contract does not move
/// (Requirement 22.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Direction {
    /// Follow `src_id → dst_id`: what this symbol depends on.
    Out,
    /// Follow `dst_id → src_id`: what depends on this symbol. This is the
    /// direction `Affected_Dependents` is collected in (Requirement 10.1).
    In,
    /// Both of the above.
    Both,
}

impl Direction {
    /// The wire string the `dependency_trace` tool already uses.
    pub fn as_str(&self) -> &'static str {
        match self {
            Direction::Out => "out",
            Direction::In => "in",
            Direction::Both => "both",
        }
    }

    /// Parse the tool's `direction` argument. `None` for an unrecognised value,
    /// so the caller decides the default rather than this function guessing.
    pub fn parse_str(s: &str) -> Option<Direction> {
        match s {
            "out" => Some(Direction::Out),
            "in" => Some(Direction::In),
            "both" => Some(Direction::Both),
            _ => None,
        }
    }

    /// Whether a traversal in this direction follows `src → dst`.
    pub fn follows_outbound(&self) -> bool {
        matches!(self, Direction::Out | Direction::Both)
    }

    /// Whether a traversal in this direction follows `dst → src`.
    pub fn follows_inbound(&self) -> bool {
        matches!(self, Direction::In | Direction::Both)
    }
}

/// One pinned read snapshot of a [`Database`].
///
/// Holds an open deferred read transaction for its whole lifetime, so every
/// read performed through it observes exactly one committed generation
/// (Requirement 13.1–13.3). The snapshot is released on drop
/// (Requirement 13.4).
///
/// The `'a` lifetime ties the snapshot to the [`Database`] it was opened from,
/// so a snapshot can never outlive its handle.
pub struct GenerationSnapshot<'a> {
    /// The handle this snapshot reads from.
    db: &'a Database,
    /// The connection holding the open deferred read transaction. Every read
    /// this snapshot performs runs on **this** connection — a read issued on a
    /// different connection would see a different (later) commit and break the
    /// pinning guarantee.
    conn: Rc<Connection>,
    /// Freshness values captured from the materialised snapshot. Caching them
    /// keeps these infallible accessors honest: an already-open snapshot cannot
    /// fail later merely while rendering response metadata.
    source_generation: i64,
    semantic_generation: i64,
    pending_vector_count: i64,
}

impl<'a> GenerationSnapshot<'a> {
    /// Open a snapshot: `BEGIN DEFERRED`, then read
    /// `generation.source_generation` as the first statement so the WAL snapshot
    /// is materialised before any other read.
    pub fn open(db: &'a Database) -> Result<Self> {
        if db.path() == ":memory:" {
            // A second connection to `:memory:` would be a different database.
            // The cached connection is safe here because an in-memory database
            // has no concurrent external writer to isolate from.
            let conn = db.connect()?;
            return Self::open_on_connection(db, conn);
        }

        let conn = Rc::new(db.open_new_connection()?);
        Self::open_on_connection(db, conn)
    }

    fn open_on_connection(db: &'a Database, conn: Rc<Connection>) -> Result<Self> {
        conn.execute_batch("BEGIN DEFERRED").map_err(store_err)?;

        let values = (|| -> Result<(i64, i64, i64)> {
            // This MUST be the first statement after BEGIN: in WAL mode the
            // first read pins the transaction to the current committed state.
            let source_generation = conn
                .query_row(
                    "SELECT source_generation FROM generation WHERE id = 1",
                    [],
                    |row| row.get(0),
                )
                .map_err(store_err)?;
            let semantic_generation = conn
                .query_row(
                    "SELECT semantic_generation FROM generation WHERE id = 1",
                    [],
                    |row| row.get(0),
                )
                .map_err(store_err)?;
            let pending_vector_count = conn
                .query_row("SELECT COUNT(*) FROM embedding_queue", [], |row| row.get(0))
                .map_err(store_err)?;
            Ok((source_generation, semantic_generation, pending_vector_count))
        })();

        let (source_generation, semantic_generation, pending_vector_count) = match values {
            Ok(values) => values,
            Err(error) => {
                let _ = conn.execute_batch("ROLLBACK");
                return Err(error);
            }
        };

        Ok(Self {
            db,
            conn,
            source_generation,
            semantic_generation,
            pending_vector_count,
        })
    }

    /// The handle this snapshot was opened from.
    pub fn database(&self) -> &'a Database {
        self.db
    }

    /// The connection holding this snapshot's open read transaction.
    ///
    /// Exposed so the bounded-read query methods (task 5.2) and the digest
    /// (task 3.1) can run their statements inside the pinned transaction rather
    /// than on a fresh connection.
    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    /// The `Source_Generation` this snapshot is pinned to — the value every
    /// read below is consistent with, and the value reported as
    /// `source_generation` in `Freshness_Metadata` (Requirement 15.2).
    pub fn source_generation(&self) -> i64 {
        self.source_generation
    }

    /// The committed `Semantic_Generation` as of this snapshot
    /// (Requirement 15.3).
    pub fn semantic_generation(&self) -> i64 {
        self.semantic_generation
    }

    /// `SELECT COUNT(*) FROM embedding_queue` as of this snapshot — the literal
    /// definition of `Pending_Vector_Count` (Requirement 11.6, 15.4).
    pub fn pending_vector_count(&self) -> i64 {
        self.pending_vector_count
    }

    /// Symbols declared by exactly `path`, read inside this pinned generation.
    pub fn symbols_in_file(&self, path: &str) -> Result<Vec<Symbol>> {
        symbols_in_file_on_connection(self.connection(), path)
    }

    /// Symbol ids declared by exactly `path`, read inside this pinned generation.
    pub fn symbol_ids_in_file(&self, path: &str) -> Result<Vec<String>> {
        symbol_ids_in_file_on_connection(self.connection(), path)
    }

    /// Symbols whose qualified names are requested, using chunked indexed `IN` queries.
    pub fn symbols_by_qualified_names(&self, names: &[String]) -> Result<Vec<Symbol>> {
        symbols_by_values_on_connection(self.connection(), SymbolValueColumn::QualifiedName, names)
    }

    /// Symbols whose simple names are requested, using chunked indexed `IN` queries.
    pub fn symbols_by_names(&self, names: &[String]) -> Result<Vec<Symbol>> {
        symbols_by_values_on_connection(self.connection(), SymbolValueColumn::Name, names)
    }

    /// Hydrate exactly `ids`, by chunked `WHERE id IN (…)` on the primary key.
    /// Replaces `all_symbols()`-then-filter in `store_engine::hydrate`
    /// (Requirement 9.3).
    pub fn hydrate_ids(&self, ids: &[String]) -> Result<Vec<Symbol>> {
        symbols_by_values_on_connection(self.connection(), SymbolValueColumn::Id, ids)
    }

    /// Resolve `needle` through the existing ladder — `id` → `qualified_name` →
    /// `name` → `lower(name)` → substring — executed as ordered indexed queries,
    /// each stopping at the first hit, preserving today's resolution order
    /// (Requirement 9.4, 9.8).
    pub fn resolve_symbol(&self, needle: &str, kind: Option<&str>) -> Result<Option<Symbol>> {
        resolve_symbol_on_connection(self.connection(), needle, kind)
    }

    /// Edges incident to `ids` in `dir`, by indexed predicate on `idx_edge_src`
    /// / `idx_edge_dst`. Replaces the per-hop `list_edges()` scan in
    /// `dependency_trace` (Requirement 9.5).
    pub fn edges_incident(&self, ids: &[String], dir: Direction) -> Result<Vec<Edge>> {
        edges_incident_on_connection(self.connection(), ids, dir)
    }

    /// Lexical FTS5 search inside this snapshot, with the same hit shape and
    /// ordering as [`crate::SymbolStore::fts_search`].
    pub fn fts_search(&self, q: &str, k: usize) -> Result<Vec<Hit>> {
        fts_search_on_connection(self.connection(), q, k)
    }

    /// Vector KNN inside this snapshot, with the same hit shape and ordering as
    /// [`crate::SymbolStore::vec_search`].
    pub fn vec_search(&self, q: &[f32], k: usize) -> Result<Vec<Hit>> {
        vec_search_on_connection(self.connection(), q, k)
    }

    /// The persisted embedding-space identity inside this snapshot, if any.
    ///
    /// `None` when `vector_space` has no row — the `Vector_Space_Guard` treats
    /// that as unverifiable and withholds semantic scoring (Requirement 14.2).
    pub fn vector_space_fingerprint(&self) -> Result<Option<VectorSpaceFingerprint>> {
        match self.connection().query_row(
            "SELECT fingerprint, embedding_dim, model_id FROM vector_space WHERE id = 1",
            [],
            |row| {
                let dim = row.get::<_, i64>(1)?;
                Ok(VectorSpaceFingerprint {
                    digest: row.get(0)?,
                    dim: usize::try_from(dim).unwrap_or(0),
                    model_id: row.get(2)?,
                })
            },
        ) {
            Ok(fingerprint) => Ok(Some(fingerprint)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(error) => Err(store_err(error)),
        }
    }

    /// Count persisted vectors inside this snapshot.
    pub fn vec_row_count(&self) -> Result<usize> {
        vec_row_count_on_connection(self.connection())
    }

    /// Build the resident code graph inside this snapshot rather than on a
    /// fresh connection.
    pub fn build_code_graph(&self, kinds: Option<&[String]>) -> Result<CodeGraph> {
        build_code_graph_on_connection(self.connection(), kinds)
    }
}

impl Drop for GenerationSnapshot<'_> {
    fn drop(&mut self) {
        // Dropping a snapshot must never panic. ROLLBACK releases the read
        // transaction without implying that a read-only call committed work.
        let _ = self.conn.execute_batch("ROLLBACK");
    }
}

#[cfg(test)]
mod tests {
    use cognis_core::{EdgeKind, SymbolKind};

    use super::*;
    use crate::SymbolWriter;

    fn symbol(id: &str, kind: SymbolKind, name: &str, qualified_name: &str, file: &str) -> Symbol {
        Symbol {
            id: id.to_string(),
            kind,
            name: name.to_string(),
            qualified_name: qualified_name.to_string(),
            language: "rust".to_string(),
            module: "fixture".to_string(),
            file_path: file.to_string(),
            line_start: 1,
            line_end: 1,
            signature: None,
            docstring: None,
            content_hash: id.to_string(),
            body_excerpt: None,
            semantic_summary: None,
            risk_score: 0.0,
            ambiguous: false,
            untrusted_flags: Vec::new(),
            updated_at: 1,
        }
    }

    fn edge(src_id: &str, dst_id: &str) -> Edge {
        Edge {
            src_id: src_id.to_string(),
            dst_id: dst_id.to_string(),
            kind: EdgeKind::Calls,
            confidence: 1.0,
            meta: serde_json::Value::Null,
        }
    }

    fn bounded_fixture() -> Database {
        let mut db = Database::new(":memory:");
        db.connect().unwrap();
        db.upsert_symbols(&[
            symbol(
                "a-id",
                SymbolKind::Function,
                "Alpha",
                "fixture.Alpha",
                "src/a.rs",
            ),
            symbol(
                "b-id",
                SymbolKind::Method,
                "alpha",
                "fixture.alpha",
                "src/a.rs",
            ),
            symbol(
                "c-id",
                SymbolKind::Function,
                "alphabet",
                "fixture.alphabet",
                "src/b.rs",
            ),
            symbol(
                "d-id",
                SymbolKind::Function,
                "delta",
                "fixture.alpha_delta",
                "src/b.rs",
            ),
        ])
        .unwrap();
        db.upsert_edges(&[
            edge("a-id", "b-id"),
            edge("b-id", "c-id"),
            edge("c-id", "a-id"),
        ])
        .unwrap();
        db
    }

    fn set_freshness(conn: &Connection, source: i64, semantic: i64, pending: usize) {
        conn.execute(
            "UPDATE generation \
             SET source_generation = ?1, semantic_generation = ?2, updated_at = 1 \
             WHERE id = 1",
            rusqlite::params![source, semantic],
        )
        .unwrap();
        conn.execute("DELETE FROM embedding_queue", []).unwrap();
        for index in 0..pending {
            conn.execute(
                "INSERT INTO embedding_queue( \
                     symbol_id, file_path, enqueued_at, attempts, next_attempt_at, last_error \
                 ) VALUES (?1, ?2, 1, 0, 0, NULL)",
                rusqlite::params![format!("symbol-{index}"), format!("src/{index}.rs")],
            )
            .unwrap();
        }
    }

    #[test]
    fn snapshot_exposes_freshness_from_its_read_transaction() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(dir.path().join("freshness.db")).unwrap();
        set_freshness(&db.connect().unwrap(), 7, 5, 2);

        let snapshot = GenerationSnapshot::open(&db).unwrap();

        assert_eq!(snapshot.source_generation(), 7);
        assert_eq!(snapshot.semantic_generation(), 5);
        assert_eq!(snapshot.pending_vector_count(), 2);
        assert_eq!(snapshot.database().path(), db.path());
        assert!(!snapshot.connection().is_autocommit());

        drop(snapshot);
        db.close_thread_connection();
    }

    #[test]
    fn snapshot_stays_pinned_across_a_later_commit() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(dir.path().join("pinned.db")).unwrap();
        let writer = db.connect().unwrap();
        set_freshness(&writer, 3, 2, 1);
        let snapshot = GenerationSnapshot::open(&db).unwrap();

        set_freshness(&writer, 4, 4, 0);

        assert_eq!(snapshot.source_generation(), 3);
        assert_eq!(snapshot.semantic_generation(), 2);
        assert_eq!(snapshot.pending_vector_count(), 1);
        assert_eq!(
            snapshot
                .connection()
                .query_row(
                    "SELECT source_generation FROM generation WHERE id = 1",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            3,
            "later reads on the snapshot connection must stay at the pinned generation"
        );

        drop(snapshot);
        let current = GenerationSnapshot::open(&db).unwrap();
        assert_eq!(current.source_generation(), 4);
        assert_eq!(current.semantic_generation(), 4);
        assert_eq!(current.pending_vector_count(), 0);
        drop(current);
        drop(writer);
        db.close_thread_connection();
    }

    #[test]
    fn dropping_snapshot_releases_its_transaction() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(dir.path().join("drop.db")).unwrap();
        let snapshot = GenerationSnapshot::open(&db).unwrap();
        let snapshot_conn = Rc::clone(&snapshot.conn);

        assert!(!snapshot_conn.is_autocommit());
        drop(snapshot);
        assert!(
            snapshot_conn.is_autocommit(),
            "Drop must end the deferred read transaction"
        );

        db.close_thread_connection();
    }

    #[test]
    fn in_memory_database_uses_the_existing_database_connection() {
        let db = Database::open(":memory:").unwrap();
        set_freshness(&db.connect().unwrap(), 9, 8, 1);

        let snapshot = GenerationSnapshot::open(&db).unwrap();

        assert_eq!(snapshot.source_generation(), 9);
        assert_eq!(snapshot.semantic_generation(), 8);
        assert_eq!(snapshot.pending_vector_count(), 1);
        drop(snapshot);
        db.close_thread_connection();
    }

    #[test]
    fn bounded_symbol_reads_preserve_id_order_and_deduplicate_requests() {
        let db = bounded_fixture();
        let snapshot = GenerationSnapshot::open(&db).unwrap();

        let in_file: Vec<_> = snapshot
            .symbols_in_file("src/a.rs")
            .unwrap()
            .into_iter()
            .map(|symbol| symbol.id)
            .collect();
        assert_eq!(in_file, ["a-id", "b-id"]);
        assert_eq!(
            snapshot.symbol_ids_in_file("src/b.rs").unwrap(),
            ["c-id", "d-id"]
        );

        let requested = vec![
            "fixture.alphabet".to_string(),
            "fixture.Alpha".to_string(),
            "fixture.Alpha".to_string(),
            "missing".to_string(),
        ];
        let by_qname: Vec<_> = snapshot
            .symbols_by_qualified_names(&requested)
            .unwrap()
            .into_iter()
            .map(|symbol| symbol.id)
            .collect();
        assert_eq!(by_qname, ["a-id", "c-id"]);

        let names = vec!["alpha".to_string(), "Alpha".to_string()];
        let by_name: Vec<_> = snapshot
            .symbols_by_names(&names)
            .unwrap()
            .into_iter()
            .map(|symbol| symbol.id)
            .collect();
        assert_eq!(by_name, ["a-id", "b-id"]);

        let ids = vec![
            "c-id".to_string(),
            "a-id".to_string(),
            "a-id".to_string(),
            "unknown".to_string(),
        ];
        let hydrated: Vec<_> = snapshot
            .hydrate_ids(&ids)
            .unwrap()
            .into_iter()
            .map(|symbol| symbol.id)
            .collect();
        assert_eq!(hydrated, ["a-id", "c-id"]);
    }

    #[test]
    fn resolve_symbol_preserves_the_ordered_ladder_kind_filter_and_ties() {
        let db = bounded_fixture();
        let snapshot = GenerationSnapshot::open(&db).unwrap();

        assert_eq!(
            snapshot.resolve_symbol("b-id", None).unwrap().unwrap().id,
            "b-id"
        );
        assert_eq!(
            snapshot
                .resolve_symbol("fixture.Alpha", None)
                .unwrap()
                .unwrap()
                .id,
            "a-id"
        );
        assert_eq!(
            snapshot.resolve_symbol("alpha", None).unwrap().unwrap().id,
            "b-id"
        );
        assert_eq!(
            snapshot.resolve_symbol("ALPHA", None).unwrap().unwrap().id,
            "a-id"
        );
        assert_eq!(
            snapshot
                .resolve_symbol("alpha", Some("function"))
                .unwrap()
                .unwrap()
                .id,
            "a-id"
        );
        assert_eq!(
            snapshot.resolve_symbol("pha", None).unwrap().unwrap().id,
            "a-id"
        );
        assert_eq!(
            snapshot.resolve_symbol("delta", None).unwrap().unwrap().id,
            "d-id"
        );
        assert!(snapshot
            .resolve_symbol("alpha", Some("class"))
            .unwrap()
            .is_none());
        assert!(snapshot.resolve_symbol("  ", None).unwrap().is_none());
    }

    #[test]
    fn incident_edges_follow_direction_deduplicate_both_and_preserve_order() {
        let db = bounded_fixture();
        let snapshot = GenerationSnapshot::open(&db).unwrap();
        let ids = vec!["a-id".to_string()];

        let out: Vec<_> = snapshot
            .edges_incident(&ids, Direction::Out)
            .unwrap()
            .into_iter()
            .map(|edge| (edge.src_id, edge.dst_id))
            .collect();
        assert_eq!(out, [("a-id".to_string(), "b-id".to_string())]);

        let incoming: Vec<_> = snapshot
            .edges_incident(&ids, Direction::In)
            .unwrap()
            .into_iter()
            .map(|edge| (edge.src_id, edge.dst_id))
            .collect();
        assert_eq!(incoming, [("c-id".to_string(), "a-id".to_string())]);

        let both: Vec<_> = snapshot
            .edges_incident(&ids, Direction::Both)
            .unwrap()
            .into_iter()
            .map(|edge| (edge.src_id, edge.dst_id))
            .collect();
        assert_eq!(
            both,
            [
                ("a-id".to_string(), "b-id".to_string()),
                ("c-id".to_string(), "a-id".to_string()),
            ]
        );
    }

    #[test]
    fn snapshot_searches_match_database_search_semantics() {
        let mut db = bounded_fixture();
        db.upsert_embeddings(&[("a-id".to_string(), vec![1.0, 0.0])])
            .unwrap();
        let expected_fts = db.fts_search("Alpha", 10).unwrap();
        let expected_vec = db.vec_search(&[1.0, 0.0], 10).unwrap();
        let snapshot = GenerationSnapshot::open(&db).unwrap();

        assert_eq!(snapshot.fts_search("Alpha", 10).unwrap(), expected_fts);
        assert_eq!(snapshot.vec_search(&[1.0, 0.0], 10).unwrap(), expected_vec);
        assert!(snapshot.fts_search("  ", 10).unwrap().is_empty());
        assert!(snapshot.vec_search(&[], 10).unwrap().is_empty());
    }

    #[test]
    fn direction_round_trips_the_tools_wire_strings() {
        for dir in [Direction::Out, Direction::In, Direction::Both] {
            assert_eq!(Direction::parse_str(dir.as_str()), Some(dir));
        }
        // Exactly the three values `dependency_trace` accepts today.
        assert_eq!(Direction::Out.as_str(), "out");
        assert_eq!(Direction::In.as_str(), "in");
        assert_eq!(Direction::Both.as_str(), "both");
        assert_eq!(Direction::parse_str("outbound"), None);
        assert_eq!(Direction::parse_str("Out"), None);
        assert_eq!(Direction::parse_str(""), None);
    }

    #[test]
    fn direction_follow_predicates_match_the_traversal_arms() {
        assert!(Direction::Out.follows_outbound() && !Direction::Out.follows_inbound());
        assert!(Direction::In.follows_inbound() && !Direction::In.follows_outbound());
        assert!(Direction::Both.follows_outbound() && Direction::Both.follows_inbound());
    }
}
