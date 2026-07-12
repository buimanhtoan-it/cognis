//! Property-based test for YAML/TOML leaf-key mapping (Task 3.2).
//!
//! Feature: non-code-artifact-coverage, Property 7: YAML/TOML leaf-key mapping
//!
//! Validates: Requirements 2.1, 2.2, 2.3
//!
//! ## The property
//!
//! *For any* YAML or TOML document, the extractor emits exactly one
//! `SymbolKind::Var` per leaf key (each sequence element counting as its own
//! leaf), and each symbol's searchable text contains the fully-qualified
//! leaf-key path and its scalar value truncated to at most 4096 characters.
//!
//! ## How it is driven
//!
//! Rather than round-tripping arbitrary text (whose leaf set would be
//! unknowable), the generator produces a **known structured model** and renders
//! *both* the document text *and* the exact expected leaf set from that same
//! model. The document is then fed through the genuine public extractor
//! (`extract_artifact`) and the emitted symbols are checked against the model's
//! computed leaves for an exact one-`Var`-per-leaf bijection, with every
//! symbol's searchable text asserted to contain its dotted path and its
//! (truncated) scalar value.
//!
//! The generator is deliberately constrained to the mainstream, well-supported
//! surface the extractor documents — block-style YAML (nested mappings and
//! sequences of scalars) and standard TOML (root keys, `[section]` tables, and
//! arrays of scalars). Every generated key is globally unique, so leaf paths
//! never collide and the expected set is an exact oracle. A rare oversized
//! scalar value exercises the 4096-character truncation branch.

use std::collections::BTreeMap;

use cognis_core::SymbolKind;
use cognis_indexer::parser::artifact::extract_artifact;
use cognis_indexer::ArtifactKind;
use proptest::prelude::*;

/// Must match `yaml::SCALAR_LIMIT` — the max chars of the scalar component of a
/// leaf's searchable text (Req 2.2).
const SCALAR_LIMIT: usize = 4096;

// ===========================================================================
// Structured model
// ===========================================================================

/// A YAML node whose keys are assigned during rendering (so global uniqueness is
/// guaranteed, making leaf paths a collision-free oracle).
#[derive(Debug, Clone)]
enum YamlShape {
    /// A non-empty inline scalar leaf.
    Scalar(String),
    /// A null / empty leaf (`key:`), searchable text is the path alone (Req 2.4).
    Null,
    /// A block sequence of scalar elements; each element is its own leaf (Req 2.3).
    Seq(Vec<String>),
    /// A nested (non-empty) mapping.
    Map(Vec<YamlShape>),
}

/// A single TOML value.
#[derive(Debug, Clone)]
enum TomlVal {
    Scalar(String),
    Empty,
    /// A non-empty array of scalars; each element is its own leaf (Req 2.3).
    Array(Vec<String>),
}

/// A TOML document model: root-level entries (rendered before any header) plus a
/// list of `[section]` tables, each with its own entries.
#[derive(Debug, Clone)]
struct TomlModel {
    root: Vec<TomlVal>,
    sections: Vec<Vec<TomlVal>>,
}

/// Either kind of generated document.
#[derive(Debug, Clone)]
enum Doc {
    Yaml(Vec<YamlShape>),
    Toml(TomlModel),
}

// ===========================================================================
// Generators
// ===========================================================================

/// A scalar value: mostly a short letter-led alphanumeric token, rarely a huge
/// value to exercise the 4096-char truncation. Never a YAML null token.
fn scalar_value() -> impl Strategy<Value = String> {
    prop_oneof![
        24 => "[a-z][a-z0-9]{0,7}".prop_map(|s| if s == "null" { "nullx".to_string() } else { s }),
        1 => Just("a".repeat(SCALAR_LIMIT + 904)), // > 4096 → truncation branch
    ]
}

fn yaml_shape() -> impl Strategy<Value = YamlShape> {
    let leaf = prop_oneof![
        3 => scalar_value().prop_map(YamlShape::Scalar),
        1 => Just(YamlShape::Null),
        2 => prop::collection::vec(scalar_value(), 1..4).prop_map(YamlShape::Seq),
    ];
    // Nested mappings up to a small depth; each branch is a non-empty child list.
    leaf.prop_recursive(3, 24, 4, |inner| {
        prop::collection::vec(inner, 1..4).prop_map(YamlShape::Map)
    })
}

