//! cognis-store — UCKG access (rusqlite, bundled SQLite).
//!
//! Task 3.1 lands the connection layer + migration runner:
//!
//! - A [`Database`] handle opens a bundled-SQLite connection in **WAL** mode
//!   with a **per-thread** connection cache (mirrors the Python
//!   `cognis.db.Database` thread-local), so the engine never shares a raw
//!   connection across threads.
//! - [`run_migrations`] applies the numbered `NNN_*.sql` migrations — exactly
//!   the existing `001_initial.sql` schema — driven by `meta.schema_version`,
//!   and is idempotent: opening a `.cognis/uckg.db` already built by the Python
//!   engine (already at the latest schema version) applies **nothing**.
//!
//! The UCKG schema is immutable (Requirement 2): `001_initial.sql` is a verbatim
//! copy of `packages/core/cognis/migrations/001_initial.sql`. Any future schema
//! change ships as a new numbered migration — never an edit to 001. Table,
//! column and index names, WAL mode, the `meta.dst_missing` convention and the
//! `node_id` format (`<lang>:<path>:<qname>@<hash>`) are all preserved so a
//! Python-built DB reads back unchanged (Requirement 2.1, 2.2).
//!
//! Read/write traits (`SymbolStore`/`SymbolWriter`), FTS5 / sqlite-vec search
//! and the resident CSR builder land in tasks 3.2–3.5. This module exposes the
//! minimal read helpers needed to prove schema compatibility.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::rc::Rc;

use cognis_core::{CognisError, Edge, EdgeKind, Symbol, SymbolKind};
use rusqlite::Connection;

pub use cognis_core::{CodeGraph, Hit, Result};

/// Per-connection `busy_timeout` (ms) — mirrors Python `BUSY_TIMEOUT_MS`.
const BUSY_TIMEOUT_MS: u32 = 5000;

/// The highest migration version shipped by this crate. A DB whose
/// `meta.schema_version` already equals this gets a no-op `run_migrations`.
pub const LATEST_SCHEMA_VERSION: i64 = 1;

/// Default vector dimensionality for a fresh DB (`bge-small-en-v1.5`, 384-d) —
/// mirrors Python `cognis.db.EMBEDDING_DIM`. The *active* dimension is persisted
/// in `meta.embedding_dim` and can differ when a higher-dim model is plugged in
/// (see [`SymbolWriter::reconcile_embedding_dim`]).
pub const DEFAULT_EMBEDDING_DIM: usize = 384;

/// `meta` key under which the active vector dimension is persisted (mirrors
/// Python `cognis.db.EMBEDDING_DIM_META_KEY`).
const EMBEDDING_DIM_META_KEY: &str = "embedding_dim";

/// Embedded migrations, ordered by ascending version. The SQL is a verbatim
/// copy of the Python engine's migration set (Requirement 2). Add new entries
/// here for new numbered migrations; never edit an existing one.
const MIGRATIONS: &[(i64, &str)] = &[(1, include_str!("../migrations/001_initial.sql"))];

fn store_err<E: std::fmt::Display>(e: E) -> CognisError {
    CognisError::Store(e.to_string())
}

/// Read surface over a UCKG database (retrieval + MCP).
///
/// Mirrors the Python retrieval layers' view of the store: dependency-neutral
/// primitives returning hydrated [`Hit`]s / models, with no query rewriting or
/// fusion (those live in `cognis-retrieval`). Task 3.2 landed [`fts_search`] and
/// Task 3.3 lands [`vec_search`]; `neighbors`, `build_code_graph` and `hydrate`
/// follow in tasks 3.4–3.5.
///
/// [`fts_search`]: SymbolStore::fts_search
/// [`vec_search`]: SymbolStore::vec_search
pub trait SymbolStore {
    /// Lexical FTS5 search: top-`k` hits for the FTS5 MATCH query `q`.
    ///
    /// `q` is a raw FTS5 query string (e.g. `"validate"` or `"jwt OR auth"`),
    /// not natural language — query rewriting is the `cognis-retrieval`
    /// lexical layer's job (Task 5.1). Returns hydrated [`Hit`]s ordered best
    /// first (descending score), where `score = -rank` inverts FTS5's negative
    /// BM25 rank so higher is better (mirrors `lexical.py`). A blank query, or
    /// an FTS5 syntax / missing-table error, degrades to an empty result rather
    /// than an error (graceful degradation, design Error Handling).
    fn fts_search(&self, q: &str, k: usize) -> Result<Vec<Hit>>;

    /// Semantic KNN search: top-`k` hits nearest the query embedding `q`.
    ///
    /// `q` is a pre-computed query embedding (embedding is `cognis-embed`'s
    /// job, Task 6); the store only does the nearest-neighbour search. Hits are
    /// ordered nearest first (ascending cosine distance ⇒ descending score),
    /// where `score = max(0, 1 - distance)` and `evidence = {"score": distance}`
    /// — mirroring the Python `SemanticLayer` field-for-field (Requirement 4.2).
    ///
    /// Two execution paths (design Data Models → Embeddings, Error Handling):
    ///
    /// * **Primary** — when `symbol_vec` is a sqlite-vec `vec0` virtual table
    ///   and the extension is loadable, run the same `embedding MATCH ? AND
    ///   k = ?` KNN the Python layer issues.
    /// * **Fallback** (Requirement 2.4) — when sqlite-vec cannot be loaded
    ///   (`symbol_vec` is the plain-BLOB table), read the BLOB vectors and do a
    ///   cosine linear scan in Rust. Graceful degradation, never panics.
    ///
    /// A blank query (`q` empty), `k == 0`, or a `vec0` table with no loadable
    /// extension degrade to an empty result rather than an error.
    fn vec_search(&self, q: &[f32], k: usize) -> Result<Vec<Hit>>;

    /// Build the resident CSR [`CodeGraph`] CSAR diffuses over (Requirement
    /// 4.3, Property 2).
    ///
    /// Mirrors the Python `cognis_retrieval.csar.build_code_graph` oracle
    /// operation-for-operation so the two builders agree to within the CSAR L1
    /// tolerance on the same DB:
    ///
    /// * **Nodes** are every row of `symbol`, in the engine's natural
    ///   `SELECT id FROM symbol` order (so node index ↔ symbol id matches the
    ///   oracle exactly). `index` is the inverse map.
    /// * **Edges** are `edge` rows, *dropping* any flagged
    ///   `meta.dst_missing = true` (the structural-traversal filter, design
    ///   `meta.dst_missing` convention) and any whose endpoints are not both in
    ///   the node set, plus self-edges (`src == dst`) and non-positive weights.
    ///   `kinds`, when `Some`, is an edge-**kind** whitelist (mirrors the Python
    ///   `edge_kinds` argument); `None` includes every kind.
    /// * The graph is **symmetrized** (undirected) and parallel edges between a
    ///   pair sum their `confidence` weights, so diffusion reaches both callers
    ///   and callees of a seed.
    /// * Each row's neighbour indices are **sorted ascending** (matching the
    ///   oracle's `sorted(neighbors.items())` push order); `degree[u]` is the
    ///   weighted column sum.
    /// * An **isolated** node gets a single self-loop `(u, 1.0)` with
    ///   `degree 1.0`, keeping the transition matrix column-stochastic.
    ///
    /// The result is CSR: `indptr` (`n + 1`), `indices`/`weights` (`nnz`),
    /// `degree` (`n`), and the `node_ids` / `index` boundary maps.
    fn build_code_graph(&self, kinds: Option<&[String]>) -> Result<CodeGraph>;
}

/// Write surface over a UCKG database (indexer Writer + daemon).
///
/// Mirrors the Python `cognis.db` write helpers (`upsert_symbols`,
/// `upsert_edges`, `delete_symbol`, `Database.reconcile_embedding_dim`). Every
/// method runs under a single `BEGIN IMMEDIATE` transaction (the design's
/// per-file/per-batch "writer txn"): all rows of a batch commit atomically, or
/// the batch rolls back on the first error. The `&mut self` receiver marks
/// these as the write half of the read/write split — read traffic uses the
/// `&self` [`SymbolStore`] surface concurrently under WAL.
pub trait SymbolWriter {
    /// Insert or replace `symbols` and keep `symbol_fts` in sync.
    ///
    /// `symbol` rows upsert on the `id` primary key (`ON CONFLICT(id) DO
    /// UPDATE` — mirrors `cognis.db.upsert_symbols`), so a caller can pass the
    /// full set for a file and trust the end-state. `symbol_fts` is a
    /// *contentless* FTS5 table with no FK to `symbol` and no triggers (design
    /// Indexer Pipeline → Writer keeps the sync explicit), so for each symbol
    /// its stale FTS row is deleted and a fresh one inserted in the same
    /// transaction — exactly one FTS row per symbol id, no orphan accumulation.
    /// FTS column values mirror `lexical.populate_fts` (`signature`/`docstring`/
    /// `body_excerpt` default to `""`). An empty slice is a no-op.
    fn upsert_symbols(&mut self, symbols: &[Symbol]) -> Result<()>;

    /// Insert or replace `edges` (`ON CONFLICT(src_id, dst_id, kind) DO
    /// UPDATE` — mirrors `cognis.db.upsert_edges`). `meta` serialises to a JSON
    /// string, or `NULL` when it is null / an empty object (matching the
    /// Python `json.dumps(e.meta) if e.meta else None`). An empty slice is a
    /// no-op.
    fn upsert_edges(&mut self, edges: &[Edge]) -> Result<()>;

    /// Delete the symbol `id` and apply the application-layer FK cascade.
    ///
    /// Mirrors `cognis.db.delete_symbol` (design Property 3 / CP-3):
    ///
    /// 1. Delete the `symbol` row — this cascades `symbol_attribute` and
    ///    `symbol_vec` via their `ON DELETE CASCADE` FKs (`foreign_keys=ON`).
    /// 2. Delete the contentless `symbol_fts` row for `id` (no FK / trigger, so
    ///    the writer clears it explicitly — keeps lexical search in sync).
    /// 3. Delete every **outbound** edge `(id, *, *)` — the source is gone, so
    ///    these are unrecoverable.
    /// 4. **Keep** every **inbound** edge `(*, id, *)` but flag it
    ///    `meta.dst_missing = true` (JSON boolean), preserving the audit trail.
    ///    Structural traversal filters on this flag rather than erasing history.
    ///
    /// Deleting an absent id is a no-op for the symbol row; the edge/FTS steps
    /// still run harmlessly (matching the Python oracle).
    fn delete_symbol(&mut self, id: &str) -> Result<()>;

    /// Reconcile the persisted vector dimension + `symbol_vec` table to `dim`.
    ///
    /// Mirrors `cognis.db.Database.reconcile_embedding_dim` (Requirement 2.3):
    /// when `dim` differs from the dimension stored in `meta.embedding_dim` (or
    /// from an existing `vec0` table's `FLOAT[n]` width) — i.e. a model with a
    /// new vector size was plugged in — persist the new dimension and **drop +
    /// recreate** `symbol_vec` at `dim`, discarding now-invalid vectors. They
    /// are regenerated on the next index pass (re-embed is idempotent). A
    /// `vec0` table is recreated as `vec0(... FLOAT[dim])`; the plain-BLOB
    /// fallback is recreated with the same shape as migration 001. Idempotent:
    /// when the stored dimension already equals `dim` and the table form is
    /// consistent, nothing is written.
    fn reconcile_embedding_dim(&mut self, dim: usize) -> Result<()>;

    /// Persist symbol embeddings into `symbol_vec` (upsert by `symbol_id`).
    ///
    /// Each `(symbol_id, vector)` pair is written as the little-endian `f32`
    /// BLOB layout sqlite-vec and the Python writer share (`struct.pack`). The
    /// write is an idempotent replace (delete-then-insert under the batch
    /// transaction), so re-embedding a symbol overwrites its prior vector and
    /// no duplicate rows accumulate — the same end-state guarantee
    /// [`upsert_symbols`] gives for the symbol table.
    ///
    /// The caller is responsible for reconciling `symbol_vec` to the embedder's
    /// dimension first ([`reconcile_embedding_dim`]); vectors must all match the
    /// table's active dimension. An empty slice is a no-op. Rows whose
    /// `symbol_id` is not (yet) a `symbol` row are skipped in the BLOB-table
    /// form (the FK would reject them) — semantic rows only exist for real
    /// symbols.
    ///
    /// [`upsert_symbols`]: SymbolWriter::upsert_symbols
    /// [`reconcile_embedding_dim`]: SymbolWriter::reconcile_embedding_dim
    fn upsert_embeddings(&mut self, rows: &[(String, Vec<f32>)]) -> Result<()>;
}

thread_local! {
    /// Per-thread `{abs_path -> connection}` cache. SQLite connections are not
    /// `Send`; caching per thread gives each thread its own connection without
    /// cross-thread sharing (mirrors the Python `threading.local` cache). The OS
    /// reaps connections when the thread exits.
    static CONNECTIONS: RefCell<HashMap<String, Rc<Connection>>> = RefCell::new(HashMap::new());
}

/// A handle binding a UCKG database path to its connection policy.
///
/// Construction is cheap and does not open a connection; the first
/// [`Database::connect`] on a given thread opens one, applies the WAL pragmas,
/// runs migrations, and caches it for that thread. Cloning a `Database` is cheap
/// (it only clones the path string) and clones share the per-thread cache keyed
/// by path.
#[derive(Debug, Clone)]
pub struct Database {
    path: String,
}

impl Database {
    /// Bind to the database at `path` (created on first connect if absent).
    ///
    /// `":memory:"` is supported for tests; note each thread gets its *own*
    /// in-memory database because that is how SQLite scopes memory DBs.
    pub fn new<P: AsRef<Path>>(path: P) -> Self {
        Database {
            path: path.as_ref().to_string_lossy().into_owned(),
        }
    }

