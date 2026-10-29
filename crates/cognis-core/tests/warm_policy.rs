//! Property 5 unit suite — warm policy is consumed with documented precedence.
//!
//! Task 4.3 of the `mcp-process-ram-duplication` bugfix spec. This dedicated
//! integration test file is the "Property 5" resolver suite; it is deliberately
//! separate from the co-located smoke checks in
//! `crates/cognis-core/src/warm_policy.rs` so the exhaustive precedence coverage
//! lives in one clearly-labeled place.
//!
//! **Property 5: Bug Condition** — the resolved [`SemanticWarmPolicy`] honors the
//! finalized precedence (Task 4.1):
//!
//! * `"1"` → `Eager`
//! * `"0"` → `Lazy`
//! * absent → `Eager` (legacy / direct-launch)
//! * invalid / empty / whitespace-only → `Eager` (+ stderr warning)
//! * whitespace around an accepted value is trimmed
//!
//! The key Property 5 clause is that the **extension's generated default** — the
//! value the extension emits when the user disables warm startup — is `"0"` and
//! must resolve to `Lazy`.
//!
//! **Validates: Requirements 2.4**
//!
//! Cases are exercised through [`SemanticWarmPolicy::from_env_value`] for
//! determinism: it takes an explicit `Option<&str>`, so the table-driven suite
//! never touches the process-global environment and is safe under Rust's default
//! parallel test execution. A small, serialized `from_env` check appears at the
//! end and mutates the real env var under a mutex so it cannot race the other
//! `from_env` case.

use std::sync::Mutex;

use cognis_core::{SemanticWarmPolicy, WARM_SEMANTIC_ENV};

/// The exact value the extension writes into generated MCP config by default
/// (`cognis.mcpWarmSemanticOnStartup = false`). Property 5's headline clause:
/// this must resolve to `Lazy`.
const EXTENSION_GENERATED_DEFAULT: &str = "0";

// ---------------------------------------------------------------------------
// Accepted values
// ---------------------------------------------------------------------------

#[test]
fn one_resolves_to_eager() {
    assert_eq!(
        SemanticWarmPolicy::from_env_value(Some("1")),
        SemanticWarmPolicy::Eager,
        "\"1\" must select eager semantic warm-up",
    );
}

#[test]
fn zero_resolves_to_lazy() {
    assert_eq!(
        SemanticWarmPolicy::from_env_value(Some("0")),
        SemanticWarmPolicy::Lazy,
        "\"0\" must select lazy (deferred) semantic init",
    );
}

// ---------------------------------------------------------------------------
// Property 5 headline clause: extension's generated default → Lazy
// ---------------------------------------------------------------------------

#[test]
fn extension_generated_default_resolves_to_lazy() {
    // The extension's shipped default emits "0". That value MUST resolve to
    // Lazy or the lazy-lifecycle fix is dead on arrival at the process boundary
    // (this is the bug facet Property 5 guards against).
    assert_eq!(
        EXTENSION_GENERATED_DEFAULT, "0",
        "the extension's shipped default must be the string \"0\"",
    );
    assert_eq!(
        SemanticWarmPolicy::from_env_value(Some(EXTENSION_GENERATED_DEFAULT)),
        SemanticWarmPolicy::Lazy,
        "the extension's generated default (\"0\") must resolve to Lazy",
    );
    assert!(
        SemanticWarmPolicy::from_env_value(Some(EXTENSION_GENERATED_DEFAULT)).is_lazy(),
        "extension default must report is_lazy()",
    );
}

// ---------------------------------------------------------------------------
// Absent variable → Eager (legacy / direct-launch)
// ---------------------------------------------------------------------------

#[test]
fn absent_resolves_to_eager() {
    assert_eq!(
        SemanticWarmPolicy::from_env_value(None),
        SemanticWarmPolicy::Eager,
        "an absent variable must keep the legacy eager-warm behavior",
    );
}

// ---------------------------------------------------------------------------
// Invalid values → Eager (safe fallback, with a warning)
// ---------------------------------------------------------------------------

#[test]
fn invalid_values_resolve_to_eager() {
    // Unrecognized tokens, booleans/synonyms that are NOT accepted, and numeric
    // look-alikes all fall back to Eager. Only the literal "1" / "0" are
    // accepted values.
    let invalid = [
        "maybe", "true", "false", "2", "01", "yes", "no", "on", "off", "TRUE", "Lazy", "eager",
        "-1", "1.0", "10", "00", "1 0", "yeah", "enable", "disable",
    ];
    for value in invalid {
        assert_eq!(
            SemanticWarmPolicy::from_env_value(Some(value)),
            SemanticWarmPolicy::Eager,
            "invalid value {value:?} must fall back to Eager",
        );
    }
}

#[test]
fn empty_resolves_to_eager() {
    assert_eq!(
        SemanticWarmPolicy::from_env_value(Some("")),
        SemanticWarmPolicy::Eager,
        "empty string must fall back to Eager",
    );
}

