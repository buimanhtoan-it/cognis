//! Property-based test for secret redaction of artifact values (Task 3.3).
//!
//! Feature: non-code-artifact-coverage, Property 8: Secret redaction of artifact
//! values
//!
//! Validates: Requirements 2.8
//!
//! ## The property
//!
//! *For any* YAML/TOML leaf whose scalar value is secret-shaped, when
//! `security.redact_secrets` is enabled the persisted symbol's text does not
//! contain the raw secret and the symbol is flagged `secret_redacted`.
//!
//! ## How it is driven
//!
//! The YAML/TOML extractor
//! (`cognis_indexer::parser::artifact::extract_artifact`) places each leaf's
//! scalar value into the symbol's `body_excerpt`
//! (`crates/cognis-indexer/src/parser/artifact/yaml.rs`), which is exactly the
//! field the enricher's secret-redaction path scrubs. Enabling
//! `security.redact_secrets` is realized by running the real
//! [`Enricher`](cognis_indexer::Enricher) — the same instance the pipeline
//! constructs in `parse_and_enrich` and applies to every emitted symbol before
//! the Writer persists it. `redact_secrets` defaults to `true`
//! (`crates/cognis-core/src/config.rs`) and the enricher's redaction runs on
//! that enabled path, so `Enricher::new()` models "redaction enabled".
//!
//! Each case generates a one-leaf document whose scalar value is a
//! secret-shaped token that matches a known-shape detector pattern in
//! `crates/cognis-indexer/src/enricher/secrets.rs` (AWS access key, GitHub PAT,
//! OpenAI key, Slack token). These shapes are deliberately restricted to
//! characters that survive YAML plain-scalar / TOML basic-string parsing intact
//! (no `#`, no `": "`, no quotes), so the *raw* secret reaches `body_excerpt`
//! before enrichment. The test then extracts → enriches → asserts the raw
//! secret is gone from every searchable text field and the `secret_redacted`
//! flag is present on the persisted symbol.

use cognis_indexer::parser::artifact::extract_artifact;
use cognis_indexer::{ArtifactKind, Enricher};
use proptest::prelude::*;

/// Secret-shaped scalar values, each matching a known-shape pattern in the
/// enricher's [`SecretDetector`]. Every shape uses only characters that pass
/// through YAML plain-scalar and TOML basic-string parsing unchanged, so the raw
/// token reaches the symbol's `body_excerpt` before redaction runs:
///
/// * `AKIA[0-9A-Z]{16}`            → `aws-access-key`
/// * `ghp_[A-Za-z0-9]{36}`         → `github-pat`
/// * `sk-[A-Za-z0-9]{20,40}`       → `openai-key`
/// * `xoxb-[A-Za-z0-9]{10,30}`     → `slack-token`
fn secret_strategy() -> impl Strategy<Value = String> {
    prop_oneof![
        "AKIA[0-9A-Z]{16}",
        "ghp_[A-Za-z0-9]{36}",
        "sk-[A-Za-z0-9]{20,40}",
        "xoxb-[A-Za-z0-9]{10,30}",
    ]
}

proptest! {
    // Minimum 100 iterations per the spec; one test for Property 8. Each case
    // extracts a fresh one-leaf artifact document and runs the real enricher.
    #![proptest_config(ProptestConfig::with_cases(100))]

    // Feature: non-code-artifact-coverage, Property 8: Secret redaction of
    // artifact values
    #[test]
    fn secret_shaped_artifact_values_are_redacted_and_flagged(
        // A simple leaf key: lowercase identifier, safe as a YAML/TOML key.
        key in "[a-z][a-z0-9_]{0,11}",
        secret in secret_strategy(),
        // Exercise both surfaces the extractor supports.
        is_toml in any::<bool>(),
    ) {
        // Build a document with exactly one leaf whose scalar value is the
        // secret. YAML uses a plain scalar; TOML uses a basic (quoted) string —
        // both unquote/trim to the raw secret in `body_excerpt`.
        let (kind, path, doc) = if is_toml {
            (
                ArtifactKind::Toml,
                "config/app.toml",
                format!("{key} = \"{secret}\"\n"),
            )
        } else {
            (
                ArtifactKind::Yaml,
                "config/app.yaml",
                format!("{key}: {secret}\n"),
            )
        };

        let out = extract_artifact(kind, path, &doc);

        // A single well-formed leaf must extract structurally, never via the
        // whole-file textual fallback.
        prop_assert!(
            !out.fell_back,
            "single-leaf document must extract structurally, not fall back: {doc:?}"
        );

        // Pre-enrich sanity: the extractor must have placed the *raw* secret into
        // a symbol's searchable text (`body_excerpt`), which is what the
        // redaction path scrubs (Req 2.8).
        let leaf = out
            .symbols
            .iter()
            .find(|s| {
                s.body_excerpt
                    .as_deref()
                    .is_some_and(|b| b.contains(&secret))
            })
            .expect("extractor must place the raw scalar value into body_excerpt");

        // Enrich with redaction enabled (the pipeline's real enricher; the
        // `security.redact_secrets` default is `true`).
        let enriched = Enricher::new().enrich(leaf);

        // 1. The raw secret must not survive in any redacted searchable field of
        //    the persisted symbol.
        for field in [
            enriched.symbol.body_excerpt.as_deref(),
            enriched.symbol.signature.as_deref(),
            enriched.symbol.docstring.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            prop_assert!(
                !field.contains(&secret),
                "raw secret leaked into persisted symbol text: field={field:?} secret={secret:?}"
            );
        }

        // 2. The symbol must carry the `secret_redacted` taint flag — both on the
        //    returned flag list and on the persisted symbol itself.
        prop_assert!(
            enriched
                .untrusted_flags
                .iter()
                .any(|f| f == "secret_redacted"),
            "expected secret_redacted in untrusted_flags, got {:?}",
            enriched.untrusted_flags
        );
        prop_assert!(
            enriched
                .symbol
                .untrusted_flags
                .iter()
                .any(|f| f == "secret_redacted"),
            "persisted symbol must carry the secret_redacted flag, got {:?}",
            enriched.symbol.untrusted_flags
        );
    }
}