    /// Bind to `path` and eagerly open a connection (running migrations) so any
    /// schema / IO error surfaces immediately rather than on first query.
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let db = Database::new(path);
        db.connect()?;
        Ok(db)
    }

    /// The bound database path.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Return this thread's cached connection, opening and migrating one on the
    /// first call. The returned [`Rc`] keeps the connection alive for the
    /// caller; the cache holds its own clone for reuse.
    pub fn connect(&self) -> Result<Rc<Connection>> {
        CONNECTIONS.with(|cell| {
            if let Some(conn) = cell.borrow().get(&self.path) {
                return Ok(Rc::clone(conn));
            }
            let conn = Rc::new(self.open_new_connection()?);
            cell.borrow_mut()
                .insert(self.path.clone(), Rc::clone(&conn));
            Ok(conn)
        })
    }

    /// Close and drop this thread's cached connection, if any. Test fixtures use
    /// this to release the file handle between cases (notably on Windows, where
    /// SQLite holds the file open until the connection is dropped).
    pub fn close_thread_connection(&self) {
        CONNECTIONS.with(|cell| {
            cell.borrow_mut().remove(&self.path);
        });
    }

    /// Open a fresh connection, apply the design's pragmas, and migrate.
    fn open_new_connection(&self) -> Result<Connection> {
        let conn = Connection::open(&self.path).map_err(store_err)?;

        // WAL persists in the file header; re-issuing per connection is the
        // documented idempotent pattern. `synchronous=NORMAL` is the
        // WAL-recommended pairing; `foreign_keys=ON` matches the Python factory
        // (symbol_attribute / symbol_vec cascade on delete).
        conn.busy_timeout(std::time::Duration::from_millis(BUSY_TIMEOUT_MS as u64))
            .map_err(store_err)?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;\n\
             PRAGMA synchronous = NORMAL;\n\
             PRAGMA foreign_keys = ON;\n\
             PRAGMA temp_store = MEMORY;",
        )
        .map_err(store_err)?;

        run_migrations(&conn)?;
        // Self-heal a legacy `vec0` `symbol_vec` on the shared open path so every
        // surface (health/cli/indexd) applies the same outcome. Detection is
        // schema-only and the heal is a guarded no-op on non-legacy DBs, so this
        // is safe and idempotent for all inputs.
        //
        // Graceful-ignore fallback (indexd-vec0-legacy-crash Req 2.1, 2.5): the
        // heal writes to the DB, so it can fail when the DB is read-only
        // (`attempt to write a readonly database`) or the write lock can't be
        // taken. In that case the open path MUST still succeed — we log and
        // ignore the heal error and return `Ok(conn)`. Downstream reads that
        // touch `symbol_vec` rows degrade to empty (see `vec_search`,
        // `vec_symbol_ids`, `vec_row_count`) rather than propagate a fatal
        // `vec0` error, and a later rebuild on a writable DB repopulates BLOB
        // vectors.
        if let Err(e) = heal_legacy_vec0(&conn) {
            eprintln!(
                "cognis-store: legacy vec0 self-heal skipped for {} ({e}); \
                 continuing with graceful degradation",
                self.path
            );
        }
        // Reconcile-on-open (safe index self-recovery): stamp the engine build
        // and, ONLY when the DB was written by a strictly-newer engine
        // (`schema_version > LATEST_SCHEMA_VERSION`), perform a SAFE in-place
        // index reset so the extension never dead-ends on a genuinely
        // incompatible DB. Like the heal above it is best-effort: on a
        // read-only DB / lock it logs and returns Ok so open still succeeds
        // (a normal DB at schema <= LATEST is never reset).
        if let Err(e) = reconcile_on_open(&conn) {
            eprintln!(
                "cognis-store: reconcile-on-open skipped for {} ({e}); \
                 continuing with graceful degradation",
                self.path
            );
        }
        Ok(conn)
    }

    // ------------------------------------------------------------------
    // Minimal read surface (compatibility test; full SymbolStore = task 3.2+)
    // ------------------------------------------------------------------

    /// The DB's current `meta.schema_version` (0 when unset / absent).
    pub fn schema_version(&self) -> Result<i64> {
        let conn = self.connect()?;
        Ok(read_meta(&conn, "schema_version")?
            .and_then(|v| v.parse().ok())
            .unwrap_or(0))
    }

    /// Row count of `table` (read-path smoke check).
    pub fn count(&self, table: &str) -> Result<i64> {
        let conn = self.connect()?;
        // Table name is from a fixed internal allow-list at the call sites; it
        // cannot be parameterized in SQL, so guard against injection here.
        if !table.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err(CognisError::Store(format!("illegal table name {table:?}")));
        }
        conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .map_err(store_err)
    }

    /// Read every symbol back into the core [`Symbol`] model, in `id` order.
    /// Exercises the full `symbol` column mapping against the live schema.
    pub fn list_symbols(&self) -> Result<Vec<Symbol>> {
        let conn = self.connect()?;
        let mut stmt = conn
            .prepare(
                "SELECT id, kind, name, qualified_name, language, module, file_path, \
                 line_start, line_end, signature, docstring, content_hash, body_excerpt, \
                 semantic_summary, risk_score, ambiguous, untrusted_flags, updated_at \
                 FROM symbol ORDER BY id",
            )
            .map_err(store_err)?;
        let rows = stmt
            .query_map([], row_to_symbol)
            .map_err(store_err)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(store_err)?;
        Ok(rows)
    }

    /// Read every edge back into the core [`Edge`] model, ordered deterministically.
    pub fn list_edges(&self) -> Result<Vec<Edge>> {
        let conn = self.connect()?;
        let mut stmt = conn
            .prepare(
                "SELECT src_id, dst_id, kind, confidence, meta FROM edge \
                 ORDER BY src_id, dst_id, kind",
            )
            .map_err(store_err)?;
        let rows = stmt
            .query_map([], row_to_edge)
            .map_err(store_err)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(store_err)?;
        Ok(rows)
    }

    /// Minimal FTS5 lookup: symbol ids matching `query`, best rank first.
    ///
    /// This is the read-path smoke check for `symbol_fts`; the parity-tested
    /// `SymbolStore::fts_search` (hydrated [`Hit`]s, BM25 tuning) lands in 3.2.
    pub fn fts_match_ids(&self, query: &str, k: usize) -> Result<Vec<String>> {
        let conn = self.connect()?;
        let mut stmt = conn
            .prepare(
                "SELECT id FROM symbol_fts WHERE symbol_fts MATCH ?1 \
                 ORDER BY rank LIMIT ?2",
            )
            .map_err(store_err)?;
        let rows = stmt
            .query_map(rusqlite::params![query, k as i64], |r| {
                r.get::<_, String>(0)
            })
            .map_err(store_err)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(store_err)?;
        Ok(rows)
    }

    /// Symbol ids present in `symbol_vec`, in `symbol_id` order.
    ///
    /// Works against the plain-BLOB fallback `symbol_vec` table, and against a
    /// `vec0` virtual table when the sqlite-vec extension is loadable.
    ///
    /// Graceful degradation (indexd-vec0-legacy-crash Req 2.5): when
    /// `symbol_vec` is a legacy `vec0` virtual table this build cannot read
    /// (e.g. the heal-on-open couldn't run because the DB is read-only), any
    /// live query raises `no such module: vec0`. Rather than propagate that
    /// fatal error, degrade to an empty result — matching `vec_search`'s
    /// "degrade to empty" contract so `check_vector`'s warn mapping keeps
    /// working (a legacy DB reads as "no vectors" ⇒ `warn`, never a crash).
    pub fn vec_symbol_ids(&self) -> Result<Vec<String>> {
        let conn = self.connect()?;
        // Unreadable legacy `vec0` table → empty. `try_load_vec_extension`
        // both probes for and (under the `sqlite-vec` feature) loads the
        // extension, so if it *is* loadable we fall through and the query below
        // succeeds against the `vec0` table.
        if vec0_table_present(&conn)? && !try_load_vec_extension(&conn) {
            return Ok(Vec::new());
        }
        let mut stmt = conn
            .prepare("SELECT symbol_id FROM symbol_vec ORDER BY symbol_id")
            .map_err(store_err)?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(store_err)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(store_err)?;
        Ok(rows)
    }

    /// Number of vectors currently persisted in `symbol_vec`.
    ///
    /// Cheap availability probe for the semantic leg: the server treats a
    /// populated `symbol_vec` (count > 0) as "semantic index available".
    /// Works against both the plain-BLOB fallback and a `vec0` virtual table.
    /// A missing table degrades to `0` rather than erroring.
    ///
    /// Graceful degradation (indexd-vec0-legacy-crash Req 2.5): a legacy `vec0`
    /// table this build cannot read (e.g. the heal-on-open was skipped on a
    /// read-only DB) raises `no such module: vec0` on `COUNT(*)`. Degrade that
    /// to `0` — consistent with `vec_search` / `vec_symbol_ids` — so callers
    /// treating a populated `symbol_vec` as "semantic available" simply see it
    /// as empty rather than crashing.
    pub fn vec_row_count(&self) -> Result<usize> {
        let conn = self.connect()?;
        if vec0_table_present(&conn)? && !try_load_vec_extension(&conn) {
            return Ok(0);
        }
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM symbol_vec", [], |r| r.get(0))
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(0),
                other => Err(other),
            })
            .map_err(store_err)?;
        Ok(n.max(0) as usize)
    }
}

// ---------------------------------------------------------------------------
// SymbolStore — read surface (task 3.2: fts_search)
// ---------------------------------------------------------------------------

impl SymbolStore for Database {
    fn fts_search(&self, q: &str, k: usize) -> Result<Vec<Hit>> {
        // Empty / whitespace-only query → no work, no hits (mirror lexical.py's
        // early return on an empty rewritten query).
        if q.trim().is_empty() || k == 0 {
            return Ok(Vec::new());
        }

        let conn = self.connect()?;

        // One query joins the FTS match to its `symbol` row so a hit's reason
        // can name the qualified_name (LEFT JOIN: an FTS row whose symbol is
        // absent during an incremental-index gap still returns, unhydrated —
        // matching the Python layer's two-tier behaviour). `snippet()` mirrors
        // `lexical.py`: column 1 (`name`), «»/… marks, 20-token window.
        let mut stmt = conn
            .prepare(
                "SELECT f.id, \
                        snippet(symbol_fts, 1, '«', '»', '…', 20) AS snip, \
                        f.rank, \
                        s.qualified_name \
                 FROM symbol_fts f \
                 LEFT JOIN symbol s ON s.id = f.id \
                 WHERE symbol_fts MATCH ?1 \
                 ORDER BY rank \
                 LIMIT ?2",
            )
            .map_err(store_err)?;

        let mapped = stmt.query_map(rusqlite::params![q, k as i64], |row| {
            let id: String = row.get("id")?;
            let snippet: Option<String> = row.get("snip")?;
            // FTS5 rank is negative (lower = more relevant); invert so higher
            // score = better, matching the engine-wide Hit convention.
            let rank: f64 = row.get::<_, Option<f64>>("rank")?.unwrap_or(0.0);
            let qualified_name: Option<String> = row.get("qualified_name")?;
            Ok((id, snippet, -rank, qualified_name))
        });

        // Graceful degradation: a malformed FTS5 query or a missing
        // `symbol_fts` table yields an empty result rather than an error
        // (mirrors the `sqlite3.OperationalError` catch in lexical.py).
        let rows = match mapped {
            Ok(iter) => iter,
            Err(_) => return Ok(Vec::new()),
        };

        let mut hits = Vec::new();
        for row in rows {
            let (symbol_id, snippet, score, qualified_name) = match row {
                Ok(r) => r,
                Err(_) => return Ok(Vec::new()),
            };
            let reason = match &qualified_name {
                Some(qn) => format!("FTS5 BM25 match: {qn}"),
                None => "FTS5 BM25 match (symbol row not found)".to_string(),
            };
            let evidence = serde_json::json!({ "snippet": snippet.unwrap_or_default() });
            hits.push(Hit {
                symbol_id,
                score,
                layer: "lexical".to_string(),
                reason,
                evidence,
            });
        }
        Ok(hits)
    }

    fn vec_search(&self, q: &[f32], k: usize) -> Result<Vec<Hit>> {
        // Empty query / k == 0 → no work, no hits (mirror semantic.py's early
        // returns).
        if q.is_empty() || k == 0 {
            return Ok(Vec::new());
        }

        let conn = self.connect()?;

        // The store chooses path by what `symbol_vec` actually *is* on disk
        // (idempotent, decided at query time — design Data Models): a `vec0`
        // virtual table → sqlite-vec KNN; the plain-BLOB table → linear scan.
        if vec0_table_present(&conn)? {
            // Primary path. KNN needs the sqlite-vec extension loaded into this
            // connection; attempt a best-effort load (no-op unless the
            // `sqlite-vec` feature is on). If the table is `vec0` but the
            // extension can't be loaded, the MATCH query errors and we degrade
            // to an empty result — we can't BLOB-scan a vec0 table (Req 2.4).
            let _ = try_load_vec_extension(&conn);
            return Ok(vec0_knn(&conn, q, k).unwrap_or_default());
        }

        // Fallback path (Requirement 2.4): plain-BLOB `symbol_vec` + cosine
        // linear scan in Rust. Never panics; a corrupt/short BLOB row is
        // skipped rather than aborting the scan.
        blob_linear_scan(&conn, q, k)
    }