fn toml_val() -> impl Strategy<Value = TomlVal> {
    prop_oneof![
        4 => scalar_value().prop_map(TomlVal::Scalar),
        1 => Just(TomlVal::Empty),
        2 => prop::collection::vec("[a-z][a-z0-9]{0,7}", 1..4).prop_map(TomlVal::Array),
    ]
}

fn toml_model() -> impl Strategy<Value = TomlModel> {
    (
        prop::collection::vec(toml_val(), 1..4),
        prop::collection::vec(prop::collection::vec(toml_val(), 1..4), 0..3),
    )
        .prop_map(|(root, sections)| TomlModel { root, sections })
}

fn doc_strategy() -> impl Strategy<Value = Doc> {
    prop_oneof![
        prop::collection::vec(yaml_shape(), 1..4).prop_map(Doc::Yaml),
        toml_model().prop_map(Doc::Toml),
    ]
}

// ===========================================================================
// Rendering: model -> (kind, source text, expected leaves)
// ===========================================================================

/// A monotonically increasing name allocator, so every key/section is globally
/// unique and every leaf path is therefore unique.
struct Counter(u32);
impl Counter {
    fn key(&mut self) -> String {
        let n = self.0;
        self.0 += 1;
        format!("k{n}")
    }
}

fn join(prefix: &str, key: &str) -> String {
    if prefix.is_empty() {
        key.to_string()
    } else {
        format!("{prefix}.{key}")
    }
}

/// Render a YAML mapping of `shapes` at `indent`, accumulating text lines and the
/// expected `(path, scalar)` leaves.
fn render_yaml_map(
    shapes: &[YamlShape],
    indent: usize,
    prefix: &str,
    ctr: &mut Counter,
    lines: &mut Vec<String>,
    leaves: &mut Vec<(String, String)>,
) {
    let pad = " ".repeat(indent);
    let pad2 = " ".repeat(indent + 2);
    for shape in shapes {
        let key = ctr.key();
        let path = join(prefix, &key);
        match shape {
            YamlShape::Scalar(v) => {
                lines.push(format!("{pad}{key}: {v}"));
                leaves.push((path, v.clone()));
            }
            YamlShape::Null => {
                lines.push(format!("{pad}{key}:"));
                leaves.push((path, String::new()));
            }
            YamlShape::Seq(elems) => {
                lines.push(format!("{pad}{key}:"));
                for (i, e) in elems.iter().enumerate() {
                    lines.push(format!("{pad2}- {e}"));
                    leaves.push((format!("{path}[{i}]"), e.clone()));
                }
            }
            YamlShape::Map(children) => {
                lines.push(format!("{pad}{key}:"));
                render_yaml_map(children, indent + 2, &path, ctr, lines, leaves);
            }
        }
    }
}

/// Render a TOML value as its right-hand-side text and push its leaf/leaves.
fn render_toml_val(
    val: &TomlVal,
    path: &str,
    lines: &mut Vec<String>,
    leaves: &mut Vec<(String, String)>,
    key: &str,
) {
    match val {
        TomlVal::Scalar(v) => {
            lines.push(format!("{key} = \"{v}\""));
            leaves.push((path.to_string(), v.clone()));
        }
        TomlVal::Empty => {
            lines.push(format!("{key} = \"\""));
            leaves.push((path.to_string(), String::new()));
        }
        TomlVal::Array(elems) => {
            let rendered: Vec<String> = elems.iter().map(|e| format!("\"{e}\"")).collect();
            lines.push(format!("{key} = [{}]", rendered.join(", ")));
            for (i, e) in elems.iter().enumerate() {
                leaves.push((format!("{path}[{i}]"), e.clone()));
            }
        }
    }
}

/// Turn a document model into `(kind, source, expected_leaves)`.
fn render(doc: &Doc) -> (ArtifactKind, String, Vec<(String, String)>) {
    let mut lines = Vec::new();
    let mut leaves = Vec::new();
    let mut ctr = Counter(0);
    let kind = match doc {
        Doc::Yaml(children) => {
            render_yaml_map(children, 0, "", &mut ctr, &mut lines, &mut leaves);
            ArtifactKind::Yaml
        }
        Doc::Toml(model) => {
            // Root entries first (table prefix is empty → path == key).
            for val in &model.root {
                let key = ctr.key();
                render_toml_val(val, &key, &mut lines, &mut leaves, &key);
            }
            // Then each `[section]` table.
            for entries in &model.sections {
                let section = ctr.key();
                lines.push(format!("[{section}]"));
                for val in entries {
                    let key = ctr.key();
                    let path = join(&section, &key);
                    render_toml_val(val, &path, &mut lines, &mut leaves, &key);
                }
            }
            ArtifactKind::Toml
        }
    };
    let mut source = lines.join("\n");
    source.push('\n');
    (kind, source, leaves)
}