#[test]
fn whitespace_only_resolves_to_eager() {
    for value in [" ", "   ", "\t", "\n", "\r\n", " \t \n "] {
        assert_eq!(
            SemanticWarmPolicy::from_env_value(Some(value)),
            SemanticWarmPolicy::Eager,
            "whitespace-only value {value:?} must fall back to Eager",
        );
    }
}

// ---------------------------------------------------------------------------
// Whitespace around an accepted value is trimmed
// ---------------------------------------------------------------------------

#[test]
fn surrounding_whitespace_is_trimmed_for_accepted_values() {
    for value in [" 1 ", "\t1", "1\n", "  1\r\n"] {
        assert_eq!(
            SemanticWarmPolicy::from_env_value(Some(value)),
            SemanticWarmPolicy::Eager,
            "trimmed {value:?} must resolve to Eager",
        );
    }
    for value in ["\t0\n", " 0 ", "0 ", "  0\r\n"] {
        assert_eq!(
            SemanticWarmPolicy::from_env_value(Some(value)),
            SemanticWarmPolicy::Lazy,
            "trimmed {value:?} must resolve to Lazy",
        );
    }
}

// ---------------------------------------------------------------------------
// Table-driven precedence matrix (the whole Property 5 contract in one place)
// ---------------------------------------------------------------------------

#[test]
fn precedence_matrix_holds() {
    // (input, expected). `None` models an absent variable.
    let cases: &[(Option<&str>, SemanticWarmPolicy)] = &[
        (Some("1"), SemanticWarmPolicy::Eager),
        (Some("0"), SemanticWarmPolicy::Lazy),
        (None, SemanticWarmPolicy::Eager),
        (Some(""), SemanticWarmPolicy::Eager),
        (Some("   "), SemanticWarmPolicy::Eager),
        (Some("maybe"), SemanticWarmPolicy::Eager),
        (Some("true"), SemanticWarmPolicy::Eager),
        (Some("false"), SemanticWarmPolicy::Eager),
        (Some("2"), SemanticWarmPolicy::Eager),
        (Some("01"), SemanticWarmPolicy::Eager),
        (Some(" 1 "), SemanticWarmPolicy::Eager),
        (Some("\t0\n"), SemanticWarmPolicy::Lazy),
    ];

    for (input, expected) in cases {
        assert_eq!(
            SemanticWarmPolicy::from_env_value(*input),
            *expected,
            "input {input:?} must resolve to {expected:?}",
        );
    }
}

// ---------------------------------------------------------------------------
// Helpers agree with the variants; Default == Eager; env const matches contract
// ---------------------------------------------------------------------------

#[test]
fn is_eager_and_is_lazy_agree_with_variants() {
    assert!(SemanticWarmPolicy::Eager.is_eager());
    assert!(!SemanticWarmPolicy::Eager.is_lazy());
    assert!(SemanticWarmPolicy::Lazy.is_lazy());
    assert!(!SemanticWarmPolicy::Lazy.is_eager());

    // Helpers are exact complements for every resolvable value.
    for input in [Some("1"), Some("0"), None, Some("garbage"), Some(" ")] {
        let policy = SemanticWarmPolicy::from_env_value(input);
        assert_ne!(
            policy.is_eager(),
            policy.is_lazy(),
            "is_eager() and is_lazy() must be exact complements for {input:?}",
        );
    }
}

#[test]
fn default_is_eager() {
    assert_eq!(
        SemanticWarmPolicy::default(),
        SemanticWarmPolicy::Eager,
        "Default must match the legacy / direct-launch eager behavior",
    );
    assert!(SemanticWarmPolicy::default().is_eager());
}

#[test]
fn env_const_matches_extension_contract() {
    // The resolver's env-var name must stay byte-for-byte identical to the
    // string the extension writes (`resolveMcpEnvOverrides`).
    assert_eq!(WARM_SEMANTIC_ENV, "COGNIS_MCP_WARM_SEMANTIC_ON_STARTUP");
}

// ---------------------------------------------------------------------------
// from_env: exercised through the real process env under a mutex so the two
// mutations cannot race each other under parallel test execution.
// ---------------------------------------------------------------------------

static ENV_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn from_env_reads_the_documented_variable() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let previous = std::env::var(WARM_SEMANTIC_ENV).ok();

    // "0" via the real env resolves to Lazy (the extension-default path).
    std::env::set_var(WARM_SEMANTIC_ENV, "0");
    assert_eq!(SemanticWarmPolicy::from_env(), SemanticWarmPolicy::Lazy);

    // "1" via the real env resolves to Eager.
    std::env::set_var(WARM_SEMANTIC_ENV, "1");
    assert_eq!(SemanticWarmPolicy::from_env(), SemanticWarmPolicy::Eager);

    // Absent variable resolves to Eager (legacy / direct-launch).
    std::env::remove_var(WARM_SEMANTIC_ENV);
    assert_eq!(SemanticWarmPolicy::from_env(), SemanticWarmPolicy::Eager);

    // Restore whatever the environment had before this test ran.
    match previous {
        Some(value) => std::env::set_var(WARM_SEMANTIC_ENV, value),
        None => std::env::remove_var(WARM_SEMANTIC_ENV),
    }
}