    fn build_code_graph(&self, kinds: Option<&[String]>) -> Result<CodeGraph> {
        let conn = self.connect()?;

        // Nodes = every symbol row in the natural `SELECT id FROM symbol`
        // order, mirroring the Python oracle exactly so node index ↔ symbol id
        // lines up (parity is order-sensitive — the kernel indexes by i32).
        let node_ids: Vec<String> = {
            let mut stmt = conn.prepare("SELECT id FROM symbol").map_err(store_err)?;
            let rows = stmt
                .query_map([], |r| r.get::<_, String>(0))
                .map_err(store_err)?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(store_err)?;
            rows
        };
        let n = node_ids.len();
        let mut index: HashMap<String, usize> = HashMap::with_capacity(n);
        for (i, id) in node_ids.iter().enumerate() {
            index.insert(id.clone(), i);
        }

        // Optional edge-**kind** whitelist (mirrors Python `edge_kinds`).
        let kind_filter: Option<HashSet<&str>> =
            kinds.map(|ks| ks.iter().map(String::as_str).collect());

        // Per-node sorted accumulator: `BTreeMap` coalesces parallel edges and
        // keeps neighbours ascending so the CSR row order matches the oracle's
        // `sorted(neighbors.items())`. Weights accumulate in edge-row order on
        // both sides, so the summed values agree.
        let mut acc: Vec<BTreeMap<i32, f64>> = vec![BTreeMap::new(); n];

        {
            // Same projection as the oracle: `json_extract` of the JSON boolean
            // `meta.dst_missing` yields 1/0, `COALESCE`d to 0 when absent.
            let mut stmt = conn
                .prepare(
                    "SELECT src_id, dst_id, kind, confidence, \
                     COALESCE(json_extract(meta, '$.dst_missing'), 0) AS dst_missing \
                     FROM edge",
                )
                .map_err(store_err)?;
            let rows = stmt
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,      // src_id
                        r.get::<_, String>(1)?,      // dst_id
                        r.get::<_, String>(2)?,      // kind
                        r.get::<_, Option<f64>>(3)?, // confidence
                        r.get::<_, Option<i64>>(4)?, // dst_missing
                    ))
                })
                .map_err(store_err)?;

            for row in rows {
                let (src_id, dst_id, kind, confidence, dst_missing) = row.map_err(store_err)?;
                // Drop edges into deleted symbols (audit trail kept in the DB).
                if dst_missing.unwrap_or(0) == 1 {
                    continue;
                }
                if let Some(filter) = &kind_filter {
                    if !filter.contains(kind.as_str()) {
                        continue;
                    }
                }
                // Skip dangling endpoints and self-edges (self-loops are only
                // added below for genuinely isolated nodes).
                let (Some(&u), Some(&v)) = (index.get(&src_id), index.get(&dst_id)) else {
                    continue;
                };
                if u == v {
                    continue;
                }
                let w = confidence.unwrap_or(1.0);
                if w <= 0.0 {
                    continue;
                }
                // Symmetrize: both directions get the same accumulated weight.
                *acc[u].entry(v as i32).or_insert(0.0) += w;
                *acc[v].entry(u as i32).or_insert(0.0) += w;
            }
        }

        // Flatten the accumulator into CSR arrays.
        let mut indptr: Vec<i32> = Vec::with_capacity(n + 1);
        indptr.push(0);
        let mut indices: Vec<i32> = Vec::new();
        let mut weights: Vec<f64> = Vec::new();
        let mut degree: Vec<f64> = Vec::with_capacity(n);

        for (u, neighbors) in acc.iter().enumerate() {
            if neighbors.is_empty() {
                // Isolated node: self-loop keeps P column-stochastic (mass
                // stays put), degree 1.0 — mirrors the oracle.
                indices.push(u as i32);
                weights.push(1.0);
                degree.push(1.0);
            } else {
                let mut d = 0.0f64;
                for (&v, &w) in neighbors.iter() {
                    indices.push(v);
                    weights.push(w);
                    d += w;
                }
                degree.push(d);
            }
            indptr.push(indices.len() as i32);
        }

        Ok(CodeGraph {
            indptr,
            indices,
            weights,
            degree,
            node_ids,
            index,
        })
    }
}

// ---------------------------------------------------------------------------
// SymbolWriter — write surface (task 3.4)
// ---------------------------------------------------------------------------

/// `symbol` columns in INSERT order — mirrors `cognis.db._SYMBOL_COLUMNS`.
const SYMBOL_COLUMNS: &[&str] = &[
    "id",
    "kind",
    "name",
    "qualified_name",
    "language",
    "module",
    "file_path",
    "line_start",
    "line_end",
    "signature",
    "docstring",
    "content_hash",
    "body_excerpt",
    "semantic_summary",
    "risk_score",
    "ambiguous",
    "untrusted_flags",
    "updated_at",
];

impl SymbolWriter for Database {
    fn upsert_symbols(&mut self, symbols: &[Symbol]) -> Result<()> {
        if symbols.is_empty() {
            return Ok(());
        }
        let conn = self.connect()?;
        with_write_txn(&conn, |conn| {
            let placeholders = vec!["?"; SYMBOL_COLUMNS.len()].join(", ");
            let update_clause = SYMBOL_COLUMNS
                .iter()
                .filter(|col| **col != "id")
                .map(|col| format!("{col} = excluded.{col}"))
                .collect::<Vec<_>>()
                .join(", ");
            let upsert_sql = format!(
                "INSERT INTO symbol ({}) VALUES ({placeholders}) \
                 ON CONFLICT(id) DO UPDATE SET {update_clause}",
                SYMBOL_COLUMNS.join(", "),
            );

            let mut upsert = conn.prepare(&upsert_sql).map_err(store_err)?;
            // FTS sync: contentless table, no FK/trigger → delete stale + insert
            // fresh per symbol so there is exactly one row per id.
            let mut fts_delete = conn
                .prepare("DELETE FROM symbol_fts WHERE id = ?1")
                .map_err(store_err)?;
            let mut fts_insert = conn
                .prepare(
                    "INSERT INTO symbol_fts (id, name, qualified_name, signature, \
                     docstring, body_excerpt) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                )
                .map_err(store_err)?;

            for s in symbols {
                let kind = symbol_kind_to_db(s.kind)?;
                // Empty flags → NULL; else a JSON array string (mirror Python's
                // `json.dumps(flags) if flags else None`).
                let untrusted_flags = if s.untrusted_flags.is_empty() {
                    None
                } else {
                    Some(serde_json::to_string(&s.untrusted_flags).map_err(store_err)?)
                };
                upsert
                    .execute(rusqlite::params![
                        s.id,
                        kind,
                        s.name,
                        s.qualified_name,
                        s.language,
                        s.module,
                        s.file_path,
                        s.line_start,
                        s.line_end,
                        s.signature,
                        s.docstring,
                        s.content_hash,
                        s.body_excerpt,
                        s.semantic_summary,
                        s.risk_score,
                        i64::from(s.ambiguous),
                        untrusted_flags,
                        s.updated_at,
                    ])
                    .map_err(store_err)?;

                fts_delete
                    .execute(rusqlite::params![s.id])
                    .map_err(store_err)?;
                fts_insert
                    .execute(rusqlite::params![
                        s.id,
                        s.name,
                        s.qualified_name,
                        s.signature.as_deref().unwrap_or(""),
                        s.docstring.as_deref().unwrap_or(""),
                        s.body_excerpt.as_deref().unwrap_or(""),
                    ])
                    .map_err(store_err)?;
            }
            Ok(())
        })
    }

    fn upsert_edges(&mut self, edges: &[Edge]) -> Result<()> {
        if edges.is_empty() {
            return Ok(());
        }
        let conn = self.connect()?;
        with_write_txn(&conn, |conn| {
            let mut stmt = conn
                .prepare(
                    "INSERT INTO edge (src_id, dst_id, kind, confidence, meta) \
                     VALUES (?1, ?2, ?3, ?4, ?5) \
                     ON CONFLICT(src_id, dst_id, kind) DO UPDATE SET \
                     confidence = excluded.confidence, meta = excluded.meta",
                )
                .map_err(store_err)?;
            for e in edges {
                let kind = edge_kind_to_db(e.kind)?;
                let meta = edge_meta_to_db(&e.meta)?;
                stmt.execute(rusqlite::params![
                    e.src_id,
                    e.dst_id,
                    kind,
                    e.confidence,
                    meta,
                ])
                .map_err(store_err)?;
            }
            Ok(())
        })
    }

    fn delete_symbol(&mut self, id: &str) -> Result<()> {
        let conn = self.connect()?;
        with_write_txn(&conn, |conn| {
            // 1. symbol row — cascades symbol_attribute + symbol_vec via FK.
            conn.execute("DELETE FROM symbol WHERE id = ?1", rusqlite::params![id])
                .map_err(store_err)?;
            // 2. contentless symbol_fts row (no FK / trigger — clear explicitly).
            conn.execute(
                "DELETE FROM symbol_fts WHERE id = ?1",
                rusqlite::params![id],
            )
            .map_err(store_err)?;
            // 3. outbound edges — source gone, unrecoverable.
            conn.execute("DELETE FROM edge WHERE src_id = ?1", rusqlite::params![id])
                .map_err(store_err)?;
            // 4. inbound edges — keep, flag meta.dst_missing = true (JSON bool).
            //    json_patch merges so other meta keys set by a concurrent
            //    indexer aren't clobbered (mirror cognis.db.delete_symbol).
            conn.execute(
                "UPDATE edge \
                    SET meta = json_patch( \
                            COALESCE(meta, '{}'), \
                            json_object('dst_missing', json('true')) \
                        ) \
                  WHERE dst_id = ?1",
                rusqlite::params![id],
            )
            .map_err(store_err)?;
            Ok(())
        })
    }

    fn reconcile_embedding_dim(&mut self, dim: usize) -> Result<()> {
        let conn = self.connect()?;
        let current = read_meta(&conn, EMBEDDING_DIM_META_KEY)?
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(DEFAULT_EMBEDDING_DIM);
        let is_vec0 = vec0_table_present(&conn)?;
        // `vec0` carries a FLOAT[n] width; the BLOB fallback carries none.
        let table_dim = vec_table_dim(&conn)?;

        // Self-heal a legacy `vec0` table this build can't read: the shipped
        // single binary has no sqlite-vec, so a `symbol_vec` virtual table
        // created by an engine that *did* (e.g. migrated dev DBs) is unreadable
        // here — every query hits `no such module: vec0`. When the extension
        // can't load, rebuild `symbol_vec` as the plain-BLOB fallback so the
        // linear-scan `vec_search` path works. Vectors are re-embedded on this
        // same index pass, so the heal is transparent. A build WITH sqlite-vec
        // keeps the vec0 form.
        let vec_ext_ok = try_load_vec_extension(&conn);
        let heal_vec0 = is_vec0 && !vec_ext_ok;
        let target_is_vec0 = is_vec0 && vec_ext_ok;

        // Idempotent no-op: stored dim already matches, the table form is
        // consistent, and there's no legacy vec0 table to heal.
        if current == dim && table_dim.is_none_or(|d| d == dim) && !heal_vec0 {
            return Ok(());
        }

        with_write_txn(&conn, |conn| {
            write_meta(conn, EMBEDDING_DIM_META_KEY, &dim.to_string())?;
            recreate_vec_table(conn, target_is_vec0, dim)?;
            Ok(())
        })
    }

    fn upsert_embeddings(&mut self, rows: &[(String, Vec<f32>)]) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let conn = self.connect()?;
        // Only write vectors for symbols that actually exist: the BLOB-table
        // form has an `ON DELETE CASCADE` FK to `symbol(id)`, so an orphan
        // insert would fail the whole batch. Filtering keeps the write robust
        // when a caller passes ids that were concurrently deleted.
        let existing: HashSet<String> = {
            let mut stmt = conn.prepare("SELECT id FROM symbol").map_err(store_err)?;
            let ids = stmt
                .query_map([], |r| r.get::<_, String>(0))
                .map_err(store_err)?
                .collect::<std::result::Result<HashSet<_>, _>>()
                .map_err(store_err)?;
            ids
        };
        with_write_txn(&conn, |conn| {
            // Delete-then-insert is a uniform upsert that works for both the
            // plain-BLOB table and a sqlite-vec `vec0` virtual table (which does
            // not support `ON CONFLICT`).
            let mut del = conn
                .prepare("DELETE FROM symbol_vec WHERE symbol_id = ?1")
                .map_err(store_err)?;
            let mut ins = conn
                .prepare("INSERT INTO symbol_vec (symbol_id, embedding) VALUES (?1, ?2)")
                .map_err(store_err)?;
            for (id, vec) in rows {
                if !existing.contains(id) {
                    continue;
                }
                del.execute(rusqlite::params![id]).map_err(store_err)?;
                ins.execute(rusqlite::params![id, floats_to_le_bytes(vec)])
                    .map_err(store_err)?;
            }
            Ok(())
        })
    }
}

