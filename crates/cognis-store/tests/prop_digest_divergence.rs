//! Property test for diagnosable database-digest divergence.

use std::collections::{BTreeMap, BTreeSet};

use cognis_store::{database_digest, diff, Database, DatabaseDigest, DigestDiff};
use proptest::prelude::*;
use rusqlite::params;

#[derive(Clone, Copy, Debug)]
enum Category {
    Symbols,
    Edges,
    Fts,
    VectorIds,
    Files,
}

impl Category {
    fn name(self) -> &'static str {
        match self {
            Self::Symbols => "symbols",
            Self::Edges => "edges",
            Self::Fts => "fts",
            Self::VectorIds => "vector_ids",
            Self::Files => "files",
        }
    }
}

#[derive(Clone, Debug)]
struct DivergencePlan {
    seed: u64,
    stable_key_changes: Vec<Category>,
    side_only_rows: Vec<(Category, Side)>,
}

#[derive(Clone, Copy, Debug)]
enum Side {
    Left,
    Right,
}

fn arb_category() -> impl Strategy<Value = Category> {
    prop_oneof![
        Just(Category::Symbols),
        Just(Category::Edges),
        Just(Category::Fts),
        Just(Category::VectorIds),
        Just(Category::Files),
    ]
}

fn arb_content_category() -> impl Strategy<Value = Category> {
    prop_oneof![
        Just(Category::Symbols),
        Just(Category::Edges),
        Just(Category::Fts),
        Just(Category::Files),
    ]
}

fn arb_divergence_plan() -> impl Strategy<Value = DivergencePlan> {
    (
        any::<u64>(),
        prop::collection::vec(arb_content_category(), 1..=5),
        prop::collection::vec((arb_category(), any::<bool>()), 1..=8),
    )
        .prop_map(
            |(seed, stable_key_changes, side_only_rows)| DivergencePlan {
                seed,
                stable_key_changes,
                side_only_rows: side_only_rows
                    .into_iter()
                    .map(|(category, left)| (category, if left { Side::Left } else { Side::Right }))
                    .collect(),
            },
        )
}

fn add_expected(
    expected: &mut BTreeMap<&'static str, (BTreeSet<String>, BTreeSet<String>)>,
    category: Category,
    identifier: String,
    side: Option<Side>,
) {
    let (left, right) = expected.entry(category.name()).or_default();
    match side {
        Some(Side::Left) => {
            left.insert(identifier);
        }
        Some(Side::Right) => {
            right.insert(identifier);
        }
        None => {
            left.insert(identifier.clone());
            right.insert(identifier);
        }
    }
}

fn insert_symbol(db: &Database, id: &str, suffix: &str) {
    db.connect()
        .unwrap()
        .execute(
            "INSERT INTO symbol(\
                id, kind, name, qualified_name, language, module, file_path, line_start, line_end, \
                signature, docstring, content_hash, body_excerpt, semantic_summary, risk_score, \
                ambiguous, untrusted_flags, updated_at\
             ) VALUES(?1, 'function', ?2, ?3, 'rust', 'generated', ?4, 1, 2, ?5, NULL, ?6, ?7, \
                      NULL, 0.25, 0, NULL, 1)",
            params![
                id,
                format!("name-{suffix}"),
                format!("generated::name_{suffix}"),
                format!("src/{suffix}.rs"),
                format!("fn name_{suffix}()"),
                format!("hash-{suffix}"),
                format!("body-{suffix}"),
            ],
        )
        .unwrap();
}

fn insert_vector(db: &Database, id: &str) {
    db.connect()
        .unwrap()
        .execute(
            "INSERT INTO symbol_vec(symbol_id, embedding) VALUES(?1, ?2)",
            params![id, vec![1_u8, 2, 3, 4]],
        )
        .unwrap();
}

fn insert_one_sided_vector(
    left: &Database,
    right: &Database,
    side: Side,
    id: &str,
    suffix: &str,
) -> String {
    // Vector presence is the category under test; its backing symbol must not
    // introduce an unrelated symbol divergence.
    insert_symbol(left, id, suffix);
    insert_symbol(right, id, suffix);
    insert_vector(
        match side {
            Side::Left => left,
            Side::Right => right,
        },
        id,
    );
    id.to_string()
}

