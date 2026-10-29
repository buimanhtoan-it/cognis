//! `Database_Digest` — deterministic, order-independent index comparison.
//!
//! Spec: `.kiro/specs/cognis-one-toggle-engine` (Requirements 12.2–12.4, 24.2).
//!
//! The digest is what makes "incremental indexing equals a clean rebuild"
//! (Requirement 12.1, property P5) a checkable claim rather than an assertion.
//! Each category produces a set of canonical row strings; the set is sorted
//! lexicographically and hashed with SHA-256; `combined` hashes the category
//! digests in fixed order. Sorting is what makes the digest independent of
//! insertion order (Requirement 12.4).
//!
//! # What is included, and why
//!
//! | Category | Included | Excluded |
//! | --- | --- | --- |
//! | `symbols` | every `symbol` column that describes content | `updated_at` |
//! | `edges` | `src_id`, `dst_id`, `kind`, `confidence`, normalised `meta` | — |
//! | `fts` | `id`, `name`, `qualified_name`, `signature`, `docstring`, `body_excerpt` | `rank` |
//! | `vector_ids` | the **set of symbol ids** present in `symbol_vec` | the float payloads |
//! | `files` | `path`, `language`, `size_bytes`, `content_hash`, `parse_status` | `parsed_at` |
//! | (global) | — | `generation.*`, `publish_batch*`, `embedding_queue`, `vector_space`, wall-clock timestamps |
//!
//! Vector *payloads* are excluded deliberately: they are a function of the
//! model, not of the indexing path, so including them would make the digest fail
//! for reasons unrelated to what Requirement 12 asserts. Vector *presence* is
//! included, because presence is exactly the incremental-vs-rebuild difference
//! that matters.
//!
//! `diff()` is implemented separately in task 3.2.

use std::collections::BTreeSet;

use cognis_core::{CognisError, Result};
use rusqlite::{Connection, Row};
use serde::Serialize;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::{snapshot::GenerationSnapshot, try_load_vec_extension, vec0_table_present, Database};

/// The six digests of one database.
///
/// Every field is a lowercase hex SHA-256. `combined` is the hash of the five
/// category digests in the fixed order [`DatabaseDigest::CATEGORIES`] declares,
/// so two databases are equal iff their `combined` values are equal, and a
/// difference can be localised to a category without recomputing anything.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DatabaseDigest {
    pub symbols: String,
    pub edges: String,
    pub fts: String,
    pub vector_ids: String,
    pub files: String,
    /// SHA-256 over the five category digests, in `CATEGORIES` order.
    pub combined: String,
}

impl DatabaseDigest {
    /// The category names, in the fixed order `combined` folds them.
    ///
    /// This order is part of the digest definition: changing it changes every
    /// `combined` value, so it is declared once here rather than open-coded at
    /// the hashing site.
    pub const CATEGORIES: [&'static str; 5] = ["symbols", "edges", "fts", "vector_ids", "files"];

    /// This digest's value for `category`, or `None` for a name that is not one
    /// of [`CATEGORIES`](DatabaseDigest::CATEGORIES).
    pub fn category(&self, category: &str) -> Option<&str> {
        match category {
            "symbols" => Some(&self.symbols),
            "edges" => Some(&self.edges),
            "fts" => Some(&self.fts),
            "vector_ids" => Some(&self.vector_ids),
            "files" => Some(&self.files),
            _ => None,
        }
    }

    /// The categories in which `self` and `other` disagree, in `CATEGORIES`
    /// order. Empty iff the two digests are equal.
    ///
    /// This is the cheap half of divergence diagnosis (Requirement 24.2): it
    /// names *which* categories differ without opening either database again.
    /// [`diff`] names the differing rows.
    pub fn differing_categories(&self, other: &DatabaseDigest) -> Vec<&'static str> {
        DatabaseDigest::CATEGORIES
            .iter()
            .copied()
            .filter(|name| self.category(name) != other.category(name))
            .collect()
    }
}