/// Run `f` inside a single `BEGIN IMMEDIATE` write transaction, committing on
/// `Ok` and rolling back on `Err` (mirrors the Python `Database.write()`
/// context manager). `BEGIN IMMEDIATE` takes the write lock up front so a busy
/// writer fails fast against `busy_timeout` rather than mid-transaction.
fn with_write_txn<T>(conn: &Connection, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
    conn.execute_batch("BEGIN IMMEDIATE").map_err(store_err)?;
    match f(conn) {
        Ok(value) => {
            conn.execute_batch("COMMIT").map_err(store_err)?;
            Ok(value)
        }
        Err(e) => {
            // Best-effort rollback; surface the original error regardless.
            let _ = conn.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

/// Serialise a `SymbolKind` to its DB string (the serde `rename_all` form, e.g.
/// `Function` → `"function"`), matching the `kind` column the Python writer
/// stores and `row_to_symbol` reads back.
fn symbol_kind_to_db(kind: SymbolKind) -> Result<String> {
    enum_to_db_str(serde_json::to_value(kind))
}

/// Serialise an `EdgeKind` to its DB string (e.g. `RoutesTo` → `"routes_to"`).
fn edge_kind_to_db(kind: EdgeKind) -> Result<String> {
    enum_to_db_str(serde_json::to_value(kind))
}

/// Extract the JSON string a fieldless enum serialises to.
fn enum_to_db_str(value: serde_json::Result<serde_json::Value>) -> Result<String> {
    match value.map_err(store_err)? {
        serde_json::Value::String(s) => Ok(s),
        other => Err(CognisError::Store(format!(
            "expected a string enum value, got {other}"
        ))),
    }
}

/// Project `Edge::meta` to the nullable `edge.meta` TEXT column: `NULL` for a
/// null value or an empty object, else the compact JSON string. Mirrors the
/// Python `json.dumps(e.meta) if e.meta else None` (an empty dict is falsy).
fn edge_meta_to_db(meta: &serde_json::Value) -> Result<Option<String>> {
    let is_empty = match meta {
        serde_json::Value::Null => true,
        serde_json::Value::Object(map) => map.is_empty(),
        _ => false,
    };
    if is_empty {
        Ok(None)
    } else {
        Ok(Some(serde_json::to_string(meta).map_err(store_err)?))
    }
}

/// Read the `FLOAT[n]` width of the current `symbol_vec` `vec0` table, or `None`
/// when `symbol_vec` is absent or is the plain-BLOB fallback (no width).
/// Mirrors Python `cognis.db._read_vec_table_dim` without a regex dependency.
fn vec_table_dim(conn: &Connection) -> Result<Option<usize>> {
    let sql: Option<String> = conn
        .query_row(
            "SELECT sql FROM sqlite_master \
             WHERE type IN ('table','view') AND name = 'symbol_vec'",
            [],
            |r| r.get::<_, Option<String>>(0),
        )
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })
        .map_err(store_err)?;

    let Some(sql) = sql else {
        return Ok(None);
    };
    // Locate `FLOAT[<n>]` case-insensitively and parse the bracketed integer.
    let upper = sql.to_ascii_uppercase();
    let Some(open) = upper.find("FLOAT[") else {
        return Ok(None);
    };
    let rest = &sql[open + "FLOAT[".len()..];
    let Some(close) = rest.find(']') else {
        return Ok(None);
    };
    Ok(rest[..close].trim().parse::<usize>().ok())
}

/// Drop and recreate `symbol_vec` at `dim` (Requirement 2.3). A `vec0` table is
/// recreated as a sqlite-vec virtual table (best-effort extension load first);
/// the plain-BLOB fallback is recreated with migration 001's exact shape so the
/// `ON DELETE CASCADE` FK and column types stay identical.
fn recreate_vec_table(conn: &Connection, is_vec0: bool, dim: usize) -> Result<()> {
    // Defense-in-depth (indexd-vec0-legacy-crash Property 1, Req 2.2/2.4): if the
    // existing `symbol_vec` is a legacy sqlite-vec `vec0` *virtual* table this
    // build can't load, a live `DROP TABLE symbol_vec` forces SQLite to
    // instantiate the missing `vec0` module and raises `no such module: vec0`.
    // Route that drop through the same module-free `sqlite_master` deletion that
    // `heal_legacy_vec0` uses, so no code path ever issues a live `DROP` against
    // a `vec0` vtable. Detection is schema-only (`vec0_table_present`,
    // `try_load_vec_extension`), so a plain-BLOB `symbol_vec` (the common
    // post-heal shape) — and a `vec0` table on a build where the extension loads
    // — keep the ordinary `DROP TABLE` path, unchanged (Preservation, Property 2).
    if vec0_table_present(conn)? && !try_load_vec_extension(conn) {
        drop_symbol_vec_module_free(conn)?;
    } else {
        conn.execute("DROP TABLE IF EXISTS symbol_vec", [])
            .map_err(store_err)?;
    }
    if is_vec0 {
        // KNN DDL needs the sqlite-vec extension; load best-effort (a no-op in
        // the default build). If it isn't loadable the CREATE errors and the
        // surrounding transaction rolls back the DROP — we never leave a
        // vec0 table half-gone.
        let _ = try_load_vec_extension(conn);
        conn.execute_batch(&format!(
            "CREATE VIRTUAL TABLE symbol_vec USING vec0(\
               symbol_id TEXT PRIMARY KEY, \
               embedding FLOAT[{dim}]\
             );"
        ))
        .map_err(store_err)?;
    } else {
        conn.execute_batch(
            "CREATE TABLE symbol_vec (\n\
               symbol_id TEXT PRIMARY KEY,\n\
               embedding BLOB NOT NULL,\n\
               FOREIGN KEY (symbol_id) REFERENCES symbol(id) ON DELETE CASCADE\n\
             );",
        )
        .map_err(store_err)?;
    }
    Ok(())
}

/// Module-free self-heal for a legacy sqlite-vec `vec0` `symbol_vec` table on a
/// build that cannot load the `vec0` module — the bug condition
/// `hasVec0VirtualTable(X) AND NOT vec0ModuleLoadable` (indexd-vec0-legacy-crash
/// Property 1).
///
/// A real `DROP TABLE symbol_vec` against a `vec0` *virtual* table forces SQLite
/// to instantiate the `vec0` module (for `xDisconnect`/`xDestroy`); on a build
/// without the extension that raises `no such module: vec0` — the exact error
/// we are curing. So instead of a live `DROP`, this removes the virtual-table
/// entry directly from `sqlite_master` under `PRAGMA writable_schema` (which
/// never loads the module), drops the now-orphaned plain shadow tables, then
/// recreates `symbol_vec` in migration 001's exact plain-BLOB shape. All indexed
/// UCKG symbol data (`symbol`, `edge`, `symbol_attribute`, `file`, `symbol_fts`,
/// `meta`) is preserved; BLOB vectors are re-embedded on the next index pass.
///
/// Detection uses only the schema-text probes (`vec0_table_present`,
/// `try_load_vec_extension`), which read `sqlite_master.sql` and never touch the
/// missing module, so the heal is guarded to fire only for the bug condition and
/// is a byte-for-byte no-op for every other DB (plain-BLOB `symbol_vec`, a `vec0`
/// table on a build where the extension loads, no `symbol_vec`, or a fresh DB).
/// It is idempotent: after one heal the table is plain BLOB (`is_vec0 == false`)
/// so a second call returns immediately.
fn heal_legacy_vec0(conn: &Connection) -> Result<()> {
    let is_vec0 = vec0_table_present(conn)?;
    let vec_ext_ok = try_load_vec_extension(conn);
    // Only a legacy vec0 table this build can't read needs healing; every other
    // shape (Preservation, Property 2) is left untouched.
    let is_bug_condition = is_vec0 && !vec_ext_ok;
    if !is_bug_condition {
        return Ok(());
    }

    // Preserve the recorded `FLOAT[n]` width for the recreated table's active
    // dim (the plain-BLOB shape itself carries no width); fall back to the
    // persisted `meta.embedding_dim`, then the crate default.
    let dim = vec_table_dim(conn)?
        .or_else(|| {
            read_meta(conn, EMBEDDING_DIM_META_KEY)
                .ok()
                .flatten()
                .and_then(|v| v.parse::<usize>().ok())
        })
        .unwrap_or(DEFAULT_EMBEDDING_DIM);

    // Recreate `symbol_vec` in migration 001's exact plain-BLOB shape. Because
    // the existing table is a `vec0` vtable this build can't load,
    // `recreate_vec_table` routes its drop through the shared module-free
    // `sqlite_master` deletion (see `drop_symbol_vec_module_free`) instead of a
    // live `DROP TABLE`, so the missing module is never instantiated.
    with_write_txn(conn, |conn| recreate_vec_table(conn, false, dim))
}

/// Remove the `symbol_vec` virtual-table entry and its `symbol_vec_*` shadow
/// tables directly from `sqlite_master` under `PRAGMA writable_schema`, without
/// ever instantiating the (possibly missing) `vec0` module.
///
/// A live `DROP TABLE symbol_vec` against a `vec0` *virtual* table forces SQLite
/// to load the module (for `xDisconnect`/`xDestroy`); on a build without the
/// extension that raises `no such module: vec0`. Deleting the `sqlite_master`
/// rows under `writable_schema` never loads the module, so this is safe on any
/// build. Shared by `heal_legacy_vec0` (heal-on-open) and `recreate_vec_table`
/// (dimension reconcile) so no code path issues a live `DROP` of a legacy `vec0`
/// vtable (indexd-vec0-legacy-crash Req 2.2/2.4).
///
/// Data-safety: matches ONLY the `symbol_vec_` prefix (escaped `_` via
/// `ESCAPE '\'`) so it never touches `symbol`, `symbol_attribute`, or
/// `symbol_fts*`. Callers are expected to run this inside a write transaction.
fn drop_symbol_vec_module_free(conn: &Connection) -> Result<()> {
    // Shadow tables are the ordinary tables sqlite-vec created to back the
    // `vec0` virtual table (e.g. `symbol_vec_chunks`, `symbol_vec_rowids`,
    // `symbol_vec_vector_chunks00`). They are plain tables and drop without the
    // module. Match ONLY the `symbol_vec_` prefix (escaped `_`).
    let shadow_tables: Vec<String> = {
        let mut stmt = conn
            .prepare(
                "SELECT name FROM sqlite_master \
                 WHERE type = 'table' AND name LIKE 'symbol_vec\\_%' ESCAPE '\\'",
            )
            .map_err(store_err)?;
        let names = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(store_err)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(store_err)?;
        names
    };
    for name in &shadow_tables {
        // Data-safety guard: never drop anything outside the `symbol_vec_`
        // prefix, even though the query already constrained it.
        debug_assert!(name.starts_with("symbol_vec_"));
        if name.starts_with("symbol_vec_") {
            conn.execute_batch(&format!("DROP TABLE IF EXISTS \"{name}\";"))
                .map_err(store_err)?;
        }
    }

    // Remove the `vec0` virtual-table entry (and any leftover `symbol_vec_%`
    // shadow rows) straight from `sqlite_master` without instantiating the
    // module, then force the schema to be re-read (`writable_schema = RESET`) so
    // the just-deleted vtable is no longer in this connection's cached schema.
    // `ESCAPE '\'` keeps the `_` literal so the LIKE can't match unrelated names.
    conn.execute_batch(
        "PRAGMA writable_schema = ON;\n\
         DELETE FROM sqlite_master \
            WHERE name = 'symbol_vec' OR name LIKE 'symbol_vec\\_%' ESCAPE '\\';\n\
         PRAGMA writable_schema = OFF;\n\
         PRAGMA writable_schema = RESET;",
    )
    .map_err(store_err)?;
    Ok(())
}

/// Reconcile-on-open: safe index self-recovery run on every `open_new_connection`
/// immediately after the legacy-`vec0` heal.
///
/// Two layered, safety-first responsibilities:
///
/// 1. **Stamp the engine build.** Upsert `meta.index_version =
///    env!("CARGO_PKG_VERSION")` (idempotent) so `.cognis` always records the
///    engine that last opened/reconciled it. Best-effort — a read-only DB
///    silently keeps its old stamp (the write error is ignored).
/// 2. **Genuine-incompatibility safe reset.** Read the on-disk
///    `meta.schema_version`; only when it is **strictly greater** than
///    [`LATEST_SCHEMA_VERSION`] — i.e. the DB was written by a NEWER engine whose
///    schema this build's migrations cannot downgrade — perform a SAFE in-place
///    index reset ([`safe_index_reset`]). Normal opens (schema <= LATEST) never
///    reset.
///
/// Safety invariants: this never touches any file other than the DB and never
/// deletes source; a normal / plain-BLOB DB at schema <= LATEST is byte-identical
/// after open aside from the idempotent `index_version` stamp. The whole routine
/// is best-effort — its caller logs and ignores any error so open still succeeds.
fn reconcile_on_open(conn: &Connection) -> Result<()> {
    // (1) Stamp the engine build on every open. Best-effort: a read-only DB
    // (or a lost write lock) can't take the write, and that must not fail open.
    let _ = write_meta(conn, "index_version", env!("CARGO_PKG_VERSION"));

    // (2) Safe reset ONLY for a genuinely-incompatible future DB. Reading the
    // stamped schema version is a plain read (works read-only); an absent /
    // unparsable value reads as 0, which is never `> LATEST_SCHEMA_VERSION`.
    let on_disk_schema = read_meta(conn, "schema_version")?
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(0);
    if on_disk_schema > LATEST_SCHEMA_VERSION {
        safe_index_reset(conn)?;
    }
    Ok(())
}

/// SAFE in-place index reset for a DB written by a strictly-newer engine
/// (`schema_version > LATEST_SCHEMA_VERSION`), gated by [`reconcile_on_open`].
///
/// Migrations only move forward, so a future schema cannot be downgraded in
/// place; rather than dead-end the user (health `warn` → panel "Needs
/// attention"), reset the index tables to the current empty schema and let a
/// later index pass repopulate. Runs inside a single [`with_write_txn`] so a
/// failure (read-only DB / lock) rolls back atomically — the DB is never left
/// half-reset — and the caller degrades gracefully.
///
/// What is dropped: `symbol_vec` (module-free when it's a legacy `vec0` vtable
/// this build can't load — reusing [`drop_symbol_vec_module_free`] so the
/// missing module is never instantiated — else a plain `DROP`), then the other
/// index tables the schema owns: `symbol_fts` (its FTS5 shadow tables drop with
/// it), `symbol_attribute`, `edge`, `file`, `symbol`, and finally `meta`.
/// Child tables (`symbol_vec`, `symbol_attribute`) drop before their `symbol`
/// parent so the `ON DELETE CASCADE` FKs never raise. Re-running migration 001
/// (via [`run_migrations`], now that `meta` — and thus `schema_version` — is
/// gone) recreates the current empty schema and stamps
/// `schema_version = LATEST_SCHEMA_VERSION` + `index_version`.
///
/// The `.cognis` FILE and everything outside the DB (config, audit, caches) are
/// preserved; only DB tables reset. No source is ever touched.
fn safe_index_reset(conn: &Connection) -> Result<()> {
    // Decide the symbol_vec drop strategy with schema-only probes BEFORE the
    // write txn (they read `sqlite_master` and never instantiate the vec0
    // module): a legacy `vec0` vtable this build can't load needs the
    // module-free deletion; anything else takes a plain `DROP`.
    let vec_module_free = vec0_table_present(conn)? && !try_load_vec_extension(conn);

    with_write_txn(conn, |conn| {
        // Drop `symbol_vec` first (a child of `symbol` via its cascade FK).
        if vec_module_free {
            drop_symbol_vec_module_free(conn)?;
        } else {
            conn.execute("DROP TABLE IF EXISTS symbol_vec", [])
                .map_err(store_err)?;
        }
        // Remaining index tables. `symbol_fts` is an FTS5 virtual table whose
        // shadow tables drop with it (the fts5 module is always available).
        // Order matters with `foreign_keys = ON`: drop `symbol_attribute`
        // (child of `symbol`) before `symbol`.
        for ddl in [
            "DROP TABLE IF EXISTS symbol_fts",
            "DROP TABLE IF EXISTS symbol_attribute",
            "DROP TABLE IF EXISTS edge",
            "DROP TABLE IF EXISTS file",
            "DROP TABLE IF EXISTS symbol",
            "DROP TABLE IF EXISTS meta",
        ] {
            conn.execute_batch(ddl).map_err(store_err)?;
        }
        // Recreate the current empty schema. With `meta` gone, `run_migrations`
        // sees `schema_version = 0`, applies migration 001, and stamps
        // `schema_version = LATEST_SCHEMA_VERSION` + `index_version`.
        run_migrations(conn)?;
        Ok(())
    })
}

// ---------------------------------------------------------------------------
// vec_search internals (task 3.3)
// ---------------------------------------------------------------------------

/// Serialise an `f32` slice to the little-endian byte layout sqlite-vec and the
/// Python writer use (`struct.pack("<{n}f", ...)`).
fn floats_to_le_bytes(v: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(v.len() * 4);
    for f in v {
        bytes.extend_from_slice(&f.to_le_bytes());
    }
    bytes
}

/// Decode a little-endian `f32` BLOB. Returns `None` when the length is not a
/// whole number of `f32`s (corrupt row → skipped, not a panic).
fn le_bytes_to_floats(blob: &[u8]) -> Option<Vec<f32>> {
    if blob.is_empty() || !blob.len().is_multiple_of(4) {
        return None;
    }
    Some(
        blob.chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect(),
    )
}

/// Cosine distance `1 - cos(a, b)`, accumulated in `f64` for numeric parity
/// with the Python/numpy oracle regardless of summation order. A zero-norm
/// vector has undefined direction; we define its similarity as 0 ⇒ distance 1
/// (so it sorts last, never panics on a divide-by-zero).
fn cosine_distance(a: &[f32], b: &[f32]) -> f64 {
    let mut dot = 0.0f64;
    let mut na = 0.0f64;
    let mut nb = 0.0f64;
    for (x, y) in a.iter().zip(b.iter()) {
        let (x, y) = (*x as f64, *y as f64);
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        return 1.0;
    }
    let cos = dot / (na.sqrt() * nb.sqrt());
    1.0 - cos
}

/// Build a semantic [`Hit`] from a `(symbol_id, distance, qualified_name)`
/// triple, mirroring `SemanticLayer` field-for-field: `score = max(0, 1 -
/// distance)`, `evidence = {"score": distance}`, and the reason string format.
fn semantic_hit(symbol_id: String, distance: f64, qualified_name: Option<String>) -> Hit {
    let score = (1.0 - distance).max(0.0);
    let reason = match &qualified_name {
        Some(qn) => format!("KNN cosine distance {distance:.4}: {qn}"),
        None => format!("KNN cosine distance {distance:.4} (symbol row not found)"),
    };
    Hit {
        symbol_id,
        score,
        layer: "semantic".to_string(),
        reason,
        evidence: serde_json::json!({ "score": distance }),
    }
}

/// True when `symbol_vec` exists as a sqlite-vec `vec0` virtual table (vs the
/// plain-BLOB fallback). Mirrors the `USING vec0` probe in `semantic.py`.
fn vec0_table_present(conn: &Connection) -> Result<bool> {
    let sql: Option<String> = conn
        .query_row(
            "SELECT sql FROM sqlite_master \
             WHERE type IN ('table','view') AND name = 'symbol_vec'",
            [],
            |r| r.get::<_, Option<String>>(0),
        )
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })
        .map_err(store_err)?;
    Ok(sql.is_some_and(|s| s.contains("USING vec0")))
}