fn insert_category_row(db: &Database, category: Category, id: &str, suffix: &str) -> String {
    let conn = db.connect().unwrap();
    match category {
        Category::Symbols => {
            drop(conn);
            insert_symbol(db, id, suffix);
            id.to_string()
        }
        Category::Edges => {
            let dst = format!("{id}:dst");
            conn.execute(
                "INSERT INTO edge(src_id, dst_id, kind, confidence, meta) \
                 VALUES(?1, ?2, 'calls', 0.75, '{\"generated\":true}')",
                params![id, dst],
            )
            .unwrap();
            serde_json::to_string(&(id, dst, "calls")).unwrap()
        }
        Category::Fts => {
            conn.execute(
                "INSERT INTO symbol_fts(id, name, qualified_name, signature, docstring, body_excerpt) \
                 VALUES(?1, ?2, ?3, ?4, '', ?5)",
                params![
                    id,
                    format!("fts-{suffix}"),
                    format!("generated::fts_{suffix}"),
                    format!("fn fts_{suffix}()"),
                    format!("fts-body-{suffix}"),
                ],
            )
            .unwrap();
            id.to_string()
        }
        Category::VectorIds => unreachable!("vector rows require paired backing symbols"),
        Category::Files => {
            conn.execute(
                "INSERT INTO file(path, language, size_bytes, content_hash, parsed_at, parse_status) \
                 VALUES(?1, 'rust', ?2, ?3, 1, 'success')",
                params![id, suffix.len() as i64, format!("file-hash-{suffix}")],
            )
            .unwrap();
            id.to_string()
        }
    }
}
fn insert_stable_pair(
    left: &Database,
    right: &Database,
    category: Category,
    id: &str,
    ordinal: usize,
) -> String {
    let identifier = insert_category_row(left, category, id, &format!("left-{ordinal}"));
    match category {
        Category::Symbols => insert_symbol(right, id, &format!("right-{ordinal}")),
        Category::Edges => {
            let dst = format!("{id}:dst");
            right
                .connect()
                .unwrap()
                .execute(
                    "INSERT INTO edge(src_id, dst_id, kind, confidence, meta) \
                     VALUES(?1, ?2, 'calls', 0.25, '{\"generated\":true}')",
                    params![id, dst],
                )
                .unwrap();
        }
        Category::Fts => {
            right
                .connect()
                .unwrap()
                .execute(
                    "INSERT INTO symbol_fts(id, name, qualified_name, signature, docstring, body_excerpt) \
                     VALUES(?1, ?2, ?3, ?4, '', ?5)",
                    params![
                        id,
                        format!("fts-right-{ordinal}"),
                        format!("generated::fts_right_{ordinal}"),
                        format!("fn fts_right_{ordinal}()"),
                        format!("fts-body-right-{ordinal}"),
                    ],
                )
                .unwrap();
        }
        Category::VectorIds => unreachable!("vector presence has no stable-key content"),
        Category::Files => {
            right
                .connect()
                .unwrap()
                .execute(
                    "INSERT INTO file(path, language, size_bytes, content_hash, parsed_at, parse_status) \
                     VALUES(?1, 'rust', ?2, ?3, 1, 'failed')",
                    params![id, ordinal as i64 + 100, format!("right-hash-{ordinal}")],
                )
                .unwrap();
        }
    }
    identifier
}

fn expected_diffs(
    expected: BTreeMap<&'static str, (BTreeSet<String>, BTreeSet<String>)>,
) -> Vec<DigestDiff> {
    DatabaseDigest::CATEGORIES
        .into_iter()
        .filter_map(|category| {
            expected.get(category).map(|(left, right)| DigestDiff {
                category,
                only_left: left.iter().cloned().collect(),
                only_right: right.iter().cloned().collect(),
            })
        })
        .collect()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    // Property 9: Digest divergence is diagnosable.
    /// **Validates: Requirements 24.2**
    #[test]
    fn differing_digests_name_exact_categories_and_row_identifiers(plan in arb_divergence_plan()) {
        let dir = tempfile::tempdir().expect("create isolated digest-divergence case");
        let left = Database::open(dir.path().join("left.db")).expect("open left database");
        let right = Database::open(dir.path().join("right.db")).expect("open right database");
        let mut expected = BTreeMap::new();

        for (ordinal, category) in plan.stable_key_changes.iter().copied().enumerate() {
            if matches!(category, Category::VectorIds) {
                continue;
            }
            let id = match category {
                Category::Files => format!("stable/{:016x}/{ordinal}.rs", plan.seed),
                _ => format!("stable:{:016x}:{ordinal}", plan.seed),
            };
            let identifier = insert_stable_pair(&left, &right, category, &id, ordinal);
            add_expected(&mut expected, category, identifier, None);
        }

        for (ordinal, (category, side)) in plan.side_only_rows.iter().copied().enumerate() {
            let id = match category {
                Category::Files => format!("side/{:016x}/{ordinal}.rs", plan.seed),
                _ => format!("side:{:016x}:{ordinal}", plan.seed),
            };
            let identifier = match category {
                Category::VectorIds => insert_one_sided_vector(
                    &left,
                    &right,
                    side,
                    &id,
                    &format!("side-{ordinal}"),
                ),
                _ => {
                    let target = match side { Side::Left => &left, Side::Right => &right };
                    insert_category_row(target, category, &id, &format!("side-{ordinal}"))
                }
            };
            add_expected(&mut expected, category, identifier, Some(side));
        }

        let left_digest = database_digest(&left).expect("digest left database");
        let right_digest = database_digest(&right).expect("digest right database");
        prop_assert_ne!(&left_digest, &right_digest, "the generated pair must diverge");
        let expected = expected_diffs(expected);
        prop_assert_eq!(left_digest.differing_categories(&right_digest), expected.iter().map(|item| item.category).collect::<Vec<_>>());
        prop_assert_eq!(diff(&left, &right).expect("diagnose divergence"), expected);

        left.close_thread_connection();
        right.close_thread_connection();
    }
}
