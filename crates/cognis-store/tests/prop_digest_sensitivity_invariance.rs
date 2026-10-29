use cognis_store::{database_digest, Database, DatabaseDigest};
use cognis_testgen::{arb_source_tree, SourceTree};
use proptest::prelude::*;
use rusqlite::{params, Connection};

#[derive(Debug)]
struct RowSpec {
    id: String,
    name: String,
    qualified_name: String,
    language: &'static str,
    file_path: String,
    content_hash: String,
    body_excerpt: String,
    size_bytes: i64,
}

#[derive(Debug)]
struct Fixture {
    rows: Vec<RowSpec>,
}

impl Fixture {
    fn target(&self) -> &RowSpec {
        self.rows.first().expect("arb_source_tree is non-empty")
    }
}

struct TestDatabase(Database);

impl TestDatabase {
    fn open(path: impl AsRef<std::path::Path>) -> Self {
        Self(Database::open(path).expect("open property database"))
    }

    fn get(&self) -> &Database {
        &self.0
    }
}

impl Drop for TestDatabase {
    fn drop(&mut self) {
        self.0.close_thread_connection();
    }
}

fn fixture_from_tree(tree: &SourceTree) -> Fixture {
    let rows = tree
        .files
        .iter()
        .enumerate()
        .map(|(index, file)| {
            let name = file
                .declared_names()
                .first()
                .copied()
                .unwrap_or("file_surface")
                .to_string();
            RowSpec {
                id: format!("generated:{index}:{}", file.content_hash()),
                qualified_name: format!("generated_{index}.{name}"),
                name,
                language: file.language,
                file_path: file.rel_path.clone(),
                content_hash: file.content_hash(),
                body_excerpt: String::from_utf8_lossy(&file.bytes()).into_owned(),
                size_bytes: file.bytes().len() as i64,
            }
        })
        .collect();
    Fixture { rows }
}