/// Primary KNN over a `vec0` table — issues the same `embedding MATCH ? AND
/// k = ?` query the Python `SemanticLayer` does, hydrating `qualified_name` via
/// a LEFT JOIN. Returns `Err` when the extension is not loaded (the MATCH
/// fails); the caller degrades that to an empty result.
fn vec0_knn(conn: &Connection, q: &[f32], k: usize) -> Result<Vec<Hit>> {
    let blob = floats_to_le_bytes(q);
    let mut stmt = conn
        .prepare(
            "SELECT v.symbol_id, v.distance, s.qualified_name \
             FROM symbol_vec v \
             LEFT JOIN symbol s ON s.id = v.symbol_id \
             WHERE v.embedding MATCH ?1 AND k = ?2 \
             ORDER BY v.distance, v.symbol_id",
        )
        .map_err(store_err)?;
    let rows = stmt
        .query_map(rusqlite::params![blob, k as i64], |row| {
            let symbol_id: String = row.get(0)?;
            let distance: f64 = row.get(1)?;
            let qualified_name: Option<String> = row.get(2)?;
            Ok((symbol_id, distance, qualified_name))
        })
        .map_err(store_err)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(store_err)?;
    Ok(rows
        .into_iter()
        .map(|(id, dist, qn)| semantic_hit(id, dist, qn))
        .collect())
}

/// Fallback KNN (Requirement 2.4): read every `(symbol_id, embedding)` BLOB
/// row, cosine-rank in Rust, and return the top-`k` hydrated hits. Rows whose
/// BLOB is corrupt or a different dimension than `q` are skipped (graceful).
/// Ties break on `symbol_id` so the order is deterministic and matches the
/// Python oracle.
fn blob_linear_scan(conn: &Connection, q: &[f32], k: usize) -> Result<Vec<Hit>> {
    let mut stmt = conn
        .prepare(
            "SELECT v.symbol_id, v.embedding, s.qualified_name \
             FROM symbol_vec v \
             LEFT JOIN symbol s ON s.id = v.symbol_id",
        )
        .map_err(store_err)?;
    let rows = stmt
        .query_map([], |row| {
            let symbol_id: String = row.get(0)?;
            let blob: Vec<u8> = row.get(1)?;
            let qualified_name: Option<String> = row.get(2)?;
            Ok((symbol_id, blob, qualified_name))
        })
        .map_err(store_err)?;

    let mut scored: Vec<(f64, String, Option<String>)> = Vec::new();
    for row in rows {
        let (symbol_id, blob, qualified_name) = row.map_err(store_err)?;
        let Some(vec) = le_bytes_to_floats(&blob) else {
            continue; // corrupt / non-f32 BLOB — skip, don't abort.
        };
        if vec.len() != q.len() {
            continue; // dimension mismatch — not comparable, skip.
        }
        scored.push((cosine_distance(q, &vec), symbol_id, qualified_name));
    }

    // Nearest first (ascending distance), `symbol_id` as a stable tiebreak.
    scored.sort_by(|a, b| {
        a.0.partial_cmp(&b.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.1.cmp(&b.1))
    });
    scored.truncate(k);

    Ok(scored
        .into_iter()
        .map(|(dist, id, qn)| semantic_hit(id, dist, qn))
        .collect())
}

/// Best-effort load of the sqlite-vec loadable extension into `conn`.
///
/// Only does anything under the `sqlite-vec` feature (which turns on rusqlite's
/// `load_extension`); the path comes from `COGNIS_SQLITE_VEC_PATH`. Any failure
/// (feature off, env unset, file missing, load error) returns `false` so the
/// caller degrades gracefully (Requirement 2.4). Task 10.1 will static-link the
/// extension for the shipped single binary.
#[cfg(feature = "sqlite-vec")]
fn try_load_vec_extension(conn: &Connection) -> bool {
    let Ok(path) = std::env::var("COGNIS_SQLITE_VEC_PATH") else {
        return false;
    };
    // SAFETY: loading an extension runs its init function. We enable extension
    // loading only around this call and disable it immediately after, per
    // rusqlite guidance, so no other code path can load arbitrary libraries.
    unsafe {
        if conn.load_extension_enable().is_err() {
            return false;
        }
        let loaded = conn.load_extension(&path, None).is_ok();
        let _ = conn.load_extension_disable();
        loaded
    }
}

/// No-op when the `sqlite-vec` feature is disabled (the default): there is no
/// extension to load, so `vec_search` uses the BLOB linear-scan fallback.
#[cfg(not(feature = "sqlite-vec"))]
fn try_load_vec_extension(_conn: &Connection) -> bool {
    false
}

// ---------------------------------------------------------------------------
// Migration runner
// ---------------------------------------------------------------------------
/// Apply pending migrations and return the resulting `schema_version`.
///
/// Reads `meta.schema_version` (default 0), runs every embedded migration whose
/// numeric version exceeds it (ascending), and records the new
/// `schema_version` + `index_version`. The DDL is idempotent
/// (`CREATE ... IF NOT EXISTS`), and a DB already at the latest version applies
/// nothing — so opening a Python-built `.cognis/uckg.db` is a no-op
/// (Requirement 2.1).
pub fn run_migrations(conn: &Connection) -> Result<i64> {
    let current = read_meta(conn, "schema_version")?
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(0);

    let mut applied = current;
    for (version, sql) in MIGRATIONS {
        if *version <= current {
            continue;
        }
        // execute_batch runs the DDL under autocommit; statements are
        // individually idempotent, so a partial failure is safely retried.
        conn.execute_batch(sql).map_err(store_err)?;
        write_meta(conn, "schema_version", &version.to_string())?;
        write_meta(conn, "index_version", env!("CARGO_PKG_VERSION"))?;
        applied = *version;
    }
    Ok(applied)
}

fn read_meta(conn: &Connection, key: &str) -> Result<Option<String>> {
    match conn.query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| {
        r.get::<_, String>(0)
    }) {
        Ok(v) => Ok(Some(v)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        // `meta` table doesn't exist yet — the first-boot case before 001 runs.
        Err(e) if e.to_string().contains("no such table") => Ok(None),
        Err(e) => Err(store_err(e)),
    }
}

fn write_meta(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO meta(key, value) VALUES(?1, ?2) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        rusqlite::params![key, value],
    )
    .map_err(store_err)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Row mapping (mirror of cognis.db._row_to_symbol / _row_to_edge)
// ---------------------------------------------------------------------------

fn parse_symbol_kind(s: &str) -> rusqlite::Result<SymbolKind> {
    serde_json::from_value(serde_json::Value::String(s.to_string()))
        .map_err(|e| to_sqlite_conv_err(format!("bad symbol kind {s:?}: {e}")))
}

fn parse_edge_kind(s: &str) -> rusqlite::Result<EdgeKind> {
    serde_json::from_value(serde_json::Value::String(s.to_string()))
        .map_err(|e| to_sqlite_conv_err(format!("bad edge kind {s:?}: {e}")))
}

fn to_sqlite_conv_err(msg: String) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, msg.into())
}

fn row_to_symbol(row: &rusqlite::Row<'_>) -> rusqlite::Result<Symbol> {
    let flags_raw: Option<String> = row.get("untrusted_flags")?;
    let untrusted_flags = match flags_raw.as_deref() {
        None | Some("") => Vec::new(),
        Some(text) => serde_json::from_str::<Vec<String>>(text)
            .map_err(|e| to_sqlite_conv_err(format!("corrupt untrusted_flags: {e}")))?,
    };
    let ambiguous_int: i64 = row.get("ambiguous")?;
    Ok(Symbol {
        id: row.get("id")?,
        kind: parse_symbol_kind(&row.get::<_, String>("kind")?)?,
        name: row.get("name")?,
        qualified_name: row.get("qualified_name")?,
        language: row.get("language")?,
        module: row.get("module")?,
        file_path: row.get("file_path")?,
        line_start: row.get("line_start")?,
        line_end: row.get("line_end")?,
        signature: row.get("signature")?,
        docstring: row.get("docstring")?,
        content_hash: row.get("content_hash")?,
        body_excerpt: row.get("body_excerpt")?,
        semantic_summary: row.get("semantic_summary")?,
        risk_score: row.get("risk_score")?,
        ambiguous: ambiguous_int != 0,
        untrusted_flags,
        updated_at: row.get("updated_at")?,
    })
}

