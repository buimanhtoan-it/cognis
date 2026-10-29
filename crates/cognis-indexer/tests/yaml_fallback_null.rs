//! Unit/integration tests for the YAML/TOML extractor's fallback boundaries and
//! null-leaf handling (Task 3.4, Requirements 2.4 and 2.7).
//!
//! Feature: non-code-artifact-coverage
//!
//! ## What this pins
//!
//! - Requirement 2.7: "IF a YAML or TOML Artifact_File is malformed such that
//!   structured extraction yields no leaf keys, OR yields more than 10,000 leaf
//!   keys, THEN THE YAML_Extractor SHALL fall back to a single textual symbol
//!   spanning line 1 to the file's last line so the file remains searchable and
//!   the batch continues."
//! - Requirement 2.4: "WHEN a leaf key's value is null or empty, THE
//!   YAML_Extractor SHALL emit the symbol with searchable text containing the
//!   leaf key path and an empty scalar value."
//!
//! These drive the public artifact API
//! (`cognis_indexer::parser::artifact::extract_artifact`) with
//! `cognis_indexer::ArtifactKind::{Yaml, Toml}`. The extractor
//! (`crates/cognis-indexer/src/parser/artifact/yaml.rs`) uses `MAX_LEAVES =
//! 10_000`: 0 leaves OR > 10,000 leaves route to the shared whole-file
//! `textual_fallback`, which produces exactly one `SymbolKind::Module` symbol
//! spanning line 1..last with `fell_back == true`. A null/empty value produces a
//! leaf `Var` symbol whose `body_excerpt` is just the key path (empty scalar).

use cognis_core::SymbolKind;
use cognis_indexer::parser::artifact::extract_artifact;
use cognis_indexer::ArtifactKind;

/// Req 2.7 (0-leaf boundary): non-empty YAML content that yields no structured
/// leaf keys falls back to exactly one whole-file `Module` symbol spanning line
/// 1..last, with `fell_back == true`.
#[test]
fn yaml_zero_leaves_falls_back_to_one_whole_file_symbol() {
    // Prose with no `key: value` mapping lines and no sequence items — the
    // walker resolves zero leaves, so the file routes to the textual fallback.
    let source = "lorem ipsum dolor\nsit amet consectetur\nadipiscing elit sed\n";
    let expected_last_line = source.lines().count() as u32;
    assert!(expected_last_line >= 2, "fixture must be multi-line");

    let out = extract_artifact(ArtifactKind::Yaml, "config/prose.yaml", source);

    assert_eq!(
        out.symbols.len(),
        1,
        "0-leaf YAML must produce exactly one whole-file fallback symbol, got {:?}",
        out.symbols
    );
    assert!(
        out.fell_back,
        "0-leaf YAML must be marked as a fallback result"
    );

    let sym = &out.symbols[0];
    assert_eq!(
        sym.kind,
        SymbolKind::Module,
        "the whole-file fallback symbol must be a Module"
    );
    assert_eq!(sym.line_start, 1, "fallback spans from line 1");
    assert_eq!(
        sym.line_end, expected_last_line,
        "fallback spans to the file's last line"
    );
    sym.validate()
        .expect("fallback symbol must satisfy Symbol::validate");
}

/// Req 2.7 (>10,000-leaf boundary, YAML): a document that would produce more
/// than `MAX_LEAVES` (10,000) leaves falls back to exactly one whole-file
/// `Module` symbol instead of emitting a symbol explosion.
#[test]
fn yaml_over_max_leaves_falls_back_to_one_whole_file_symbol() {
    // 10,001 distinct top-level `k{i}: {i}` mapping entries → 10,001 leaves,
    // which is strictly greater than MAX_LEAVES (10,000), tripping the fallback.
    let mut source = String::new();
    for i in 0..=10_000 {
        source.push_str(&format!("k{i}: {i}\n"));
    }
    let expected_last_line = source.lines().count() as u32;
    assert_eq!(expected_last_line, 10_001, "fixture must have 10,001 keys");

    let out = extract_artifact(ArtifactKind::Yaml, "config/huge.yaml", &source);

    assert_eq!(
        out.symbols.len(),
        1,
        ">10,000-leaf YAML must produce exactly one whole-file fallback symbol, got {}",
        out.symbols.len()
    );
    assert!(
        out.fell_back,
        ">10,000-leaf YAML must be marked as a fallback result"
    );

    let sym = &out.symbols[0];
    assert_eq!(sym.kind, SymbolKind::Module);
    assert_eq!(sym.line_start, 1);
    assert_eq!(sym.line_end, expected_last_line);
    sym.validate()
        .expect("fallback symbol must satisfy Symbol::validate");
}

