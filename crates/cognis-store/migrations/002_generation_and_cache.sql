-- cognis migration 002 — generation markers, publish audit, durable embedding
-- queue, vector-space identity, and the bounded-read indexes.
--
-- Spec: .kiro/specs/cognis-one-toggle-engine (Requirements 7.8, 9.6, 22.5, 22.6).
--
-- IMPORTANT: `001_initial.sql` is FROZEN. Every schema change this feature needs
-- ships here, as a new numbered migration, and 001 is left byte-identical
-- (Requirement 7.8, 22.5).
--
-- Every statement below is `CREATE ... IF NOT EXISTS` (and the single seed row is
-- `INSERT OR IGNORE`), so `run_migrations` stays idempotent and a pre-feature
-- database is upgraded IN PLACE with every existing symbol, edge, FTS row and
-- vector preserved (Requirement 22.6). Nothing here drops, renames, or rewrites a
-- table that 001 created.

-- ---------------------------------------------------------------------------
-- generation — committed generation markers. Single row (id = 1).
--
-- `source_generation` is advanced by the Index_Publisher inside the same
-- transaction as the row mutations of a Publish_Batch (Requirement 7.2);
-- `semantic_generation` only catches up when the embedding queue is empty
-- (Requirement 7.6, 7.7, 11.5).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS generation (
  id                  INTEGER PRIMARY KEY CHECK (id = 1),
  source_generation   INTEGER NOT NULL DEFAULT 0,
  semantic_generation INTEGER NOT NULL DEFAULT 0,
  updated_at          INTEGER NOT NULL DEFAULT 0
);
INSERT OR IGNORE INTO generation(id, source_generation, semantic_generation, updated_at)
VALUES (1, 0, 0, 0);

-- ---------------------------------------------------------------------------
-- publish_batch / publish_batch_file — what each committed generation covered
-- (Requirement 7.5).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS publish_batch (
  source_generation INTEGER PRIMARY KEY,
  committed_at      INTEGER NOT NULL,
  file_count        INTEGER NOT NULL,
  reason            TEXT    NOT NULL   -- initial | incremental | reconcile | full | reembed
);
CREATE TABLE IF NOT EXISTS publish_batch_file (
  source_generation INTEGER NOT NULL,
  path              TEXT    NOT NULL,
  change            TEXT    NOT NULL,  -- added | changed | removed
  PRIMARY KEY (source_generation, path)
);

-- ---------------------------------------------------------------------------
-- embedding_queue — durable pending embedding work (Requirement 11).
--
-- One row per symbol still awaiting a vector. `pending_vector_count` is literally
-- `SELECT COUNT(*) FROM embedding_queue`; a failed embed retains its row and
-- advances `attempts` / `next_attempt_at` on the bounded backoff ladder
-- (Requirement 11.6, 11.7).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS embedding_queue (
  symbol_id       TEXT PRIMARY KEY,
  file_path       TEXT    NOT NULL,
  enqueued_at     INTEGER NOT NULL,
  attempts        INTEGER NOT NULL DEFAULT 0,
  next_attempt_at INTEGER NOT NULL DEFAULT 0,
  last_error      TEXT
);
CREATE INDEX IF NOT EXISTS idx_embedding_queue_file ON embedding_queue(file_path);
CREATE INDEX IF NOT EXISTS idx_embedding_queue_next ON embedding_queue(next_attempt_at);

-- ---------------------------------------------------------------------------
-- vector_space — persisted embedding-space identity (Requirement 14).
-- Single row (id = 1): the fingerprint of the model that produced the vectors
-- currently stored in `symbol_vec`, established on the first vector write and
-- replaced atomically by a re-embed batch.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS vector_space (
  id             INTEGER PRIMARY KEY CHECK (id = 1),
  fingerprint    TEXT    NOT NULL,
  embedding_dim  INTEGER NOT NULL,
  model_id       TEXT    NOT NULL,
  established_at INTEGER NOT NULL
);

-- ---------------------------------------------------------------------------
-- Indexes required by the bounded-read paths (Requirement 9.6).
-- idx_symbol_file / idx_symbol_qname / idx_edge_src / idx_edge_dst already ship
-- in 001; these complete the required set checked by
-- `missing_required_indexes()`.
-- ---------------------------------------------------------------------------
CREATE INDEX IF NOT EXISTS idx_symbol_name       ON symbol(name);
CREATE INDEX IF NOT EXISTS idx_symbol_name_lower ON symbol(lower(name));
CREATE INDEX IF NOT EXISTS idx_file_parse_status ON file(parse_status);