fn row_to_edge(row: &rusqlite::Row<'_>) -> rusqlite::Result<Edge> {
    let meta_raw: Option<String> = row.get("meta")?;
    let meta = match meta_raw.as_deref() {
        None | Some("") => serde_json::Value::Object(serde_json::Map::new()),
        Some(text) => serde_json::from_str(text)
            .map_err(|e| to_sqlite_conv_err(format!("corrupt edge.meta: {e}")))?,
    };
    Ok(Edge {
        src_id: row.get("src_id")?,
        dst_id: row.get("dst_id")?,
        kind: parse_edge_kind(&row.get::<_, String>("kind")?)?,
        confidence: row.get("confidence")?,
        meta,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrations_create_full_schema_on_fresh_db() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fresh.db");
        let db = Database::open(&path).unwrap();

        assert_eq!(db.schema_version().unwrap(), LATEST_SCHEMA_VERSION);

        // Every UCKG table/virtual-table the schema promises exists and reads.
        for table in ["symbol", "edge", "symbol_attribute", "file"] {
            assert_eq!(
                db.count(table).unwrap(),
                0,
                "table {table} should exist + be empty"
            );
        }
        // meta carries the two keys the migration runner writes.
        assert_eq!(
            db.count("meta").unwrap(),
            2,
            "meta has schema_version + index_version"
        );
        // FTS5 + the BLOB-fallback symbol_vec must be present and queryable.
        assert!(db.fts_match_ids("anything", 5).unwrap().is_empty());
        assert!(db.vec_symbol_ids().unwrap().is_empty());

        db.close_thread_connection();
    }

    #[test]
    fn migrations_are_idempotent_noop_on_second_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("idem.db");

        let db = Database::open(&path).unwrap();
        db.close_thread_connection();

        // Re-open: schema_version is stable and run_migrations applies nothing.
        let db2 = Database::open(&path).unwrap();
        let conn = db2.connect().unwrap();
        let applied = run_migrations(&conn).unwrap();
        assert_eq!(applied, LATEST_SCHEMA_VERSION);
        assert_eq!(db2.schema_version().unwrap(), LATEST_SCHEMA_VERSION);
        db2.close_thread_connection();
    }

    #[test]
    fn illegal_table_name_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(dir.path().join("guard.db")).unwrap();
        assert!(db.count("symbol; DROP TABLE symbol").is_err());
        db.close_thread_connection();
    }

    // ----- vec_search fallback (task 3.3) ---------------------------------

    /// Round-trip a vector through the little-endian BLOB layout the Python
    /// writer + sqlite-vec use, so the fallback reads exactly what was written.
    #[test]
    fn le_bytes_roundtrip() {
        let v = vec![0.0f32, 1.5, -2.25, 384.0];
        let blob = floats_to_le_bytes(&v);
        assert_eq!(blob.len(), v.len() * 4);
        assert_eq!(le_bytes_to_floats(&blob), Some(v));
        // A short / non-multiple-of-4 BLOB decodes to None (skipped, no panic).
        assert_eq!(le_bytes_to_floats(&[1, 2, 3]), None);
        assert_eq!(le_bytes_to_floats(&[]), None);
    }

    /// Cosine distance: identical direction → 0, orthogonal → 1, opposite → 2;
    /// a zero vector degrades to distance 1 instead of dividing by zero.
    #[test]
    fn cosine_distance_basics() {
        assert!((cosine_distance(&[1.0, 0.0], &[2.0, 0.0])).abs() < 1e-12);
        assert!((cosine_distance(&[1.0, 0.0], &[0.0, 1.0]) - 1.0).abs() < 1e-12);
        assert!((cosine_distance(&[1.0, 0.0], &[-1.0, 0.0]) - 2.0).abs() < 1e-12);
        assert!((cosine_distance(&[0.0, 0.0], &[1.0, 1.0]) - 1.0).abs() < 1e-12);
    }

    /// The public `upsert_embeddings` writer path: vectors written through the
    /// trait are read back by `vec_search` nearest-first, upserts replace in
    /// place (no dup rows), and orphan ids (no matching symbol) are skipped.
    /// This is the seam the indexer uses to persist embeddings and the MCP
    /// server queries — the wiring the contract tests don't exercise.
    #[test]
    fn upsert_embeddings_roundtrips_through_vec_search() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = Database::open(dir.path().join("emb.db")).unwrap();

        // Three real symbols (FK requires the symbol rows to exist first).
        let syms: &[(&str, [f32; 3])] = &[
            ("s:a", [1.0, 0.0, 0.0]),
            ("s:b", [0.0, 1.0, 0.0]),
            ("s:c", [0.9, 0.1, 0.0]),
        ];
        {
            let conn = db.connect().unwrap();
            for (id, _) in syms {
                conn.execute(
                    "INSERT INTO symbol(id, kind, name, qualified_name, language, module, \
                     file_path, line_start, line_end, content_hash, updated_at) \
                     VALUES(?1,'function',?1,?1,'rust','m','m.rs',1,1,'h',0)",
                    rusqlite::params![id],
                )
                .unwrap();
            }
        }

        // Reconcile to dim 3 then write via the public trait method.
        db.reconcile_embedding_dim(3).unwrap();
        let rows: Vec<(String, Vec<f32>)> = syms
            .iter()
            .map(|(id, v)| ((*id).to_string(), v.to_vec()))
            .chain(std::iter::once((
                // Orphan id (no symbol row) must be skipped, not error the batch.
                "s:ghost".to_string(),
                vec![0.5, 0.5, 0.0],
            )))
            .collect();
        db.upsert_embeddings(&rows).unwrap();
        assert_eq!(db.vec_row_count().unwrap(), 3, "orphan id was skipped");

        // Query along s:a's axis → s:a nearest, then s:c, then s:b.
        let hits = db.vec_search(&[1.0, 0.0, 0.0], 10).unwrap();
        let ids: Vec<&str> = hits.iter().map(|h| h.symbol_id.as_str()).collect();
        assert_eq!(ids, vec!["s:a", "s:c", "s:b"]);
        assert_eq!(hits[0].layer, "semantic");

        // Re-upsert s:a with a new vector: replace in place, no duplicate row.
        db.upsert_embeddings(&[("s:a".to_string(), vec![0.0, 0.0, 1.0])])
            .unwrap();
        assert_eq!(db.vec_row_count().unwrap(), 3, "upsert replaced, no dup");
        let top = db.vec_search(&[0.0, 0.0, 1.0], 1).unwrap();
        assert_eq!(top[0].symbol_id, "s:a", "new vector is now on the z-axis");

        db.close_thread_connection();
    }

    /// Insert BLOB vectors into a fresh fallback `symbol_vec` and confirm the
    /// linear scan returns nearest-first, respects `k`, and carries the
    /// semantic-layer hit contract (layer / score = 1 - distance / evidence).
    #[test]
    fn vec_search_fallback_ranks_nearest_first() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(dir.path().join("vec.db")).unwrap();
        let conn = db.connect().unwrap();

        let rows: &[(&str, [f32; 3])] = &[
            ("s:a", [1.0, 0.0, 0.0]),
            ("s:b", [0.0, 1.0, 0.0]),
            ("s:c", [0.9, 0.1, 0.0]), // close to s:a in direction
        ];
        for (id, vec) in rows {
            conn.execute(
                "INSERT INTO symbol(id, kind, name, qualified_name, language, module, \
                 file_path, line_start, line_end, content_hash, updated_at) \
                 VALUES(?1,'function',?1,?1,'rust','m','m.rs',1,1,'h',0)",
                rusqlite::params![id],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO symbol_vec(symbol_id, embedding) VALUES(?1, ?2)",
                rusqlite::params![id, floats_to_le_bytes(vec)],
            )
            .unwrap();
        }

        // Query along the s:a axis: nearest is s:a (dist 0), then s:c, then s:b.
        let hits = db.vec_search(&[1.0, 0.0, 0.0], 10).unwrap();
        let ids: Vec<&str> = hits.iter().map(|h| h.symbol_id.as_str()).collect();
        assert_eq!(ids, vec!["s:a", "s:c", "s:b"]);

        // Hit contract mirrors SemanticLayer.
        assert_eq!(hits[0].layer, "semantic");
        assert!((hits[0].score - 1.0).abs() < 1e-9, "self-match score ≈ 1");
        assert!(hits[0].reason.contains("KNN cosine distance"));
        let ev = hits[0].evidence.get("score").and_then(|v| v.as_f64());
        assert!(
            ev.is_some_and(|d| d.abs() < 1e-9),
            "evidence.score = distance"
        );

        // k caps the result; empty query / k = 0 are graceful no-ops.
        assert_eq!(db.vec_search(&[1.0, 0.0, 0.0], 1).unwrap().len(), 1);
        assert!(db.vec_search(&[], 10).unwrap().is_empty());
        assert!(db.vec_search(&[1.0, 0.0, 0.0], 0).unwrap().is_empty());

        db.close_thread_connection();
    }

    /// A stored vector whose dimension differs from the query is skipped rather
    /// than compared (graceful degradation, no panic).
    #[test]
    fn vec_search_skips_dimension_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(dir.path().join("vecdim.db")).unwrap();
        let conn = db.connect().unwrap();
        for (id, vec) in [
            ("s:ok", vec![1.0f32, 0.0]),
            ("s:bad", vec![1.0f32, 0.0, 0.0]),
        ] {
            conn.execute(
                "INSERT INTO symbol(id, kind, name, qualified_name, language, module, \
                 file_path, line_start, line_end, content_hash, updated_at) \
                 VALUES(?1,'function',?1,?1,'rust','m','m.rs',1,1,'h',0)",
                rusqlite::params![id],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO symbol_vec(symbol_id, embedding) VALUES(?1, ?2)",
                rusqlite::params![id, floats_to_le_bytes(&vec)],
            )
            .unwrap();
        }
        let hits = db.vec_search(&[1.0, 0.0], 10).unwrap();
        let ids: Vec<&str> = hits.iter().map(|h| h.symbol_id.as_str()).collect();
        assert_eq!(ids, vec!["s:ok"], "mismatched-dim row is skipped");
        db.close_thread_connection();
    }

    // ----- SymbolWriter (task 3.4) ----------------------------------------

    /// A minimal valid symbol; vary `id`/`name` per row.
    fn sym(id: &str, name: &str) -> Symbol {
        Symbol {
            id: id.into(),
            kind: SymbolKind::Function,
            name: name.into(),
            qualified_name: format!("m.{name}"),
            language: "rust".into(),
            module: "m".into(),
            file_path: "src/m.rs".into(),
            line_start: 1,
            line_end: 2,
            signature: Some("fn ()".into()),
            docstring: None,
            content_hash: "h".into(),
            body_excerpt: Some(format!("body of {name}")),
            semantic_summary: None,
            risk_score: 0.0,
            ambiguous: false,
            untrusted_flags: vec![],
            updated_at: 0,
        }
    }

    /// A `Calls` edge with empty meta; callers tweak kind/meta as needed.
    fn edge(src: &str, dst: &str) -> Edge {
        Edge {
            src_id: src.into(),
            dst_id: dst.into(),
            kind: EdgeKind::Calls,
            confidence: 1.0,
            meta: serde_json::Value::Null,
        }
    }

    /// Symbols round-trip through upsert + read-back, and the contentless
    /// `symbol_fts` stays in sync (searchable, and re-upserting an id leaves
    /// exactly one FTS row — no orphan accumulation).
    #[test]
    fn upsert_symbols_roundtrip_and_fts_sync() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = Database::open(dir.path().join("w.db")).unwrap();

        db.upsert_symbols(&[sym("s:a", "alpha"), sym("s:b", "beta")])
            .unwrap();

        let got = db.list_symbols().unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].id, "s:a");
        assert_eq!(got[0].name, "alpha");
        assert_eq!(got[0].signature.as_deref(), Some("fn ()"));

        // FTS row present + searchable; one row per symbol.
        assert_eq!(db.fts_match_ids("alpha", 10).unwrap(), vec!["s:a"]);
        assert_eq!(db.count("symbol_fts").unwrap(), 2);

        // Re-upsert the same id with new text: the symbol row updates and the
        // FTS table keeps exactly one row (stale tokens gone, no duplicates).
        let mut updated = sym("s:a", "alpharenamed");
        updated.body_excerpt = Some("changed".into());
        db.upsert_symbols(&[updated]).unwrap();

        assert_eq!(db.count("symbol").unwrap(), 2);
        assert_eq!(db.count("symbol_fts").unwrap(), 2, "no duplicate FTS rows");
        assert!(
            db.fts_match_ids("alpha", 10).unwrap().is_empty(),
            "stale token removed"
        );
        assert_eq!(db.fts_match_ids("alpharenamed", 10).unwrap(), vec!["s:a"]);

        // Empty slice is a no-op.
        db.upsert_symbols(&[]).unwrap();
        assert_eq!(db.count("symbol").unwrap(), 2);

        db.close_thread_connection();
    }

    /// Edges round-trip; empty meta persists as NULL (reads back as `{}`), a
    /// populated meta survives, and the composite-key upsert updates in place.
    #[test]
    fn upsert_edges_roundtrip_with_meta_null_rule() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = Database::open(dir.path().join("e.db")).unwrap();

        let mut with_meta = edge("s:a", "s:b");
        with_meta.kind = EdgeKind::Imports;
        with_meta.confidence = 0.5;
        with_meta.meta = serde_json::json!({ "weight": 3 });
        db.upsert_edges(&[edge("s:a", "s:b"), with_meta]).unwrap();

        let got = db.list_edges().unwrap();
        assert_eq!(got.len(), 2);
        let calls = got.iter().find(|e| e.kind == EdgeKind::Calls).unwrap();
        assert_eq!(
            calls.meta,
            serde_json::json!({}),
            "empty meta ⇒ NULL ⇒ {{}}"
        );
        let imports = got.iter().find(|e| e.kind == EdgeKind::Imports).unwrap();
        assert_eq!(imports.confidence, 0.5);
        assert_eq!(imports.meta, serde_json::json!({ "weight": 3 }));

        // Conflict on (src,dst,kind) updates confidence + meta in place.
        let mut changed = edge("s:a", "s:b");
        changed.confidence = 0.9;
        db.upsert_edges(&[changed]).unwrap();
        let got2 = db.list_edges().unwrap();
        assert_eq!(got2.len(), 2, "upsert, not insert");
        let calls2 = got2.iter().find(|e| e.kind == EdgeKind::Calls).unwrap();
        assert_eq!(calls2.confidence, 0.9);

        db.upsert_edges(&[]).unwrap(); // no-op
        db.close_thread_connection();
    }

    /// `delete_symbol` removes the symbol + its FTS/attribute/vec rows and its
    /// outbound edges, but KEEPS inbound edges flagged `meta.dst_missing=true`
    /// (preserving other meta keys) — the CP-3 application-layer FK.
    #[test]
    fn delete_symbol_cascades_and_flags_inbound_edges() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = Database::open(dir.path().join("d.db")).unwrap();

        db.upsert_symbols(&[sym("s:a", "aaa"), sym("s:x", "xxx"), sym("s:b", "bbb")])
            .unwrap();

        // An attribute + a vector for s:x — both must cascade on delete.
        {
            let conn = db.connect().unwrap();
            conn.execute(
                "INSERT INTO symbol_attribute(symbol_id, key, value) \
                 VALUES('s:x','http_route','/x')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO symbol_vec(symbol_id, embedding) VALUES('s:x', ?1)",
                rusqlite::params![floats_to_le_bytes(&[1.0, 2.0])],
            )
            .unwrap();
        }

        // a -> x is inbound to x (must be kept); x -> b is outbound (must go).
        let mut a_to_x = edge("s:a", "s:x");
        a_to_x.meta = serde_json::json!({ "note": "keep" });
        db.upsert_edges(&[a_to_x, edge("s:x", "s:b")]).unwrap();

        db.delete_symbol("s:x").unwrap();

        // Symbol + FTS + attribute + vec for s:x all gone (FTS + FK cascades).
        assert!(db.list_symbols().unwrap().iter().all(|s| s.id != "s:x"));
        assert!(db.fts_match_ids("xxx", 10).unwrap().is_empty());
        assert_eq!(db.count("symbol_attribute").unwrap(), 0, "attr cascaded");
        assert!(
            !db.vec_symbol_ids().unwrap().contains(&"s:x".to_string()),
            "vec row cascaded"
        );

        // Outbound x->b removed; inbound a->x kept + flagged, note preserved.
        let edges = db.list_edges().unwrap();
        assert_eq!(edges.len(), 1, "only the inbound edge survives");
        let inbound = &edges[0];
        assert_eq!(
            (inbound.src_id.as_str(), inbound.dst_id.as_str()),
            ("s:a", "s:x")
        );
        assert!(inbound.dst_missing(), "inbound flagged dst_missing");
        assert_eq!(
            inbound.meta.get("note").and_then(|v| v.as_str()),
            Some("keep"),
            "prior meta keys preserved by json_patch"
        );

        // Deleting an absent id is a harmless no-op.
        db.delete_symbol("s:nope").unwrap();
        db.close_thread_connection();
    }

    /// `reconcile_embedding_dim` persists the new dim, drops + recreates
    /// `symbol_vec` (discarding stale vectors), and is idempotent on re-call.
    #[test]
    fn reconcile_embedding_dim_changes_dim_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = Database::open(dir.path().join("r.db")).unwrap();

        db.upsert_symbols(&[sym("s:a", "aaa")]).unwrap();
        {
            let conn = db.connect().unwrap();
            conn.execute(
                "INSERT INTO symbol_vec(symbol_id, embedding) VALUES('s:a', ?1)",
                rusqlite::params![floats_to_le_bytes(&[1.0, 2.0, 3.0])],
            )
            .unwrap();
        }
        assert_eq!(db.vec_symbol_ids().unwrap(), vec!["s:a".to_string()]);

        // Plug in a new-dim model: meta updated + stale vectors dropped.
        db.reconcile_embedding_dim(768).unwrap();
        {
            let conn = db.connect().unwrap();
            assert_eq!(
                read_meta(&conn, EMBEDDING_DIM_META_KEY).unwrap().as_deref(),
                Some("768")
            );
        }
        assert!(
            db.vec_symbol_ids().unwrap().is_empty(),
            "stale-dim vectors dropped on recreate"
        );

        // The recreated fallback table accepts a new-dim vector.
        {
            let conn = db.connect().unwrap();
            conn.execute(
                "INSERT INTO symbol_vec(symbol_id, embedding) VALUES('s:a', ?1)",
                rusqlite::params![floats_to_le_bytes(&[0.0f32; 768])],
            )
            .unwrap();
        }

        // Idempotent: same dim again is a no-op (does NOT drop the table).
        db.reconcile_embedding_dim(768).unwrap();
        assert_eq!(
            db.vec_symbol_ids().unwrap(),
            vec!["s:a".to_string()],
            "no-op reconcile keeps existing rows"
        );

        db.close_thread_connection();
    }

    // ----- build_code_graph (task 3.5) ------------------------------------

    /// Assert two f64 slices agree within the CSAR L1 tolerance (Req 4.3),
    /// avoiding spurious failures from float summation order (e.g. 0.9 + 0.8).
    fn assert_l1_close(actual: &[f64], expected: &[f64]) {
        assert_eq!(actual.len(), expected.len(), "length mismatch");
        let l1: f64 = actual
            .iter()
            .zip(expected)
            .map(|(a, b)| (a - b).abs())
            .sum();
        assert!(
            l1 < 1e-9,
            "L1 {l1} exceeds tol\n  actual:   {actual:?}\n  expected: {expected:?}"
        );
    }

    /// Insert an edge directly (so meta can carry `dst_missing`), bypassing the
    /// `SymbolWriter` so dangling endpoints are allowed in the test fixtures.
    fn insert_edge(conn: &Connection, src: &str, dst: &str, kind: &str, conf: f64, meta: &str) {
        conn.execute(
            "INSERT INTO edge(src_id, dst_id, kind, confidence, meta) \
             VALUES(?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![src, dst, kind, conf, meta],
        )
        .unwrap();
    }

    /// The core invariants of `build_code_graph` on a hand-built graph:
    /// symmetrization, parallel-edge weight summing, ascending neighbour order,
    /// weighted degree, `dst_missing` + dangling + self-edge exclusion, and a
    /// self-loop (degree 1.0) for an isolated node.
    #[test]
    fn build_code_graph_symmetrizes_and_handles_special_cases() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = Database::open(dir.path().join("g.db")).unwrap();

        // Nodes a,b,c,d inserted in this order ⇒ indices 0,1,2,3.
        db.upsert_symbols(&[
            sym("s:a", "aaa"),
            sym("s:b", "bbb"),
            sym("s:c", "ccc"),
            sym("s:d", "ddd"),
        ])
        .unwrap();

        {
            let conn = db.connect().unwrap();
            // a->b calls 0.5 and a->b imports 0.4 ⇒ parallel edges sum to 0.9.
            insert_edge(&conn, "s:a", "s:b", "calls", 0.5, "{}");
            insert_edge(&conn, "s:a", "s:b", "imports", 0.4, "{}");
            // b->c calls 0.8.
            insert_edge(&conn, "s:b", "s:c", "calls", 0.8, "{}");
            // a->a self-edge: excluded (only isolated nodes get self-loops).
            insert_edge(&conn, "s:a", "s:a", "calls", 1.0, "{}");
            // a->gone: dangling endpoint (gone not a node) ⇒ excluded.
            insert_edge(&conn, "s:a", "s:gone", "calls", 1.0, "{}");
            // c->gone imports, flagged dst_missing ⇒ excluded.
            insert_edge(
                &conn,
                "s:c",
                "s:gone",
                "imports",
                1.0,
                r#"{"dst_missing":true}"#,
            );
            // b->c with non-positive weight ⇒ excluded (no effect on the 0.8).
            insert_edge(&conn, "s:c", "s:b", "references", 0.0, "{}");
        }

        let g = db.build_code_graph(None).unwrap();

        // 4 nodes in insertion order; index is the inverse map.
        assert_eq!(g.node_ids, vec!["s:a", "s:b", "s:c", "s:d"]);
        assert_eq!(g.n(), 4);
        assert_eq!(g.index["s:c"], 2);

        // Adjacency (symmetric): a<->b (0.9), b<->c (0.8), d isolated self-loop.
        //   row a: [(b,0.9)]            row b: [(a,0.9),(c,0.8)]
        //   row c: [(b,0.8)]            row d: [(d,1.0)]
        assert_eq!(g.indptr, vec![0, 1, 3, 4, 5]);
        assert_eq!(g.indices, vec![1, 0, 2, 1, 3]);
        assert_l1_close(&g.weights, &[0.9, 0.9, 0.8, 0.8, 1.0]);
        // Weighted degree = column sums; isolated d ⇒ 1.0.
        assert_l1_close(&g.degree, &[0.9, 1.7, 0.8, 1.0]);

        // Per-row neighbour slices are sorted ascending.
        assert_eq!(g.neighbors(1).0, &[0i32, 2][..]);
        assert_l1_close(g.neighbors(1).1, &[0.9, 0.8]);

        db.close_thread_connection();
    }

    /// The `kinds` argument is an edge-**kind** whitelist (mirrors Python
    /// `edge_kinds`): filtering to a kind with no surviving edges leaves every
    /// node isolated (all self-loops, degree 1.0).
    #[test]
    fn build_code_graph_edge_kind_filter() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = Database::open(dir.path().join("gk.db")).unwrap();

        db.upsert_symbols(&[sym("s:a", "aaa"), sym("s:b", "bbb")])
            .unwrap();
        {
            let conn = db.connect().unwrap();
            insert_edge(&conn, "s:a", "s:b", "calls", 0.7, "{}");
        }

        // Whitelisting "calls" keeps the edge ⇒ a<->b symmetrized.
        let calls = db.build_code_graph(Some(&["calls".to_string()])).unwrap();
        assert_eq!(calls.indptr, vec![0, 1, 2]);
        assert_eq!(calls.indices, vec![1, 0]);
        assert_eq!(calls.weights, vec![0.7, 0.7]);
        assert_eq!(calls.degree, vec![0.7, 0.7]);

        // Whitelisting only "imports" drops the calls edge ⇒ both isolated.
        let imports = db.build_code_graph(Some(&["imports".to_string()])).unwrap();
        assert_eq!(imports.indptr, vec![0, 1, 2]);
        assert_eq!(imports.indices, vec![0, 1], "self-loops on isolated nodes");
        assert_eq!(imports.weights, vec![1.0, 1.0]);
        assert_eq!(imports.degree, vec![1.0, 1.0]);

        db.close_thread_connection();
    }

    /// An empty DB yields an empty graph (no nodes, just the `indptr` sentinel).
    #[test]
    fn build_code_graph_empty_db() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(dir.path().join("ge.db")).unwrap();
        let g = db.build_code_graph(None).unwrap();
        assert!(g.is_empty());
        assert_eq!(g.n(), 0);
        assert_eq!(g.nnz(), 0);
        assert_eq!(g.indptr, vec![0]);
        assert!(g.indices.is_empty() && g.weights.is_empty() && g.degree.is_empty());
        db.close_thread_connection();
    }

    // ----- heal_legacy_vec0 (indexd-vec0-legacy-crash task 3.1) -----------

    /// Rewrite the connection's plain-BLOB `symbol_vec` into a legacy `vec0`
    /// virtual table plus `symbol_vec_*` shadow tables — module-free, exactly
    /// how an older sqlite-vec engine leaves it on disk. After this, the schema
    /// probe sees a `vec0` table while any live `symbol_vec` query (or a real
    /// `DROP`) raises `no such module: vec0`.
    fn craft_legacy_vec0(conn: &Connection) {
        conn.execute_batch(
            "DROP TABLE IF EXISTS symbol_vec;\n\
             CREATE TABLE IF NOT EXISTS symbol_vec_chunks(chunk_id INTEGER PRIMARY KEY, data BLOB);\n\
             CREATE TABLE IF NOT EXISTS symbol_vec_rowids(rowid INTEGER PRIMARY KEY, symbol_id TEXT);\n\
             INSERT INTO symbol_vec_chunks(chunk_id, data) VALUES (1, x'00');\n\
             INSERT INTO symbol_vec_rowids(rowid, symbol_id) VALUES (1, 's:legacy');\n\
             PRAGMA writable_schema=ON;\n\
             INSERT INTO sqlite_master(type,name,tbl_name,rootpage,sql) \
                 VALUES('table','symbol_vec','symbol_vec',0,\
                 'CREATE VIRTUAL TABLE symbol_vec USING vec0(symbol_id TEXT PRIMARY KEY, embedding FLOAT[384])');\n\
             PRAGMA writable_schema=OFF;",
        )
        .unwrap();
    }

    /// `heal_legacy_vec0` on a crafted legacy-`vec0` DB (this build has no
    /// sqlite-vec module) converts `symbol_vec` back to the plain-BLOB fallback,
    /// preserves all indexed symbol data, drops only the `symbol_vec_*` shadow
    /// tables, and is idempotent on a second call.
    #[test]
    fn heal_legacy_vec0_converts_to_blob_preserves_data_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(dir.path().join("legacy.db")).unwrap();
        let conn = db.connect().unwrap();

        // Seed real UCKG data that must survive the heal.
        db_insert_symbols(&conn, &["s:a", "s:b", "s:c"]);
        insert_edge(&conn, "s:a", "s:b", "calls", 1.0, "{}");
        // A same-prefix-but-unrelated table must NOT be dropped: `symbol` and
        // `symbol_attribute` don't match `symbol_vec_%`, but assert the guard
        // by keeping a `symbol_attribute` row too.
        conn.execute(
            "INSERT INTO symbol_attribute(symbol_id, key, value) VALUES('s:a','k','v')",
            [],
        )
        .unwrap();

        // Turn symbol_vec into a legacy vec0 vtable + shadow tables.
        craft_legacy_vec0(&conn);
        assert!(
            vec0_table_present(&conn).unwrap(),
            "fixture must present as a vec0 table"
        );
        // A live query against the vtable really does hit the missing module.
        assert!(conn
            .query_row("SELECT COUNT(*) FROM symbol_vec", [], |r| r
                .get::<_, i64>(0))
            .is_err());

        // Heal.
        heal_legacy_vec0(&conn).unwrap();

        // symbol_vec is now the plain-BLOB fallback: queryable, empty, no vec0.
        assert!(
            !vec0_table_present(&conn).unwrap(),
            "symbol_vec must be plain BLOB after heal"
        );
        let vec_sql: String = conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE name = 'symbol_vec'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            vec_sql.to_ascii_uppercase().contains("BLOB")
                && !vec_sql.to_ascii_uppercase().contains("USING VEC0"),
            "recreated in migration-001 BLOB shape: {vec_sql}"
        );
        assert!(
            db.vec_symbol_ids().unwrap().is_empty(),
            "BLOB table is empty"
        );

        // Indexed symbol data preserved; only symbol_vec_* shadow tables gone.
        assert_eq!(db.count("symbol").unwrap(), 3);
        assert_eq!(db.count("edge").unwrap(), 1);
        assert_eq!(db.count("symbol_attribute").unwrap(), 1, "attr preserved");
        let shadow_left: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master \
                 WHERE type = 'table' AND name LIKE 'symbol_vec\\_%' ESCAPE '\\'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(shadow_left, 0, "symbol_vec_* shadow tables removed");

        // Idempotent: a second heal is a guarded no-op on the plain-BLOB table.
        heal_legacy_vec0(&conn).unwrap();
        assert!(!vec0_table_present(&conn).unwrap());
        assert_eq!(db.count("symbol").unwrap(), 3);

        db.close_thread_connection();
    }

    /// A non-legacy DB (plain-BLOB `symbol_vec`) is left byte-for-byte
    /// untouched by `heal_legacy_vec0` (Preservation guard).
    #[test]
    fn heal_legacy_vec0_is_noop_on_plain_blob_db() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(dir.path().join("plain.db")).unwrap();
        let conn = db.connect().unwrap();
        db_insert_symbols(&conn, &["s:a"]);
        conn.execute(
            "INSERT INTO symbol_vec(symbol_id, embedding) VALUES('s:a', ?1)",
            rusqlite::params![floats_to_le_bytes(&[1.0, 2.0])],
        )
        .unwrap();

        let before: String = conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE name = 'symbol_vec'",
                [],
                |r| r.get(0),
            )
            .unwrap();

        heal_legacy_vec0(&conn).unwrap();

        let after: String = conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE name = 'symbol_vec'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(before, after, "plain-BLOB symbol_vec unchanged");
        assert_eq!(db.vec_symbol_ids().unwrap(), vec!["s:a".to_string()]);
        db.close_thread_connection();
    }

    /// Graceful-ignore fallback (indexd-vec0-legacy-crash task 3.4, Req 2.1/2.5):
    /// when the heal-on-open cannot run (e.g. a read-only DB) the `symbol_vec`
    /// stays a legacy `vec0` virtual table this build can't read. The open path
    /// must still succeed and every downstream read that touches `symbol_vec`
    /// rows must degrade to empty rather than propagate the fatal
    /// `no such module: vec0` error — matching `vec_search`'s degrade-to-empty
    /// contract and `check_vector`'s warn mapping.
    ///
    /// We simulate "heal couldn't run" by crafting the legacy `vec0` shape onto
    /// a live connection *after* open (so no heal reran over it), then assert
    /// the reads degrade over the still-unhealed vtable.
    #[test]
    fn reads_degrade_to_empty_when_legacy_vec0_cannot_be_healed() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(dir.path().join("unhealed.db")).unwrap();
        let conn = db.connect().unwrap();

        // Real UCKG data that must remain readable while the vector leg degrades.
        db_insert_symbols(&conn, &["s:a", "s:b"]);
        insert_edge(&conn, "s:a", "s:b", "calls", 1.0, "{}");

        // Leave the connection holding a legacy `vec0` `symbol_vec` (as if the
        // heal was skipped on a read-only DB).
        craft_legacy_vec0(&conn);
        assert!(
            vec0_table_present(&conn).unwrap(),
            "fixture must present as an unhealed vec0 table"
        );
        // A raw live query really does hit the missing module (the crash surface
        // the graceful reads must absorb).
        assert!(
            conn.query_row("SELECT COUNT(*) FROM symbol_vec", [], |r| r
                .get::<_, i64>(0))
                .is_err(),
            "a raw vec0 query must raise `no such module: vec0`"
        );

        // Downstream reads degrade to empty / zero instead of propagating.
        assert!(
            db.vec_symbol_ids().unwrap().is_empty(),
            "vec_symbol_ids degrades to empty on an unhealed legacy vec0 table"
        );
        assert_eq!(
            db.vec_row_count().unwrap(),
            0,
            "vec_row_count degrades to 0 on an unhealed legacy vec0 table"
        );
        assert!(
            db.vec_search(&[1.0, 0.0, 0.0], 5).unwrap().is_empty(),
            "vec_search degrades to empty on an unhealed legacy vec0 table"
        );

        // Non-vector symbol data stays fully readable (data preserved).
        assert_eq!(db.count("symbol").unwrap(), 2);
        assert_eq!(db.count("edge").unwrap(), 1);

        db.close_thread_connection();
    }

    /// Craft the legacy `vec0` shape directly on disk via a RAW rusqlite
    /// connection (never through `Database::open`, so no heal runs over it),
    /// exactly how an older sqlite-vec engine leaves the file. `path` must be an
    /// already-migrated DB with real UCKG rows to preserve.
    fn craft_legacy_vec0_on_disk(path: &std::path::Path) {
        let raw = Connection::open(path).unwrap();
        craft_legacy_vec0(&raw);
        assert!(
            vec0_table_present(&raw).unwrap(),
            "on-disk fixture must present as a vec0 table before open-path heal"
        );
        // `raw` drops at end of scope, releasing the file handle (Windows) and
        // checkpointing the crafted schema into the DB file.
    }

    /// End-to-end open-path heal (Task 4.1, Req 2.1): a legacy-`vec0` fixture
    /// that exists on disk *before* the store opens it heals through the shared
    /// `open_new_connection` path (not by calling `heal_legacy_vec0` directly).
    /// `Database::open` returns `Ok` and leaves a queryable plain-BLOB
    /// `symbol_vec`, with all indexed symbol data preserved.
    #[test]
    fn open_new_connection_heals_legacy_vec0_fixture_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("open_heal.db");

        // 1. Build a real migrated DB file + seed UCKG data, then release it.
        {
            let db = Database::open(&path).unwrap();
            let conn = db.connect().unwrap();
            db_insert_symbols(&conn, &["s:a", "s:b", "s:c"]);
            insert_edge(&conn, "s:a", "s:b", "calls", 1.0, "{}");
            db.close_thread_connection();
        }

        // 2. Turn the on-disk `symbol_vec` into a legacy `vec0` vtable (no heal).
        craft_legacy_vec0_on_disk(&path);

        // 3. Open through the shared store path: `open_new_connection` runs
        //    `run_migrations` then `heal_legacy_vec0`, so the fixture heals
        //    end-to-end and open returns Ok (no `no such module: vec0`).
        let db = Database::open(&path).unwrap();
        let conn = db.connect().unwrap();

        assert!(
            !vec0_table_present(&conn).unwrap(),
            "symbol_vec must be plain BLOB after heal-on-open"
        );
        // A live query that would have raised on the vtable now succeeds & is empty.
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM symbol_vec", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0, "healed BLOB symbol_vec is queryable and empty");
        assert!(db.vec_symbol_ids().unwrap().is_empty());

        // Indexed symbol data preserved across the on-disk heal.
        assert_eq!(db.count("symbol").unwrap(), 3);
        assert_eq!(db.count("edge").unwrap(), 1);
        // Shadow tables removed by the open-path heal.
        let shadow_left: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master \
                 WHERE type = 'table' AND name LIKE 'symbol_vec\\_%' ESCAPE '\\'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(shadow_left, 0, "symbol_vec_* shadow tables removed on open");

        db.close_thread_connection();
    }

    /// After the open-path heal (Task 4.1, Req 2.2, 2.4), `reconcile_embedding_dim`
    /// sees a plain-BLOB `symbol_vec`, so its drop/recreate is an ordinary table
    /// operation with no `no such module: vec0` error — a dim change works
    /// cleanly post-heal and the recreated table accepts a new-dim vector.
    #[test]
    fn reconcile_embedding_dim_after_heal_on_open_is_plain_blob_drop_recreate() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("reconcile_heal.db");

        {
            let db = Database::open(&path).unwrap();
            let conn = db.connect().unwrap();
            db_insert_symbols(&conn, &["s:a"]);
            db.close_thread_connection();
        }
        craft_legacy_vec0_on_disk(&path);

        // Heal on open: symbol_vec is now plain BLOB.
        let mut db = Database::open(&path).unwrap();
        assert!(
            !vec0_table_present(&db.connect().unwrap()).unwrap(),
            "heal-on-open leaves a plain-BLOB symbol_vec"
        );

        // Dim change: ordinary BLOB drop/recreate, no vec0 module ever touched.
        db.reconcile_embedding_dim(768).unwrap();
        {
            let conn = db.connect().unwrap();
            assert_eq!(
                read_meta(&conn, EMBEDDING_DIM_META_KEY).unwrap().as_deref(),
                Some("768"),
                "reconcile persisted the new dim"
            );
            assert!(
                !vec0_table_present(&conn).unwrap(),
                "table stays plain BLOB after reconcile"
            );
            // The recreated fallback table accepts a new-dim vector.
            conn.execute(
                "INSERT INTO symbol_vec(symbol_id, embedding) VALUES('s:a', ?1)",
                rusqlite::params![floats_to_le_bytes(&[0.0f32; 768])],
            )
            .unwrap();
        }
        assert_eq!(db.vec_symbol_ids().unwrap(), vec!["s:a".to_string()]);

        db.close_thread_connection();
    }

    /// Data-safety guard (Task 4.1, Req 2.4): the heal removes ONLY the
    /// `symbol_vec` vtable and its `symbol_vec_%` shadow tables. Tables whose
    /// names merely start with `symbol_vec` but are not shadow tables (e.g.
    /// `symbol_vector`, `symbolic_vec`) and the core UCKG tables (`symbol`,
    /// `symbol_attribute`, `symbol_fts`) are left fully intact with their rows.
    #[test]
    fn heal_legacy_vec0_never_removes_tables_outside_symbol_vec_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(dir.path().join("safety.db")).unwrap();
        let conn = db.connect().unwrap();

        // Core UCKG data (decoys the heal must never touch).
        db_insert_symbols(&conn, &["s:a", "s:b"]);
        conn.execute(
            "INSERT INTO symbol_attribute(symbol_id, key, value) VALUES('s:a','k','v')",
            [],
        )
        .unwrap();

        // Red-herring tables: names start with `symbol_vec`/`symbol` but do NOT
        // match the escaped `symbol_vec\_%` prefix, so they MUST survive.
        conn.execute_batch(
            "CREATE TABLE symbol_vector(id TEXT PRIMARY KEY, note TEXT);\n\
             INSERT INTO symbol_vector(id, note) VALUES('keep', 'red-herring');\n\
             CREATE TABLE symbolic_vec(id TEXT PRIMARY KEY);\n\
             INSERT INTO symbolic_vec(id) VALUES('keep2');",
        )
        .unwrap();

        // Legacy vec0 vtable + real shadow tables (symbol_vec_chunks / _rowids
        // from craft, plus an extra vector-chunks shadow) that MUST be dropped.
        craft_legacy_vec0(&conn);
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS symbol_vec_vector_chunks00(rowid INTEGER PRIMARY KEY, vectors BLOB);\n\
             INSERT INTO symbol_vec_vector_chunks00(rowid, vectors) VALUES(1, x'00');",
        )
        .unwrap();

        heal_legacy_vec0(&conn).unwrap();

        // Every `symbol_vec_%` shadow table is gone.
        let shadow_left: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master \
                 WHERE type = 'table' AND name LIKE 'symbol_vec\\_%' ESCAPE '\\'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(shadow_left, 0, "only symbol_vec_* shadow tables removed");

        // Red-herring same-prefix tables survive with their rows.
        assert_eq!(db.count("symbol_vector").unwrap(), 1, "symbol_vector kept");
        assert_eq!(db.count("symbolic_vec").unwrap(), 1, "symbolic_vec kept");

        // Core UCKG tables + rows intact; symbol_fts table still present.
        assert_eq!(db.count("symbol").unwrap(), 2);
        assert_eq!(db.count("symbol_attribute").unwrap(), 1);
        let fts_present: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master \
                 WHERE type = 'table' AND name = 'symbol_fts'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(fts_present, 1, "symbol_fts preserved");

        db.close_thread_connection();
    }

    /// Insert minimal valid `symbol` rows for the given ids (satisfies the NOT
    /// NULL columns so FK-bearing inserts succeed).
    fn db_insert_symbols(conn: &Connection, ids: &[&str]) {
        for id in ids {
            conn.execute(
                "INSERT INTO symbol(id, kind, name, qualified_name, language, module, \
                 file_path, line_start, line_end, content_hash, updated_at) \
                 VALUES(?1,'function',?1,?1,'rust','m','m.rs',1,1,'h',0)",
                rusqlite::params![id],
            )
            .unwrap();
        }
    }

    // ------------------------------------------------------------------
    // reconcile-on-open — safe index self-recovery
    // ------------------------------------------------------------------

    /// (a) A DB stamped by a strictly-newer engine (`schema_version = 999`,
    /// with seeded rows) is SAFELY reset on re-open: `Database::open` returns
    /// `Ok`, the schema is downgraded to `LATEST_SCHEMA_VERSION`, and every
    /// schema table exists, is queryable, and is empty (a later index pass
    /// repopulates). The `.cognis` DB file itself is preserved.
    #[test]
    fn reconcile_on_open_resets_future_schema_db_safely() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("future.db");

        // Build a normal DB, seed data, then stamp a FUTURE schema version.
        {
            let db = Database::open(&path).unwrap();
            let conn = db.connect().unwrap();
            db_insert_symbols(&conn, &["s:a", "s:b"]);
            insert_edge(&conn, "s:a", "s:b", "calls", 1.0, "{}");
            conn.execute(
                "INSERT INTO symbol_vec(symbol_id, embedding) VALUES('s:a', ?1)",
                rusqlite::params![floats_to_le_bytes(&[1.0, 2.0])],
            )
            .unwrap();
            write_meta(&conn, "schema_version", "999").unwrap();
            db.close_thread_connection();
        }

        // Re-open: reconcile-on-open sees schema_version > LATEST and resets.
        let db = Database::open(&path).unwrap();
        assert_eq!(
            db.schema_version().unwrap(),
            LATEST_SCHEMA_VERSION,
            "future schema is safely downgraded to the current schema"
        );

        // Every schema table exists, is queryable, and is empty.
        for table in ["symbol", "edge", "symbol_attribute", "file", "symbol_vec"] {
            assert_eq!(
                db.count(table).unwrap(),
                0,
                "table {table} exists and is empty after safe reset"
            );
        }
        // FTS + vector legs are queryable (empty) too.
        assert!(db.fts_match_ids("anything", 5).unwrap().is_empty());
        assert!(db.vec_symbol_ids().unwrap().is_empty());
        // symbol_vec was recreated in the plain-BLOB shape (not a vec0 vtable).
        assert!(!vec0_table_present(&db.connect().unwrap()).unwrap());

        db.close_thread_connection();
    }

    /// (b) A normal DB at `LATEST_SCHEMA_VERSION` with seeded symbols is NOT
    /// reset by reconcile-on-open — the symbol count is preserved — and a
    /// second open is idempotent (still preserved, still at LATEST).
    #[test]
    fn reconcile_on_open_does_not_reset_current_schema_db() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("current.db");

        {
            let db = Database::open(&path).unwrap();
            let conn = db.connect().unwrap();
            db_insert_symbols(&conn, &["s:a", "s:b", "s:c"]);
            assert_eq!(db.schema_version().unwrap(), LATEST_SCHEMA_VERSION);
            db.close_thread_connection();
        }

        // First re-open: no reset, rows preserved.
        {
            let db = Database::open(&path).unwrap();
            assert_eq!(db.schema_version().unwrap(), LATEST_SCHEMA_VERSION);
            assert_eq!(
                db.count("symbol").unwrap(),
                3,
                "no reset at schema <= LATEST"
            );
            db.close_thread_connection();
        }

        // Second re-open: idempotent — still preserved.
        {
            let db = Database::open(&path).unwrap();
            assert_eq!(db.schema_version().unwrap(), LATEST_SCHEMA_VERSION);
            assert_eq!(
                db.count("symbol").unwrap(),
                3,
                "idempotent — still preserved"
            );
            db.close_thread_connection();
        }
    }

    /// (c) `meta.index_version` equals `env!("CARGO_PKG_VERSION")` after any
    /// open — reconcile-on-open stamps the engine build idempotently, including
    /// on a plain DB that is otherwise untouched.
    #[test]
    fn reconcile_on_open_stamps_index_version() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stamp.db");

        let db = Database::open(&path).unwrap();
        let conn = db.connect().unwrap();
        assert_eq!(
            read_meta(&conn, "index_version").unwrap().as_deref(),
            Some(env!("CARGO_PKG_VERSION")),
            "engine build stamped on open"
        );
        db.close_thread_connection();

        // Stamp an arbitrary stale value, then re-open: the stamp is refreshed.
        {
            let db = Database::open(&path).unwrap();
            let conn = db.connect().unwrap();
            write_meta(&conn, "index_version", "0.0.0-stale").unwrap();
            db.close_thread_connection();
        }
        let db = Database::open(&path).unwrap();
        let conn = db.connect().unwrap();
        assert_eq!(
            read_meta(&conn, "index_version").unwrap().as_deref(),
            Some(env!("CARGO_PKG_VERSION")),
            "re-open refreshes the engine build stamp"
        );
        db.close_thread_connection();
    }
}
