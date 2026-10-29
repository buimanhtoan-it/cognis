//! Property 36: schema migration 002 preserves every pre-feature data category.

use std::fmt::Write as _;
use std::path::Path;

use cognis_store::{Database, DatabaseDigest, LATEST_SCHEMA_VERSION};
use proptest::prelude::*;
use rusqlite::types::Value;
use rusqlite::{params, Connection};
use sha2::{Digest, Sha256};

const MIGRATION_001: &str = include_str!("../migrations/001_initial.sql");
const SYMBOL_KINDS: &[&str] = &[
    "function",
    "class",
    "method",
    "interface",
    "route",
    "module",
    "var",
    "const",
];
const EDGE_KINDS: &[&str] = &[
    "calls",
    "imports",
    "inherits",
    "implements",
    "reads",
    "writes",
    "routes_to",
    "tests",
];
const NAMES: &[&str] = &[
    "alpha",
    "beta value",
    "quote_name",
    "delta-ops",
    "CamelCase",
    "under_score",
];
const LANGUAGES: &[&str] = &["python", "typescript", "rust", "go"];

#[derive(Debug, Clone)]
struct LegacyDbSpec {
    seed: u64,
    symbols: Vec<(u8, u8, bool)>,
    vector_dim: usize,
    edge_candidates: Vec<(u8, u8, u8, u8, u8)>,
    files: Vec<(u8, u16, u8)>,
}

