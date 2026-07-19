//! Semantic warm policy — resolves `COGNIS_MCP_WARM_SEMANTIC_ON_STARTUP`.
//!
//! The VS Code / Cursor extension emits `COGNIS_MCP_WARM_SEMANTIC_ON_STARTUP`
//! (`resolveMcpEnvOverrides` in `apps/cognis-vscode/src/mcpConfig.ts`) to signal
//! whether a freshly spawned heavy daemon should build its embedder eagerly at
//! `open` (`"1"`) or defer it until the first semantic demand (`"0"`). Until this
//! fix the Rust engine never read that variable, so the signal was dropped at the
//! process boundary and "lazy" was never actually lazy
//! (bug facet `semanticWarmPolicyIsIgnoredOrInconsistent`).
//!
//! This module provides the single, documented resolver the engines consume so
//! the warm policy is honored with a stable precedence (Requirement 2.4;
//! Correctness Property 5).
//!
//! ## Accepted values and precedence
//!
//! The value is read from the environment and matched after trimming surrounding
//! ASCII whitespace. Only the two values the extension emits are accepted:
//!
//! | Env value                       | Resolved policy     |
//! |---------------------------------|---------------------|
//! | `"1"`                           | [`Eager`]           |
//! | `"0"`                           | [`Lazy`]            |
//! | absent / unset                  | [`Eager`] (default) |
//! | empty / whitespace-only         | [`Eager`] (+warn)   |
//! | any other value                 | [`Eager`] (+warn)   |
//!
//! [`Eager`]: SemanticWarmPolicy::Eager
//! [`Lazy`]: SemanticWarmPolicy::Lazy
//!
//! **Accepted values:** `"1"` (eager) and `"0"` (lazy). These are the only
//! values the extension writes via `resolveMcpEnvOverrides`.
//!
//! **Invalid values:** any non-empty string other than `"1"` or `"0"` after
//! trimming (including `"true"`, `"false"`, `"yes"`, `"no"`, `"on"`, `"off"`,
//! numeric values other than `0`/`1`, free text), and the empty /
//! whitespace-only string. Invalid values resolve to [`Eager`] and log a
//! warning.
//!
//! **Precedence / rationale:**
//!
//! * An **absent** variable resolves to [`Eager`]. A process launched directly
//!   (legacy invocation, CLI, or a host that predates this contract) sees no
//!   variable and must keep the original eager-warm behavior so semantic tools
//!   do not regress. The extension's generated config always sets the variable
//!   explicitly, so the absent case is exclusively the legacy / direct-launch
//!   path.
//! * An **invalid** value (empty, whitespace-only, or unrecognized) resolves to
//!   [`Eager`] and logs a warning to stderr. Eager is the safe fallback: it
//!   never leaves semantic tools waiting on an unexpectedly deferred load, and
//!   the warning surfaces the misconfiguration without failing startup.
//! * The environment variable is the only input; there is no config-file or
//!   compile-time override for the warm policy, so precedence is unambiguous.
//!
//! `cognis-core` is the dependency-neutral foundation crate (no `log` / `tracing`
//! dependency), so the invalid-value warning is written directly to stderr with
//! [`eprintln!`]. Callers that want structured logging can resolve from an
//! explicit string via [`SemanticWarmPolicy::from_env_value`] and log the
//! outcome themselves.

/// The env var the extension emits to select eager vs lazy semantic startup.
pub const WARM_SEMANTIC_ENV: &str = "COGNIS_MCP_WARM_SEMANTIC_ON_STARTUP";

/// Whether a heavy daemon should build its embedder up front or on demand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SemanticWarmPolicy {
    /// Build/initialize the embedder up front, before semantic readiness.
    ///
    /// Selected by `"1"`, by an absent variable (legacy / direct-launch
    /// compatibility), and by any invalid value (safe fallback, with a logged
    /// warning).
    Eager,
    /// Defer embedder construction until the first semantic demand.
    ///
    /// Selected by `"0"`. This is the policy the extension's generated config
    /// resolves to when the user disables warm startup
    /// (`cognis.mcpWarmSemanticOnStartup = false`).
    Lazy,
}

impl Default for SemanticWarmPolicy {
    /// The default when no signal is present is [`Eager`](SemanticWarmPolicy::Eager),
    /// matching the legacy / direct-launch behavior.
    fn default() -> Self {
        SemanticWarmPolicy::Eager
    }
}

impl SemanticWarmPolicy {
    /// True when the embedder should be built up front at `open`.
    pub fn is_eager(self) -> bool {
        matches!(self, SemanticWarmPolicy::Eager)
    }

    /// True when embedder construction should be deferred to first demand.
    pub fn is_lazy(self) -> bool {
        matches!(self, SemanticWarmPolicy::Lazy)
    }