proptest! {
    // Minimum 100 iterations, one test for Property 7.
    #![proptest_config(ProptestConfig::with_cases(200))]

    // Feature: non-code-artifact-coverage, Property 7: YAML/TOML leaf-key mapping
    #[test]
    fn yaml_toml_leaf_key_mapping(doc in doc_strategy()) {
        let (kind, source, expected) = render(&doc);

        // Sanity: the model itself must yield a collision-free, non-empty leaf
        // set (globally unique keys guarantee unique paths) so the oracle is exact.
        let mut expected_map: BTreeMap<String, String> = BTreeMap::new();
        for (path, scalar) in &expected {
            prop_assert!(
                expected_map.insert(path.clone(), scalar.clone()).is_none(),
                "generator produced a duplicate leaf path {path} — oracle would be ambiguous"
            );
        }
        prop_assert!(!expected_map.is_empty(), "model must produce at least one leaf");

        let out = extract_artifact(kind, "config/app.cfg", &source);

        // Structured extraction (not the whole-file textual fallback), since the
        // model always yields 1..=MAX_LEAVES leaves.
        prop_assert!(
            !out.fell_back,
            "expected structured extraction, got fallback for source:\n{source}"
        );

        // --- Exactly one `Var` per leaf (Req 2.1, 2.3). ---
        prop_assert_eq!(
            out.symbols.len(),
            expected_map.len(),
            "symbol count must equal leaf count.\nsource:\n{}\nsymbols: {:?}",
            source,
            out.symbols.iter().map(|s| s.name.clone()).collect::<Vec<_>>()
        );

        let mut seen: BTreeMap<String, String> = BTreeMap::new();
        for sym in &out.symbols {
            // Every emitted artifact symbol is a `Var` and is structurally valid.
            prop_assert_eq!(
                sym.kind,
                SymbolKind::Var,
                "every emitted YAML/TOML symbol must be a Var, got {:?} for {}",
                sym.kind,
                sym.name
            );
            sym.validate().map_err(|e| {
                TestCaseError::fail(format!("emitted symbol failed validate(): {e}"))
            })?;

            let body = sym.body_excerpt.clone().unwrap_or_default();

            // The searchable text must contain the fully-qualified leaf path (Req 2.2).
            prop_assert!(
                body.contains(&sym.name),
                "searchable text {body:?} must contain the leaf path {:?}",
                sym.name
            );

            prop_assert!(
                seen.insert(sym.name.clone(), body).is_none(),
                "extractor emitted a duplicate leaf path {}",
                sym.name
            );
        }

        // --- The emitted path set is exactly the model's leaf set. ---
        let emitted_paths: Vec<&String> = seen.keys().collect();
        let expected_paths: Vec<&String> = expected_map.keys().collect();
        prop_assert_eq!(
            emitted_paths,
            expected_paths,
            "emitted leaf paths must equal the model's leaf paths"
        );

        // --- Each symbol's text contains its scalar value truncated to <= 4096
        // characters (Req 2.2). ---
        for (path, scalar) in &expected_map {
            let body = seen.get(path).expect("path present by set equality");
            let truncated: String = scalar.chars().take(SCALAR_LIMIT).collect();
            prop_assert!(
                body.contains(&truncated),
                "searchable text for {path} must contain the (truncated) scalar.\n\
                 body(len={}): {:?}\ntruncated scalar(len={}): {:?}",
                body.chars().count(),
                body.chars().take(64).collect::<String>(),
                truncated.chars().count(),
                truncated.chars().take(64).collect::<String>()
            );
            // The stored scalar component never exceeds the 4096-char cap.
            prop_assert!(
                truncated.chars().count() <= SCALAR_LIMIT,
                "truncated scalar exceeds the {SCALAR_LIMIT}-char cap"
            );
        }
    }
}