fn populate(db: &Database, fixture: &Fixture, reverse: bool) {
    let conn = db.connect().expect("connect property database");
    let indices: Vec<usize> = if reverse {
        (0..fixture.rows.len()).rev().collect()
    } else {
        (0..fixture.rows.len()).collect()
    };

    for index in &indices {
        let row = &fixture.rows[*index];
        conn.execute(
            "INSERT INTO symbol(\
                id, kind, name, qualified_name, language, module, file_path, \
                line_start, line_end, signature, docstring, content_hash, body_excerpt, \
                semantic_summary, risk_score, ambiguous, untrusted_flags, updated_at\
             ) VALUES(?1, 'function', ?2, ?3, ?4, 'generated', ?5, 1, 1, ?6, ?7, ?8, ?9, \
                      ?10, 0.25, 0, ?11, ?12)",
            params![
                row.id,
                row.name,
                row.qualified_name,
                row.language,
                row.file_path,
                Some(format!("fn {}()", row.name)),
                Option::<String>::None,
                row.content_hash,
                row.body_excerpt,
                Some(format!("summary:{}", row.name)),
                r#"["generated","source-tree"]"#,
                1_700_000_000_i64 + *index as i64,
            ],
        )
        .expect("insert symbol row");
        conn.execute(
            "INSERT INTO symbol_fts(id, name, qualified_name, signature, docstring, body_excerpt) \
             VALUES(?1, ?2, ?3, ?4, '', ?5)",
            params![
                row.id,
                row.name,
                row.qualified_name,
                format!("fn {}()", row.name),
                row.body_excerpt,
            ],
        )
        .expect("insert FTS row");
        conn.execute(
            "INSERT INTO symbol_vec(symbol_id, embedding) VALUES(?1, ?2)",
            params![row.id, vec![*index as u8, 1, 2, 3]],
        )
        .expect("insert vector row");
        conn.execute(
            "INSERT INTO file(path, language, size_bytes, content_hash, parsed_at, parse_status) \
             VALUES(?1, ?2, ?3, ?4, ?5, 'ok')",
            params![
                row.file_path,
                row.language,
                row.size_bytes,
                row.content_hash,
                1_700_100_000_i64 + *index as i64,
            ],
        )
        .expect("insert file-cache row");
    }

    if fixture.rows.len() > 1 {
        let edge_indices: Vec<usize> = if reverse {
            (0..fixture.rows.len() - 1).rev().collect()
        } else {
            (0..fixture.rows.len() - 1).collect()
        };
        for index in edge_indices {
            conn.execute(
                "INSERT INTO edge(src_id, dst_id, kind, confidence, meta) \
                 VALUES(?1, ?2, 'calls', 0.75, ?3)",
                params![
                    fixture.rows[index].id,
                    fixture.rows[index + 1].id,
                    format!(r#"{{"ordinal":{index},"nested":{{"b":2,"a":1}}}}"#),
                ],
            )
            .expect("insert edge row");
        }
    } else {
        let id = &fixture.target().id;
        conn.execute(
            "INSERT INTO edge(src_id, dst_id, kind, confidence, meta) \
             VALUES(?1, ?1, 'calls', 0.75, ?2)",
            params![id, r#"{"nested":{"b":2,"a":1}}"#],
        )
        .expect("insert self edge row");
    }
}

fn assert_only_category_changed(
    baseline: &DatabaseDigest,
    changed: &DatabaseDigest,
    category: &'static str,
) -> Result<(), TestCaseError> {
    prop_assert_ne!(baseline.category(category), changed.category(category));
    prop_assert_ne!(&baseline.combined, &changed.combined);
    prop_assert_eq!(baseline.differing_categories(changed), vec![category]);
    Ok(())
}
fn mutate_included(conn: &Connection, fixture: &Fixture, category: u8) -> &'static str {
    let target = fixture.target();
    match category {
        0 => {
            conn.execute(
                "UPDATE symbol SET content_hash = content_hash || ':mutated' WHERE id = ?1",
                [&target.id],
            )
            .expect("mutate one symbol row");
            "symbols"
        }
        1 => {
            conn.execute(
                "UPDATE edge SET confidence = 0.5 WHERE rowid = (SELECT rowid FROM edge LIMIT 1)",
                [],
            )
            .expect("mutate one edge row");
            "edges"
        }
        2 => {
            conn.execute(
                "UPDATE symbol_fts SET body_excerpt = body_excerpt || ':mutated' WHERE id = ?1",
                [&target.id],
            )
            .expect("mutate one FTS row");
            "fts"
        }
        3 => {
            conn.execute("DELETE FROM symbol_vec WHERE symbol_id = ?1", [&target.id])
                .expect("mutate vector-bearing id set");
            "vector_ids"
        }
        _ => {
            conn.execute(
                "UPDATE file SET parse_status = 'failed:mutated' WHERE path = ?1",
                [&target.file_path],
            )
            .expect("mutate one file-cache row");
            "files"
        }
    }
}

fn mutate_excluded(conn: &Connection, fixture: &Fixture) {
    let target = fixture.target();
    conn.execute(
        "UPDATE symbol SET updated_at = updated_at + 999 WHERE id = ?1",
        [&target.id],
    )
    .expect("mutate excluded symbol timestamp");
    conn.execute(
        "UPDATE file SET parsed_at = parsed_at + 999 WHERE path = ?1",
        [&target.file_path],
    )
    .expect("mutate excluded file timestamp");
    conn.execute(
        "UPDATE generation SET source_generation = 7, semantic_generation = 6, updated_at = 999 \
         WHERE id = 1",
        [],
    )
    .expect("mutate excluded generations");
    conn.execute(
        "UPDATE symbol_vec SET embedding = ?1 WHERE symbol_id = ?2",
        params![vec![9_u8, 8, 7, 6], target.id],
    )
    .expect("mutate excluded vector payload");
    conn.execute(
        "INSERT INTO embedding_queue(symbol_id, file_path, enqueued_at, last_error) \
         VALUES('excluded:queued', ?1, 999, 'excluded')",
        [&target.file_path],
    )
    .expect("insert excluded queue row");
    conn.execute(
        "INSERT INTO vector_space(id, fingerprint, embedding_dim, model_id, established_at) \
         VALUES(1, 'excluded-fingerprint', 4, 'excluded-model', 999)",
        [],
    )
    .expect("insert excluded vector-space row");
    conn.execute(
        "INSERT INTO publish_batch(source_generation, committed_at, file_count, reason) \
         VALUES(7, 999, 1, 'incremental')",
        [],
    )
    .expect("insert excluded publish row");
    conn.execute(
        "INSERT INTO publish_batch_file(source_generation, path, change) \
         VALUES(7, ?1, 'changed')",
        [&target.file_path],
    )
    .expect("insert excluded publish-file row");
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    // Property 8: Digest sensitivity and invariance.
    // **Validates: Requirements 12.2, 12.3, 12.4**
    #[test]
    fn digest_is_sensitive_only_to_included_rows_and_invariant_to_order(
        tree in arb_source_tree(),
        category in 0u8..5,
    ) {
        let fixture = fixture_from_tree(&tree);
        let dir = tempfile::tempdir().expect("create isolated digest case");
        let canonical = TestDatabase::open(dir.path().join("canonical.db"));
        let permuted = TestDatabase::open(dir.path().join("permuted.db"));
        populate(canonical.get(), &fixture, false);
        populate(permuted.get(), &fixture, true);

        let baseline = database_digest(canonical.get()).expect("digest canonical content");
        let reordered = database_digest(permuted.get()).expect("digest permuted content");
        prop_assert_eq!(&reordered, &baseline, "insertion order must not affect any digest");

        mutate_excluded(&permuted.get().connect().expect("connect permuted database"), &fixture);
        let excluded_changed =
            database_digest(permuted.get()).expect("digest after excluded mutations");
        prop_assert_eq!(excluded_changed, baseline.clone(), "excluded values changed the digest");

        let changed_category = mutate_included(
            &canonical.get().connect().expect("connect canonical database"),
            &fixture,
            category,
        );
        let included_changed =
            database_digest(canonical.get()).expect("digest after one included-row mutation");
        assert_only_category_changed(&baseline, &included_changed, changed_category)?;
    }
}