fn arb_legacy_db() -> impl Strategy<Value = LegacyDbSpec> {
    (1usize..7).prop_flat_map(|symbol_count| {
        (
            any::<u64>(),
            prop::collection::vec((0u8..8, 0u8..6, any::<bool>()), symbol_count),
            1usize..9,
            prop::collection::vec((any::<u8>(), any::<u8>(), 0u8..8, 0u8..=100, 0u8..3), 1..12),
            prop::collection::vec((0u8..4, any::<u16>(), 0u8..3), 1..6),
        )
            .prop_map(
                |(seed, symbols, vector_dim, edge_candidates, files)| LegacyDbSpec {
                    seed,
                    symbols,
                    vector_dim,
                    edge_candidates,
                    files,
                },
            )
    })
}
fn write_pre_feature_db(path: &Path, spec: &LegacyDbSpec) -> rusqlite::Result<()> {
    let conn = Connection::open(path)?;
    conn.execute_batch(MIGRATION_001)?;
    conn.execute_batch("PRAGMA foreign_keys = ON; BEGIN IMMEDIATE;")?;

    let result = (|| {
        for (index, (kind_pick, name_pick, nullable)) in spec.symbols.iter().copied().enumerate() {
            let name = NAMES[usize::from(name_pick) % NAMES.len()];
            let language = LANGUAGES[(index + spec.seed as usize) % LANGUAGES.len()];
            let file_path = format!("src/generated_{}_{}.txt", spec.seed % 17, index % 4);
            let qualified_name = format!("module_{}.{}", index % 3, name);
            let id = symbol_id(spec, index);
            let optional = (!nullable).then(|| format!("text {index} | seed {}", spec.seed));
            let flags = (index % 3 == 0).then_some("[\"generated\",\"legacy\"]");

            conn.execute(
                "INSERT INTO symbol (id, kind, name, qualified_name, language, module, file_path, \
                 line_start, line_end, signature, docstring, content_hash, body_excerpt, \
                 semantic_summary, risk_score, ambiguous, untrusted_flags, updated_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
                params![
                    id,
                    SYMBOL_KINDS[usize::from(kind_pick) % SYMBOL_KINDS.len()],
                    name,
                    qualified_name,
                    language,
                    format!("module_{}", index % 3),
                    file_path,
                    index as i64 + 1,
                    index as i64 + 2 + i64::from(kind_pick % 4),
                    optional.as_ref().map(|s| format!("sig::{s}")),
                    optional.as_ref().map(|s| format!("doc::{s}")),
                    format!("{:016x}{index:04x}", spec.seed),
                    optional.as_ref().map(|s| format!("body::{s}")),
                    optional.as_ref().map(|s| format!("summary::{s}")),
                    f64::from(kind_pick % 11) / 10.0,
                    i64::from(index % 2 == 0),
                    flags,
                    1_700_000_000_i64 + index as i64,
                ],
            )?;
            conn.execute(
                "INSERT INTO symbol_fts (id, name, qualified_name, signature, docstring, body_excerpt) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    symbol_id(spec, index),
                    name,
                    format!("module_{}.{}", index % 3, name),
                    optional.as_ref().map(|s| format!("sig::{s}")).unwrap_or_default(),
                    optional.as_ref().map(|s| format!("doc::{s}")).unwrap_or_default(),
                    optional.as_ref().map(|s| format!("body::{s}")).unwrap_or_default(),
                ],
            )?;

            // Every generated database contains vector data; later symbols vary
            // presence so the vector-id set is non-trivial as the case grows.
            if index == 0 || (spec.seed.rotate_left(index as u32) & 1) == 1 {
                let vector = (0..spec.vector_dim)
                    .map(|dim| {
                        ((spec.seed.wrapping_add((index * 31 + dim) as u64) % 2001) as f32 - 1000.0)
                            / 1000.0
                    })
                    .collect::<Vec<_>>();
                conn.execute(
                    "INSERT INTO symbol_vec(symbol_id, embedding) VALUES (?1, ?2)",
                    params![symbol_id(spec, index), floats_to_le_bytes(&vector)],
                )?;
            }
        }

        if !spec.symbols.is_empty() {
            for (src, dst, kind, confidence, meta) in &spec.edge_candidates {
                let src = usize::from(*src) % spec.symbols.len();
                let dst = usize::from(*dst) % spec.symbols.len();
                conn.execute(
                    "INSERT OR IGNORE INTO edge(src_id, dst_id, kind, confidence, meta) \
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        symbol_id(spec, src),
                        symbol_id(spec, dst),
                        EDGE_KINDS[usize::from(*kind) % EDGE_KINDS.len()],
                        f64::from(*confidence) / 100.0,
                        match meta % 3 {
                            0 => None,
                            1 => Some("{}"),
                            _ => Some("{\"weight\":2,\"dst_missing\":false}"),
                        },
                    ],
                )?;
            }
        }

        for (index, (language, size, status)) in spec.files.iter().copied().enumerate() {
            conn.execute(
                "INSERT INTO file(path, language, size_bytes, content_hash, parsed_at, parse_status) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    format!("cache/generated_{}_{}.dat", spec.seed % 13, index),
                    LANGUAGES[usize::from(language) % LANGUAGES.len()],
                    i64::from(size),
                    format!("file-{:016x}-{index}", spec.seed),
                    1_700_100_000_i64 + index as i64,
                    ["ok", "partial", "failed"][usize::from(status) % 3],
                ],
            )?;
        }

        conn.execute(
            "INSERT OR REPLACE INTO meta(key, value) VALUES('schema_version', '1')",
            [],
        )?;
        Ok(())
    })();

    if result.is_ok() {
        conn.execute_batch("COMMIT")?;
    } else {
        let _ = conn.execute_batch("ROLLBACK");
    }
    result
}

fn symbol_id(spec: &LegacyDbSpec, index: usize) -> String {
    format!("legacy:{:016x}:{index}", spec.seed)
}

fn floats_to_le_bytes(values: &[f32]) -> Vec<u8> {
    values
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect()
}
#[derive(Debug, Clone, PartialEq, Eq)]
struct LegacySnapshot {
    symbols: Vec<String>,
    edges: Vec<String>,
    fts: Vec<String>,
    vectors: Vec<String>,
    vector_ids: Vec<String>,
    files: Vec<String>,
}

