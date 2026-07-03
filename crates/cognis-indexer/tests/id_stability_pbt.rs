//! Property-based tests for the content-hash / symbol-id normalizer
//! (Requirement 9.2 parity, CP-1 idempotency, CP-2 id stability).
//!
//! Properties exercised:
//!  - P-ID-WS:   re-indentation / extra blank lines never change a symbol id.
//!  - P-ID-CMT:  trailing line-comment text never changes a symbol id.
//!  - P-ID-NAME: renaming the function changes the id.

use cognis_indexer::parse_source;
use proptest::prelude::*;

/// Extract the single function's id from a tiny Python module.
fn py_fn_id(body_indent: &str, blanks: usize, comment: &str) -> Option<String> {
    let pad = "\n".repeat(blanks);
    let src = format!(
        "def target(a, b):{pad}\n{body_indent}return a + b  # {comment}\n",
        pad = pad
    );
    let out = parse_source("m.py", &src);
    out.symbols
        .into_iter()
        .find(|s| s.name == "target")
        .map(|s| s.id)
}

proptest! {
    // P-ID-WS + P-ID-CMT: cosmetic edits (indentation width, blank-line count,
    // and comment text) must not change the symbol id.
    #![proptest_config(ProptestConfig::with_cases(200))]

    #[test]
    fn cosmetic_edits_preserve_id(
        spaces_a in 4usize..12,
        spaces_b in 4usize..12,
        blanks_a in 0usize..3,
        blanks_b in 0usize..3,
        comment_a in "[a-zA-Z ]{0,20}",
        comment_b in "[a-zA-Z ]{0,20}",
    ) {
        let id_a = py_fn_id(&" ".repeat(spaces_a), blanks_a, &comment_a);
        let id_b = py_fn_id(&" ".repeat(spaces_b), blanks_b, &comment_b);
        prop_assert_eq!(id_a, id_b);
    }
}

proptest! {
    // P-ID-NAME: a structural change (rename) must change the id.
    #[test]
    fn rename_changes_id(stem in "[a-z][a-z0-9_]{1,15}") {
        // Prefix with `fn_` so the generated name is always a valid Python
        // identifier and never a reserved keyword (e.g. "as", "if", "def").
        let name = format!("fn_{stem}");
        let base = parse_source("m.py", "def target(a):\n    return a\n");
        let renamed_src = format!("def {name}(a):\n    return a\n");
        let renamed = parse_source("m.py", &renamed_src);

        let base_id = base.symbols.iter().find(|s| s.name == "target").map(|s| &s.id);
        let new_id = renamed.symbols.iter().find(|s| s.name == name).map(|s| &s.id);
        prop_assert!(base_id.is_some() && new_id.is_some());
        prop_assert_ne!(base_id.unwrap(), new_id.unwrap());
    }
}