/// Req 2.7 (>10,000-leaf boundary, TOML): the same fallback discipline applies
/// to the TOML surface, proving the boundary is shared across both formats.
#[test]
fn toml_over_max_leaves_falls_back_to_one_whole_file_symbol() {
    // 10,001 distinct `k{i} = {i}` assignments → 10,001 leaves > MAX_LEAVES.
    let mut source = String::new();
    for i in 0..=10_000 {
        source.push_str(&format!("k{i} = {i}\n"));
    }
    let expected_last_line = source.lines().count() as u32;
    assert_eq!(expected_last_line, 10_001, "fixture must have 10,001 keys");

    let out = extract_artifact(ArtifactKind::Toml, "config/huge.toml", &source);

    assert_eq!(
        out.symbols.len(),
        1,
        ">10,000-leaf TOML must produce exactly one whole-file fallback symbol, got {}",
        out.symbols.len()
    );
    assert!(out.fell_back);

    let sym = &out.symbols[0];
    assert_eq!(sym.kind, SymbolKind::Module);
    assert_eq!(sym.line_start, 1);
    assert_eq!(sym.line_end, expected_last_line);
    sym.validate()
        .expect("fallback symbol must satisfy Symbol::validate");
}

/// Req 2.7 boundary sanity: a document with exactly `MAX_LEAVES` (10,000) leaves
/// is *within* bounds and must NOT fall back — it emits structured leaves. This
/// pins the boundary as "> 10,000", not ">= 10,000".
#[test]
fn yaml_exactly_max_leaves_is_structured_not_fallback() {
    let mut source = String::new();
    for i in 0..10_000 {
        source.push_str(&format!("k{i}: {i}\n"));
    }

    let out = extract_artifact(ArtifactKind::Yaml, "config/at_cap.yaml", &source);

    assert!(
        !out.fell_back,
        "exactly 10,000 leaves is within MAX_LEAVES and must stay structured"
    );
    assert_eq!(
        out.symbols.len(),
        10_000,
        "exactly 10,000 leaves must emit one Var per leaf"
    );
    assert!(
        out.symbols.iter().all(|s| s.kind == SymbolKind::Var),
        "structured leaves are Var symbols"
    );
}

/// Req 2.4 (null/empty leaf, YAML): a bare `password:` entry with no value emits
/// a `password` leaf `Var` symbol whose searchable text (`body_excerpt`) is just
/// the key path with an empty scalar.
#[test]
fn yaml_null_leaf_emits_key_path_with_empty_scalar() {
    // `password:` has a null value; `other: x` is a sibling scalar to prove the
    // null leaf is emitted as its own answer-granularity symbol, not dropped.
    let source = "password:\nother: x\n";

    let out = extract_artifact(ArtifactKind::Yaml, "config/secrets.yaml", source);
    assert!(
        !out.fell_back,
        "a document with structured leaves must not fall back"
    );

    let password = out
        .symbols
        .iter()
        .find(|s| s.name == "password")
        .expect("null leaf `password` must be emitted as its own symbol");

    assert_eq!(
        password.kind,
        SymbolKind::Var,
        "a config leaf is a Var symbol"
    );
    // Searchable text is exactly the key path (empty scalar → no value appended).
    assert_eq!(
        password.body_excerpt.as_deref(),
        Some("password"),
        "a null/empty leaf's searchable text is just the key path"
    );
    password
        .validate()
        .expect("null-leaf symbol must satisfy Symbol::validate");

    // The sibling scalar is still present and carries its value, confirming the
    // null leaf did not truncate or displace the rest of the document.
    let other = out
        .symbols
        .iter()
        .find(|s| s.name == "other")
        .expect("sibling scalar `other` must be emitted");
    assert!(
        other.body_excerpt.as_deref().unwrap_or("").contains("x"),
        "sibling scalar retains its value"
    );
}

/// Req 2.4 (null/empty leaf, TOML): an empty-string TOML value likewise yields a
/// leaf whose searchable text is just the key path with an empty scalar.
#[test]
fn toml_empty_value_emits_key_path_with_empty_scalar() {
    let source = "password = \"\"\nother = \"x\"\n";

    let out = extract_artifact(ArtifactKind::Toml, "config/secrets.toml", source);
    assert!(!out.fell_back);

    let password = out
        .symbols
        .iter()
        .find(|s| s.name == "password")
        .expect("empty-value leaf `password` must be emitted");
    assert_eq!(password.kind, SymbolKind::Var);
    assert_eq!(
        password.body_excerpt.as_deref(),
        Some("password"),
        "an empty TOML scalar's searchable text is just the key path"
    );
    password.validate().expect("symbol must be valid");
}