impl LegacySnapshot {
    fn read(conn: &Connection) -> rusqlite::Result<Self> {
        let symbols = query_rows(
            conn,
            "SELECT id, kind, name, qualified_name, language, module, file_path, line_start, \
             line_end, signature, docstring, content_hash, body_excerpt, semantic_summary, \
             risk_score, ambiguous, untrusted_flags FROM symbol",
            17,
        )?;
        let edges = query_rows(
            conn,
            "SELECT src_id, dst_id, kind, confidence, meta FROM edge",
            5,
        )?;
        let fts = query_rows(
            conn,
            "SELECT id, name, qualified_name, signature, docstring, body_excerpt FROM symbol_fts",
            6,
        )?;
        let vectors = query_rows(conn, "SELECT symbol_id, hex(embedding) FROM symbol_vec", 2)?;
        let vector_ids = query_rows(conn, "SELECT symbol_id FROM symbol_vec", 1)?;
        let files = query_rows(
            conn,
            "SELECT path, language, size_bytes, content_hash, parse_status FROM file",
            5,
        )?;
        Ok(Self {
            symbols,
            edges,
            fts,
            vectors,
            vector_ids,
            files,
        })
    }

    fn digest(&self) -> DatabaseDigest {
        let symbols = hash_rows(&self.symbols);
        let edges = hash_rows(&self.edges);
        let fts = hash_rows(&self.fts);
        let vector_ids = hash_rows(&self.vector_ids);
        let files = hash_rows(&self.files);
        let combined = hash_rows(&[
            symbols.clone(),
            edges.clone(),
            fts.clone(),
            vector_ids.clone(),
            files.clone(),
        ]);
        DatabaseDigest {
            symbols,
            edges,
            fts,
            vector_ids,
            files,
            combined,
        }
    }
}

fn query_rows(conn: &Connection, sql: &str, columns: usize) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare(sql)?;
    let mut rows = stmt.query([])?;
    let mut result = Vec::new();
    while let Some(row) = rows.next()? {
        let mut encoded = Vec::with_capacity(columns);
        for column in 0..columns {
            encoded.push(encode_value(row.get::<_, Value>(column)?));
        }
        result.push(encoded.join("|"));
    }
    result.sort();
    Ok(result)
}

fn encode_value(value: Value) -> String {
    let payload = match value {
        Value::Null => "null".to_string(),
        Value::Integer(value) => format!("i:{value}"),
        Value::Real(value) => format!("r:{:016x}", value.to_bits()),
        Value::Text(value) => format!("t:{value}"),
        Value::Blob(value) => {
            let mut hex = String::with_capacity(value.len() * 2);
            for byte in value {
                let _ = write!(hex, "{byte:02x}");
            }
            format!("b:{hex}")
        }
    };
    format!("{}:{payload}", payload.len())
}

fn hash_rows(rows: &[String]) -> String {
    let mut hasher = Sha256::new();
    for row in rows {
        hasher.update((row.len() as u64).to_le_bytes());
        hasher.update(row.as_bytes());
    }
    let bytes = hasher.finalize();
    let mut hex = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// Feature: cognis-one-toggle-engine, Property 36: Migration preserves data.
    /// **Validates: Requirements 22.6**
    #[test]
    fn migration_002_preserves_pre_feature_data(spec in arb_legacy_db()) {
        let dir = tempfile::tempdir().expect("create isolated migration case");
        let path = dir.path().join("pre-feature.db");
        write_pre_feature_db(&path, &spec).expect("build schema-version-1 database");

        let before = {
            let conn = Connection::open(&path).expect("open pre-feature database");
            LegacySnapshot::read(&conn).expect("snapshot pre-feature categories")
        };
        let before_digest = before.digest();

        let db = Database::open(&path).expect("apply migration 002");
        prop_assert_eq!(db.schema_version().expect("read migrated schema version"), LATEST_SCHEMA_VERSION);
        let after = LegacySnapshot::read(&db.connect().expect("open migrated database"))
            .expect("snapshot migrated categories");
        let after_digest = after.digest();
        db.close_thread_connection();

        prop_assert_eq!(&after.symbols, &before.symbols, "every symbol row must survive");
        prop_assert_eq!(&after.edges, &before.edges, "every edge row must survive");
        prop_assert_eq!(&after.fts, &before.fts, "every FTS row must survive");
        prop_assert_eq!(&after.vectors, &before.vectors, "every vector payload must survive");
        prop_assert_eq!(
            after_digest.differing_categories(&before_digest),
            Vec::<&'static str>::new(),
            "all pre-feature DatabaseDigest categories must remain unchanged"
        );
        prop_assert_eq!(after_digest, before_digest);
    }
}