/// One category's divergence between two databases (Requirement 24.2).
///
/// `only_left` / `only_right` carry row *identifiers* — symbol ids, `(src, dst,
/// kind)` edge keys, file paths — not whole rows, so a counterexample is
/// readable and a large divergence does not produce an unusable diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DigestDiff {
    /// One of [`DatabaseDigest::CATEGORIES`].
    pub category: &'static str,
    /// Row identifiers present only in the left database, sorted.
    pub only_left: Vec<String>,
    /// Row identifiers present only in the right database, sorted.
    pub only_right: Vec<String>,
}

impl DigestDiff {
    /// Whether this category actually diverges. A [`DigestDiff`] with both sides
    /// empty means the category's rows are the same set but at least one row's
    /// *content* differs, which is still a divergence — so this is
    /// deliberately not the same question as "is this category equal".
    pub fn is_disjointly_empty(&self) -> bool {
        self.only_left.is_empty() && self.only_right.is_empty()
    }
}

/// Compute the [`DatabaseDigest`] of `db` (Requirement 12.2–12.4).
///
/// Canonical rows are JSON arrays rather than delimiter-joined strings, so
/// values containing separators cannot collide. All five category reads use
/// the same connection and only query the columns named in the digest design;
/// generation, publication, queue, vector-space, and timestamp state therefore
/// cannot affect the result.
pub fn database_digest(db: &Database) -> Result<DatabaseDigest> {
    let snapshot = GenerationSnapshot::open(db)?;
    let conn = snapshot.connection();

    let symbols = hash_rows(query_rows(
        conn,
        "SELECT id, kind, name, qualified_name, language, module, file_path, \
                line_start, line_end, signature, docstring, content_hash, \
                body_excerpt, semantic_summary, risk_score, ambiguous, \
                untrusted_flags \
           FROM symbol",
        symbol_row,
    )?);
    let edges = hash_rows(query_rows(
        conn,
        "SELECT src_id, dst_id, kind, confidence, meta FROM edge",
        edge_row,
    )?);
    let fts = hash_rows(query_rows(
        conn,
        "SELECT id, name, qualified_name, signature, docstring, body_excerpt \
           FROM symbol_fts",
        fts_row,
    )?);
    let vector_ids = hash_rows(
        vector_ids(conn)?
            .into_iter()
            .map(|id| canonical_row(vec![Value::String(id)]))
            .collect::<Result<Vec<_>>>()?,
    );
    let files = hash_rows(query_rows(
        conn,
        "SELECT path, language, size_bytes, content_hash, parse_status FROM file",
        file_row,
    )?);

    let combined = hash_category_digests([&symbols, &edges, &fts, &vector_ids, &files]);
    Ok(DatabaseDigest {
        symbols,
        edges,
        fts,
        vector_ids,
        files,
        combined,
    })
}

fn vector_ids(conn: &Connection) -> Result<Vec<String>> {
    if vec0_table_present(conn)? && !try_load_vec_extension(conn) {
        return Ok(Vec::new());
    }
    let mut stmt = conn
        .prepare("SELECT symbol_id FROM symbol_vec")
        .map_err(store_err)?;
    let rows = stmt
        .query_map([], |row| row.get(0))
        .map_err(store_err)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(store_err)?;
    Ok(rows)
}