    /// Resolve the warm policy from the process environment.
    ///
    /// Reads [`WARM_SEMANTIC_ENV`] and applies the documented precedence:
    /// `"1"`→[`Eager`](SemanticWarmPolicy::Eager),
    /// `"0"`→[`Lazy`](SemanticWarmPolicy::Lazy),
    /// absent→[`Eager`](SemanticWarmPolicy::Eager) (legacy / direct-launch),
    /// invalid→[`Eager`](SemanticWarmPolicy::Eager) with a warning logged to
    /// stderr.
    pub fn from_env() -> Self {
        match std::env::var(WARM_SEMANTIC_ENV) {
            Ok(value) => Self::from_env_value(Some(&value)),
            // NotFound or non-Unicode both mean "no usable signal"; treat as absent.
            Err(_) => Self::from_env_value(None),
        }
    }

    /// Resolve the warm policy from an explicit optional value.
    ///
    /// `None` models an absent variable (→ [`Eager`](SemanticWarmPolicy::Eager),
    /// legacy / direct-launch). `Some(value)` is matched after trimming ASCII
    /// whitespace against the accepted values `"1"` and `"0"`. Unrecognized,
    /// empty, or whitespace-only values resolve to
    /// [`Eager`](SemanticWarmPolicy::Eager) and emit a warning to stderr.
    ///
    /// This is the testable core of [`from_env`](SemanticWarmPolicy::from_env);
    /// engines and unit tests should prefer this entry point when they already
    /// hold the raw env string (or want to inject one).
    pub fn from_env_value(value: Option<&str>) -> Self {
        let Some(raw) = value else {
            // Absent variable: legacy / direct-launch compatibility → Eager.
            return SemanticWarmPolicy::Eager;
        };

        match raw.trim() {
            "1" => SemanticWarmPolicy::Eager,
            "0" => SemanticWarmPolicy::Lazy,
            _ => {
                eprintln!(
                    "cognis: warning: {WARM_SEMANTIC_ENV}={raw:?} is not a recognized \
                     value (accepted: \"1\" = eager, \"0\" = lazy); defaulting to \
                     eager semantic warm-up"
                );
                SemanticWarmPolicy::Eager
            }
        }
    }
}

// Minimal co-located smoke checks so `cargo test -p cognis-core` exercises the
// resolver. The full Property 5 unit suite (including extension-default → Lazy
// and invalid→warn coverage) lives in Task 4.3.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_is_eager() {
        assert_eq!(
            SemanticWarmPolicy::from_env_value(Some("1")),
            SemanticWarmPolicy::Eager
        );
    }

    #[test]
    fn zero_is_lazy() {
        assert_eq!(
            SemanticWarmPolicy::from_env_value(Some("0")),
            SemanticWarmPolicy::Lazy
        );
    }

    #[test]
    fn absent_is_eager() {
        assert_eq!(
            SemanticWarmPolicy::from_env_value(None),
            SemanticWarmPolicy::Eager
        );
    }

    #[test]
    fn invalid_is_eager() {
        assert_eq!(
            SemanticWarmPolicy::from_env_value(Some("maybe")),
            SemanticWarmPolicy::Eager
        );
        assert_eq!(
            SemanticWarmPolicy::from_env_value(Some("")),
            SemanticWarmPolicy::Eager
        );
        assert_eq!(
            SemanticWarmPolicy::from_env_value(Some("   ")),
            SemanticWarmPolicy::Eager
        );
        // Synonyms are NOT accepted; only "1" / "0".
        assert_eq!(
            SemanticWarmPolicy::from_env_value(Some("true")),
            SemanticWarmPolicy::Eager
        );
        assert_eq!(
            SemanticWarmPolicy::from_env_value(Some("false")),
            SemanticWarmPolicy::Eager
        );
    }

    #[test]
    fn whitespace_around_accepted_value_is_tolerated() {
        assert_eq!(
            SemanticWarmPolicy::from_env_value(Some(" 1 ")),
            SemanticWarmPolicy::Eager
        );
        assert_eq!(
            SemanticWarmPolicy::from_env_value(Some("\t0\n")),
            SemanticWarmPolicy::Lazy
        );
    }

    #[test]
    fn default_is_eager() {
        assert_eq!(SemanticWarmPolicy::default(), SemanticWarmPolicy::Eager);
    }

    #[test]
    fn helpers_agree_with_variant() {
        assert!(SemanticWarmPolicy::Eager.is_eager());
        assert!(!SemanticWarmPolicy::Eager.is_lazy());
        assert!(SemanticWarmPolicy::Lazy.is_lazy());
        assert!(!SemanticWarmPolicy::Lazy.is_eager());
    }

    #[test]
    fn env_const_matches_extension_contract() {
        assert_eq!(WARM_SEMANTIC_ENV, "COGNIS_MCP_WARM_SEMANTIC_ON_STARTUP");
    }
}