fn query_rows<F>(conn: &Connection, sql: &str, map: F) -> Result<Vec<String>>
where
    F: FnMut(&Row<'_>) -> rusqlite::Result<String>,
{
    let mut stmt = conn.prepare(sql).map_err(store_err)?;
    let rows = stmt
        .query_map([], map)
        .map_err(store_err)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(store_err)?;
    Ok(rows)
}

fn symbol_row(row: &Row<'_>) -> rusqlite::Result<String> {
    let flags = normalized_json_text(row.get::<_, Option<String>>(16)?, false)?;
    canonical_row(vec![
        text(row, 0)?,
        text(row, 1)?,
        text(row, 2)?,
        text(row, 3)?,
        text(row, 4)?,
        text(row, 5)?,
        text(row, 6)?,
        integer(row, 7)?,
        integer(row, 8)?,
        nullable_text(row, 9)?,
        nullable_text(row, 10)?,
        text(row, 11)?,
        nullable_text(row, 12)?,
        nullable_text(row, 13)?,
        real(row, 14)?,
        integer(row, 15)?,
        flags,
    ])
    .map_err(sql_conversion_error)
}

fn edge_row(row: &Row<'_>) -> rusqlite::Result<String> {
    let meta = normalized_json_text(row.get::<_, Option<String>>(4)?, true)?;
    canonical_row(vec![
        text(row, 0)?,
        text(row, 1)?,
        text(row, 2)?,
        real(row, 3)?,
        meta,
    ])
    .map_err(sql_conversion_error)
}

fn fts_row(row: &Row<'_>) -> rusqlite::Result<String> {
    canonical_row(vec![
        text(row, 0)?,
        text(row, 1)?,
        text(row, 2)?,
        nullable_text(row, 3)?,
        nullable_text(row, 4)?,
        nullable_text(row, 5)?,
    ])
    .map_err(sql_conversion_error)
}

fn file_row(row: &Row<'_>) -> rusqlite::Result<String> {
    canonical_row(vec![
        text(row, 0)?,
        text(row, 1)?,
        integer(row, 2)?,
        text(row, 3)?,
        text(row, 4)?,
    ])
    .map_err(sql_conversion_error)
}

fn text(row: &Row<'_>, index: usize) -> rusqlite::Result<Value> {
    Ok(Value::String(row.get(index)?))
}

fn nullable_text(row: &Row<'_>, index: usize) -> rusqlite::Result<Value> {
    Ok(row
        .get::<_, Option<String>>(index)?
        .map_or(Value::Null, Value::String))
}

fn integer(row: &Row<'_>, index: usize) -> rusqlite::Result<Value> {
    Ok(Value::from(row.get::<_, i64>(index)?))
}

fn real(row: &Row<'_>, index: usize) -> rusqlite::Result<Value> {
    let value = row.get::<_, f64>(index)?;
    serde_json::Number::from_f64(value)
        .map(Value::Number)
        .ok_or_else(|| sql_conversion_error("non-finite REAL in digest input"))
}

fn normalized_json_text(
    raw: Option<String>,
    empty_object_is_absent: bool,
) -> rusqlite::Result<Value> {
    let Some(raw) = raw.filter(|text| !text.is_empty()) else {
        return Ok(Value::Null);
    };
    let normalized =
        normalize_json(serde_json::from_str(&raw).map_err(|error| {
            sql_conversion_error(format!("invalid JSON in digest input: {error}"))
        })?);
    if empty_object_is_absent && matches!(&normalized, Value::Object(map) if map.is_empty()) {
        Ok(Value::Null)
    } else {
        Ok(normalized)
    }
}

fn normalize_json(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut entries = map.into_iter().collect::<Vec<_>>();
            entries.sort_unstable_by(|left, right| left.0.cmp(&right.0));
            Value::Object(
                entries
                    .into_iter()
                    .map(|(key, value)| (key, normalize_json(value)))
                    .collect::<Map<_, _>>(),
            )
        }
        Value::Array(values) => Value::Array(values.into_iter().map(normalize_json).collect()),
        other => other,
    }
}

fn canonical_row(values: Vec<Value>) -> Result<String> {
    serde_json::to_string(&values).map_err(store_err)
}

fn hash_rows(mut rows: Vec<String>) -> String {
    rows.sort_unstable();
    let mut hasher = Sha256::new();
    for row in rows {
        let bytes = row.as_bytes();
        hasher.update((bytes.len() as u64).to_be_bytes());
        hasher.update(bytes);
    }
    hex_digest(hasher.finalize())
}

fn hash_category_digests(digests: [&str; 5]) -> String {
    let mut hasher = Sha256::new();
    for digest in digests {
        hasher.update(digest.as_bytes());
    }
    hex_digest(hasher.finalize())
}

fn hex_digest(bytes: impl AsRef<[u8]>) -> String {
    bytes
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn store_err(error: impl std::fmt::Display) -> CognisError {
    CognisError::Store(error.to_string())
}

fn sql_conversion_error(error: impl std::fmt::Display) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        0,
        rusqlite::types::Type::Text,
        Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            error.to_string(),
        )),
    )
}

/// Report, per category, the row identifiers whose canonical rows occur on
/// only one side (Requirement 24.2).
///
/// Categories are returned in [`DatabaseDigest::CATEGORIES`] order and row
/// identifiers are sorted. A content mutation therefore reports the row's
/// identifier on both sides: the old canonical row exists only on `left`, and
/// the new canonical row exists only on `right`.
pub fn diff(left: &Database, right: &Database) -> Result<Vec<DigestDiff>> {
    let left = GenerationSnapshot::open(left)?;
    let right = GenerationSnapshot::open(right)?;
    let left_rows = category_rows(left.connection())?;
    let right_rows = category_rows(right.connection())?;

    Ok(DatabaseDigest::CATEGORIES
        .into_iter()
        .zip(left_rows)
        .zip(right_rows)
        .filter_map(|((category, left), right)| category_diff(category, left, right))
        .collect())
}

fn category_rows(conn: &Connection) -> Result<[Vec<(String, String)>; 5]> {
    let symbols = query_identified_rows(
        conn,
        "SELECT id, kind, name, qualified_name, language, module, file_path, \
                line_start, line_end, signature, docstring, content_hash, \
                body_excerpt, semantic_summary, risk_score, ambiguous, \
                untrusted_flags \
           FROM symbol",
        |row| row.get(0),
        symbol_row,
    )?;
    let edges = query_identified_rows(
        conn,
        "SELECT src_id, dst_id, kind, confidence, meta FROM edge",
        edge_identifier,
        edge_row,
    )?;
    let fts = query_identified_rows(
        conn,
        "SELECT id, name, qualified_name, signature, docstring, body_excerpt \
           FROM symbol_fts",
        |row| row.get(0),
        fts_row,
    )?;
    let vector_ids = vector_ids(conn)?
        .into_iter()
        .map(|id| {
            let row = canonical_row(vec![Value::String(id.clone())])?;
            Ok((row, id))
        })
        .collect::<Result<Vec<_>>>()?;
    let files = query_identified_rows(
        conn,
        "SELECT path, language, size_bytes, content_hash, parse_status FROM file",
        |row| row.get(0),
        file_row,
    )?;

    Ok([symbols, edges, fts, vector_ids, files])
}

fn query_identified_rows<I, F>(
    conn: &Connection,
    sql: &str,
    mut identify: I,
    mut canonicalize: F,
) -> Result<Vec<(String, String)>>
where
    I: FnMut(&Row<'_>) -> rusqlite::Result<String>,
    F: FnMut(&Row<'_>) -> rusqlite::Result<String>,
{
    let mut stmt = conn.prepare(sql).map_err(store_err)?;
    let rows = stmt
        .query_map([], |row| {
            let identifier = identify(row)?;
            Ok((canonicalize(row)?, identifier))
        })
        .map_err(store_err)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(store_err)?;
    Ok(rows)
}

fn edge_identifier(row: &Row<'_>) -> rusqlite::Result<String> {
    canonical_row(vec![text(row, 0)?, text(row, 1)?, text(row, 2)?]).map_err(sql_conversion_error)
}

fn category_diff(
    category: &'static str,
    mut left: Vec<(String, String)>,
    mut right: Vec<(String, String)>,
) -> Option<DigestDiff> {
    left.sort_unstable();
    right.sort_unstable();
    if left == right {
        return None;
    }

    let mut only_left = BTreeSet::new();
    let mut only_right = BTreeSet::new();
    let (mut left_index, mut right_index) = (0, 0);
    while left_index < left.len() && right_index < right.len() {
        match left[left_index].cmp(&right[right_index]) {
            std::cmp::Ordering::Less => {
                only_left.insert(left[left_index].1.clone());
                left_index += 1;
            }
            std::cmp::Ordering::Greater => {
                only_right.insert(right[right_index].1.clone());
                right_index += 1;
            }
            std::cmp::Ordering::Equal => {
                left_index += 1;
                right_index += 1;
            }
        }
    }
    only_left.extend(left[left_index..].iter().map(|(_, id)| id.clone()));
    only_right.extend(right[right_index..].iter().map(|(_, id)| id.clone()));

    Some(DigestDiff {
        category,
        only_left: only_left.into_iter().collect(),
        only_right: only_right.into_iter().collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;

    fn digest(symbols: &str, edges: &str, fts: &str, vectors: &str, files: &str) -> DatabaseDigest {
        DatabaseDigest {
            symbols: symbols.to_string(),
            edges: edges.to_string(),
            fts: fts.to_string(),
            vector_ids: vectors.to_string(),
            files: files.to_string(),
            combined: format!("{symbols}{edges}{fts}{vectors}{files}"),
        }
    }

    #[test]
    fn categories_are_addressable_by_name() {
        let d = digest("s", "e", "f", "v", "fi");
        for name in DatabaseDigest::CATEGORIES {
            assert!(d.category(name).is_some(), "{name} must be addressable");
        }
        assert_eq!(d.category("symbols"), Some("s"));
        assert_eq!(d.category("generation"), None);
        assert_eq!(d.category(""), None);
    }

    #[test]
    fn equal_digests_have_no_differing_categories() {
        let d = digest("s", "e", "f", "v", "fi");
        assert!(d.differing_categories(&d).is_empty());
    }

    #[test]
    fn differing_categories_are_reported_in_fixed_order() {
        let left = digest("s", "e", "f", "v", "fi");
        let right = digest("s", "E", "f", "V", "fi");
        assert_eq!(
            left.differing_categories(&right),
            vec!["edges", "vector_ids"]
        );
        // The relation is symmetric.
        assert_eq!(
            right.differing_categories(&left),
            vec!["edges", "vector_ids"]
        );
    }

    #[test]
    fn a_diff_with_no_disjoint_rows_is_still_a_content_divergence() {
        let same_rows = DigestDiff {
            category: "symbols",
            only_left: Vec::new(),
            only_right: Vec::new(),
        };
        assert!(same_rows.is_disjointly_empty());
    }

    #[test]
    fn diff_reports_only_changed_categories_and_sorted_row_identifiers() {
        let dir = tempfile::tempdir().unwrap();
        let left = Database::open(dir.path().join("left-diff.db")).unwrap();
        let right = Database::open(dir.path().join("right-diff.db")).unwrap();
        insert_fixture(&left, false, r#"{"z":1}"#);
        insert_fixture(&right, false, r#"{"z":1}"#);

        let left_conn = left.connect().unwrap();
        left_conn
            .execute("DELETE FROM symbol WHERE id = 's:a'", [])
            .unwrap();
        left_conn
            .execute("DELETE FROM symbol_fts WHERE id = 's:a'", [])
            .unwrap();
        left_conn
            .execute("DELETE FROM file WHERE path = 'a.rs'", [])
            .unwrap();

        let right_conn = right.connect().unwrap();
        right_conn
            .execute("DELETE FROM symbol WHERE id = 's:b'", [])
            .unwrap();
        right_conn
            .execute("DELETE FROM symbol_fts WHERE id = 's:b'", [])
            .unwrap();
        right_conn
            .execute("DELETE FROM file WHERE path = 'b.rs'", [])
            .unwrap();

        let differences = diff(&left, &right).unwrap();
        assert_eq!(
            differences,
            vec![
                DigestDiff {
                    category: "symbols",
                    only_left: vec!["s:b".to_string()],
                    only_right: vec!["s:a".to_string()],
                },
                DigestDiff {
                    category: "fts",
                    only_left: vec!["s:b".to_string()],
                    only_right: vec!["s:a".to_string()],
                },
                DigestDiff {
                    category: "vector_ids",
                    only_left: vec!["s:b".to_string()],
                    only_right: vec!["s:a".to_string()],
                },
                DigestDiff {
                    category: "files",
                    only_left: vec!["b.rs".to_string()],
                    only_right: vec!["a.rs".to_string()],
                },
            ]
        );

        left.close_thread_connection();
        right.close_thread_connection();
    }

    #[test]
    fn diff_reports_mutated_row_identifiers_on_both_sides() {
        let dir = tempfile::tempdir().unwrap();
        let left = Database::open(dir.path().join("left-content-diff.db")).unwrap();
        let right = Database::open(dir.path().join("right-content-diff.db")).unwrap();
        insert_fixture(&left, false, r#"{"z":1}"#);
        insert_fixture(&right, false, r#"{"z":1}"#);

        right
            .connect()
            .unwrap()
            .execute(
                "UPDATE symbol SET body_excerpt = 'changed' WHERE id = 's:b'",
                [],
            )
            .unwrap();
        assert_eq!(
            diff(&left, &right).unwrap(),
            vec![DigestDiff {
                category: "symbols",
                only_left: vec!["s:b".to_string()],
                only_right: vec!["s:b".to_string()],
            }]
        );

        left.close_thread_connection();
        right.close_thread_connection();
    }

    #[test]
    fn diff_uses_compound_edge_identifiers_and_ignores_normalized_meta_order() {
        let dir = tempfile::tempdir().unwrap();
        let left = Database::open(dir.path().join("left-edge-diff.db")).unwrap();
        let right = Database::open(dir.path().join("right-edge-diff.db")).unwrap();
        insert_fixture(&left, false, r#"{"a":1,"b":2}"#);
        insert_fixture(&right, false, r#"{"b":2,"a":1}"#);

        assert!(diff(&left, &right).unwrap().is_empty());
        right
            .connect()
            .unwrap()
            .execute(
                "UPDATE edge SET confidence = 0.5 \
                 WHERE src_id = 's:a' AND dst_id = 's:b' AND kind = 'calls'",
                [],
            )
            .unwrap();
        let edge_id = r#"["s:a","s:b","calls"]"#.to_string();
        assert_eq!(
            diff(&left, &right).unwrap(),
            vec![DigestDiff {
                category: "edges",
                only_left: vec![edge_id.clone()],
                only_right: vec![edge_id],
            }]
        );

        left.close_thread_connection();
        right.close_thread_connection();
    }

    #[test]
    fn database_digest_is_order_independent_and_normalizes_edge_meta() {
        let dir = tempfile::tempdir().unwrap();
        let left = Database::open(dir.path().join("left.db")).unwrap();
        let right = Database::open(dir.path().join("right.db")).unwrap();
        insert_fixture(&left, false, r#"{"z":1,"nested":{"b":2,"a":1}}"#);
        insert_fixture(&right, true, r#"{"nested":{"a":1,"b":2},"z":1}"#);

        let left_digest = database_digest(&left).unwrap();
        let right_digest = database_digest(&right).unwrap();
        assert_eq!(left_digest, right_digest);
        assert!(diff(&left, &right).unwrap().is_empty());
        for category in DatabaseDigest::CATEGORIES {
            let value = left_digest.category(category).unwrap();
            assert_eq!(value.len(), 64);
            assert!(value.bytes().all(|byte| byte.is_ascii_hexdigit()));
        }
        assert_eq!(left_digest.combined.len(), 64);

        left.close_thread_connection();
        right.close_thread_connection();
    }

    #[test]
    fn digest_tracks_included_content_but_excludes_timestamps_generations_and_vectors() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(dir.path().join("content.db")).unwrap();
        insert_fixture(&db, false, "{}");
        let baseline = database_digest(&db).unwrap();

        let conn = db.connect().unwrap();
        conn.execute("UPDATE symbol SET updated_at = 999 WHERE id = 's:a'", [])
            .unwrap();
        conn.execute("UPDATE file SET parsed_at = 999 WHERE path = 'a.rs'", [])
            .unwrap();
        conn.execute(
            "UPDATE generation SET source_generation = 9, semantic_generation = 8, updated_at = 999",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO embedding_queue(symbol_id, file_path, enqueued_at) VALUES('queued', 'a.rs', 999)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO vector_space(id, fingerprint, embedding_dim, model_id, established_at) \
             VALUES(1, 'fp', 1, 'model', 999)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO publish_batch(source_generation, committed_at, file_count, reason) \
             VALUES(9, 999, 1, 'incremental')",
            [],
        )
        .unwrap();
        assert_eq!(database_digest(&db).unwrap(), baseline);

        conn.execute(
            "UPDATE symbol_vec SET embedding = ?1 WHERE symbol_id = 's:a'",
            params![vec![9_u8, 8, 7]],
        )
        .unwrap();
        assert_eq!(database_digest(&db).unwrap(), baseline);

        conn.execute(
            "UPDATE file SET parse_status = 'failed' WHERE path = 'a.rs'",
            [],
        )
        .unwrap();
        let changed = database_digest(&db).unwrap();
        assert_ne!(changed.files, baseline.files);
        assert_ne!(changed.combined, baseline.combined);
        assert_eq!(changed.symbols, baseline.symbols);
        assert_eq!(changed.vector_ids, baseline.vector_ids);

        db.close_thread_connection();
    }

    #[test]
    fn vector_presence_and_full_fts_content_are_included() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(dir.path().join("surface.db")).unwrap();
        insert_fixture(&db, false, "null");
        let baseline = database_digest(&db).unwrap();
        let conn = db.connect().unwrap();

        conn.execute("DELETE FROM symbol_vec WHERE symbol_id = 's:a'", [])
            .unwrap();
        let without_vector = database_digest(&db).unwrap();
        assert_ne!(without_vector.vector_ids, baseline.vector_ids);
        assert_eq!(without_vector.symbols, baseline.symbols);

        conn.execute("DELETE FROM symbol_fts WHERE id = 's:a'", [])
            .unwrap();
        conn.execute(
            "INSERT INTO symbol_fts(id, name, qualified_name, signature, docstring, body_excerpt) \
             VALUES('s:a', 'a', 'a', 'fn a()', '', 'different')",
            [],
        )
        .unwrap();
        let changed_fts = database_digest(&db).unwrap();
        assert_ne!(changed_fts.fts, without_vector.fts);
        assert_eq!(changed_fts.symbols, without_vector.symbols);

        db.close_thread_connection();
    }

    fn insert_fixture(db: &Database, reverse: bool, edge_meta: &str) {
        let conn = db.connect().unwrap();
        let ids = if reverse {
            ["s:b", "s:a"]
        } else {
            ["s:a", "s:b"]
        };
        for (offset, id) in ids.into_iter().enumerate() {
            let name = id.strip_prefix("s:").unwrap();
            let content_ordinal = i64::from(name == "b");
            conn.execute(
                "INSERT INTO symbol( \
                    id, kind, name, qualified_name, language, module, file_path, \
                    line_start, line_end, signature, docstring, content_hash, body_excerpt, \
                    semantic_summary, risk_score, ambiguous, untrusted_flags, updated_at \
                 ) VALUES(?1, 'function', ?2, ?2, 'rust', 'm', ?3, 1, 2, ?4, ?5, ?6, ?7, \
                          ?8, 0.25, 0, ?9, ?10)",
                params![
                    id,
                    name,
                    format!("{name}.rs"),
                    Some(format!("fn {name}()")),
                    Option::<String>::None,
                    format!("hash-{name}"),
                    format!("body-{name}"),
                    Option::<String>::None,
                    r#"["generated","external"]"#,
                    100 + offset as i64,
                ],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO symbol_fts(id, name, qualified_name, signature, docstring, body_excerpt) \
                 VALUES(?1, ?2, ?2, ?3, '', ?4)",
                params![id, name, format!("fn {name}()"), format!("body-{name}")],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO file(path, language, size_bytes, content_hash, parsed_at, parse_status) \
                 VALUES(?1, 'rust', ?2, ?3, ?4, 'success')",
                params![
                    format!("{name}.rs"),
                    10 + content_ordinal,
                    format!("file-hash-{name}"),
                    200 + offset as i64,
                ],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO symbol_vec(symbol_id, embedding) VALUES(?1, ?2)",
                params![id, vec![offset as u8, 1, 2, 3]],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO edge(src_id, dst_id, kind, confidence, meta) \
             VALUES('s:a', 's:b', 'calls', 0.75, ?1)",
            [edge_meta],
        )
        .unwrap();
    }
}
