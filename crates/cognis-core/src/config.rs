//! Typed config — Rust mirror of `packages/core/cognis/config.py`.
//!
//! Same YAML schema and defaults as the pydantic `Config`. `#[serde(default)]`
//! on every field/section reproduces pydantic's `default_factory` behavior so a
//! partial or absent `.cognis/config.yaml` yields the baked-in defaults
//! (Requirement 7.1; matches `Config.load`).

use serde::{Deserialize, Serialize};

use crate::CognisError;

pub const CONFIG_DIR_NAME: &str = ".cognis";
pub const CONFIG_FILE_NAME: &str = "config.yaml";
pub const CONFIG_REVISION: u32 = 1;

fn default_repo_ignore() -> Vec<String> {
    [
        "node_modules",
        ".git",
        "dist",
        "target",
        "__pycache__",
        ".venv",
        "reference",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

fn default_languages() -> Vec<String> {
    [
        "typescript",
        "python",
        "go",
        "csharp",
        "java",
        "rust",
        "c",
        "cpp",
        "ruby",
        "php",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

fn default_allow_tools() -> Vec<String> {
    crate::contract::MCP_TOOLS
        .iter()
        .map(|s| s.to_string())
        .collect()
}

/// Bounded retry ladder for a failed embedding attempt, in milliseconds
/// (1 s → 5 s → 30 s → 5 min). The last rung is the steady-state retry
/// interval: the ladder is bounded, so a permanently failing row retries
/// forever at the final delay and never grows without limit (Requirement 11.7).
fn default_embed_backoff_ms() -> Vec<u64> {
    vec![1_000, 5_000, 30_000, 300_000]
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RepoConfig {
    pub root: String,
    pub ignore: Vec<String>,
}
impl Default for RepoConfig {
    fn default() -> Self {
        Self {
            root: ".".into(),
            ignore: default_repo_ignore(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LanguagesConfig {
    pub enabled: Vec<String>,
}
impl Default for LanguagesConfig {
    fn default() -> Self {
        Self {
            enabled: default_languages(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EmbedderConfig {
    pub backend: String,
    pub model: String,
    pub dim: u32,
    pub batch_size: u32,
}
impl Default for EmbedderConfig {
    fn default() -> Self {
        Self {
            backend: "local".into(),
            model: "BAAI/bge-small-en-v1.5".into(),
            dim: 384,
            batch_size: 32,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RerankerConfig {
    pub backend: String,
    pub model: String,
    pub enabled: bool,
}
impl Default for RerankerConfig {
    fn default() -> Self {
        Self {
            backend: "local".into(),
            model: "bge-reranker-v2-m3".into(),
            enabled: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GraphConfig {
    pub edge_resolver: String,
    pub max_depth: u8,
}
impl Default for GraphConfig {
    fn default() -> Self {
        Self {
            edge_resolver: "lsp_then_heuristic".into(),
            max_depth: 5,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct McpConfig {
    pub transport: Vec<String>,
    pub sse_port: u16,
    pub allow_tools: Vec<String>,
}
impl Default for McpConfig {
    fn default() -> Self {
        Self {
            transport: vec!["stdio".into()],
            sse_port: 7464,
            allow_tools: default_allow_tools(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PlannerConfig {
    pub default_max_tokens: u32,
    pub classifier: String,
}
impl Default for PlannerConfig {
    fn default() -> Self {
        Self {
            default_max_tokens: 8000,
            classifier: "rule_based".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SecurityConfig {
    pub redact_secrets: bool,
    pub taint_untrusted: bool,
    pub audit_log: String,
}
impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            redact_secrets: true,
            taint_untrusted: true,
            audit_log: ".cognis/audit.log".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EvalConfig {
    pub golden_set: String,
}
impl Default for EvalConfig {
    fn default() -> Self {
        Self {
            golden_set: ".cognis/eval/golden.jsonl".into(),
        }
    }
}

/// Non-code artifact coverage. Additive, defaulted section that gates artifact
/// admission (`enabled`), lists deploy/CI descriptor filename patterns
/// (`ci_descriptors`), and toggles integration-edge capsule context
/// (`integration_edge_context`). Population-only feature — no schema migration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ArtifactConfig {
    pub enabled: bool,
    pub ci_descriptors: Vec<String>,
    pub integration_edge_context: bool,
}
impl Default for ArtifactConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            ci_descriptors: Vec::new(),
            integration_edge_context: false,
        }
    }
}

/// `Workspace_Owner` runtime tuning. Additive, fully defaulted section: an
/// absent `owner:` key yields [`OwnerConfig::default`], so every `config.yaml`
/// written before this section existed keeps parsing unchanged.
///
/// * `watcher_queue_max` — fixed maximum `Watcher_Queue` entry count; reaching
///   it collapses the queued entries into a single reconciliation request
///   (Requirement 18.1, 18.3).
/// * `watcher_debounce_ms` — coalescing window applied before a queued batch is
///   handed to the planner.
/// * `embed_batch_budget` — maximum `Embedding_Queue` rows drained per pass.
/// * `embed_backoff_ms` — bounded retry ladder for a failed embed, indexed by
///   attempt count and clamped at the last rung (Requirement 11.7).
/// * `idle_evict_after_s` — idle interval after which the model session is
///   released, provided the queue is empty and no tool call is in flight
///   (Requirement 20.2).
/// * `lease_ttl_s` — owner-lease time-to-live; matches
///   [`crate::DEFAULT_LEASE_TTL`] (30 s).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OwnerConfig {
    pub watcher_queue_max: u32,
    pub watcher_debounce_ms: u64,
    pub embed_batch_budget: u32,
    pub embed_backoff_ms: Vec<u64>,
    pub idle_evict_after_s: u64,
    pub lease_ttl_s: u64,
}
impl Default for OwnerConfig {
    fn default() -> Self {
        Self {
            watcher_queue_max: 4096,
            watcher_debounce_ms: 250,
            embed_batch_budget: 64,
            embed_backoff_ms: default_embed_backoff_ms(),
            idle_evict_after_s: 900,
            lease_ttl_s: 30,
        }
    }
}

impl OwnerConfig {
    /// Backoff delay in milliseconds for retry `attempt` (0-based), clamped to
    /// the last rung so the ladder stays bounded (Requirement 11.7).
    ///
    /// An empty ladder yields `0` — retry immediately rather than panic.
    pub fn backoff_ms_for_attempt(&self, attempt: u32) -> u64 {
        if self.embed_backoff_ms.is_empty() {
            return 0;
        }
        let idx = (attempt as usize).min(self.embed_backoff_ms.len() - 1);
        self.embed_backoff_ms[idx]
    }
}

/// Top-level config. Every section has defaults; an empty document is valid.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub repo: RepoConfig,
    pub languages: LanguagesConfig,
    pub embedder: EmbedderConfig,
    pub reranker: RerankerConfig,
    pub graph: GraphConfig,
    pub mcp: McpConfig,
    pub planner: PlannerConfig,
    pub security: SecurityConfig,
    pub eval: EvalConfig,
    pub artifact: ArtifactConfig,
    pub owner: OwnerConfig,
}

impl Config {
    /// Parse from a YAML string (round-trip safe with [`Config::to_yaml`]).
    pub fn from_yaml_str(text: &str) -> crate::Result<Self> {
        if text.trim().is_empty() {
            return Ok(Self::default());
        }
        serde_yaml::from_str(text).map_err(|e| CognisError::Config(e.to_string()))
    }

    /// Load `<repo_root>/.cognis/config.yaml`, or defaults when absent.
    pub fn load(repo_root: impl AsRef<std::path::Path>) -> crate::Result<Self> {
        let path = repo_root
            .as_ref()
            .join(CONFIG_DIR_NAME)
            .join(CONFIG_FILE_NAME);
        match std::fs::read_to_string(&path) {
            Ok(text) => Self::from_yaml_str(&text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(CognisError::Config(format!("read {}: {e}", path.display()))),
        }
    }

    pub fn to_yaml(&self) -> crate::Result<String> {
        serde_yaml::to_string(self).map_err(|e| CognisError::Config(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_yaml_is_default() {
        assert_eq!(Config::from_yaml_str("").unwrap(), Config::default());
    }

    #[test]
    fn defaults_match_python_schema() {
        let c = Config::default();
        assert_eq!(c.embedder.model, "BAAI/bge-small-en-v1.5");
        assert_eq!(c.embedder.dim, 384);
        assert_eq!(c.graph.max_depth, 5);
        assert_eq!(c.mcp.sse_port, 7464);
        assert_eq!(c.mcp.allow_tools.len(), crate::contract::MCP_TOOLS.len());
        assert!(c.security.redact_secrets);
    }

    #[test]
    fn artifact_defaults() {
        let c = Config::default();
        assert!(c.artifact.enabled);
        assert!(c.artifact.ci_descriptors.is_empty());
        assert!(!c.artifact.integration_edge_context);
    }

    #[test]
    fn empty_yaml_yields_artifact_default() {
        let c = Config::from_yaml_str("").unwrap();
        assert_eq!(c.artifact, ArtifactConfig::default());
    }

    #[test]
    fn partial_artifact_yaml_fills_defaults() {
        let c = Config::from_yaml_str("artifact:\n  enabled: false\n").unwrap();
        assert!(!c.artifact.enabled);
        // unspecified keys keep defaults
        assert!(c.artifact.ci_descriptors.is_empty());
        assert!(!c.artifact.integration_edge_context);
    }

    #[test]
    fn artifact_unknown_key_is_rejected() {
        assert!(Config::from_yaml_str("artifact:\n  bogus: 1\n").is_err());
    }

    /// The documented default values (design §Configuration additions).
    #[test]
    fn owner_defaults() {
        let c = Config::default();
        assert_eq!(c.owner.watcher_queue_max, 4096);
        assert_eq!(c.owner.watcher_debounce_ms, 250);
        assert_eq!(c.owner.embed_batch_budget, 64);
        assert_eq!(
            c.owner.embed_backoff_ms,
            vec![1_000, 5_000, 30_000, 300_000]
        );
        assert_eq!(c.owner.idle_evict_after_s, 900);
        assert_eq!(c.owner.lease_ttl_s, 30);
    }

    /// `lease_ttl_s` must stay in lockstep with the lease module's default.
    #[test]
    fn owner_lease_ttl_matches_lease_module_default() {
        assert_eq!(
            OwnerConfig::default().lease_ttl_s,
            crate::DEFAULT_LEASE_TTL.as_secs()
        );
    }

    #[test]
    fn empty_yaml_yields_owner_default() {
        let c = Config::from_yaml_str("").unwrap();
        assert_eq!(c.owner, OwnerConfig::default());
    }

    /// Backward compatibility: a `config.yaml` written before the `owner`
    /// section existed still parses, and every other section keeps its value
    /// while `owner` falls back to the documented defaults.
    #[test]
    fn pre_owner_config_yaml_parses_and_yields_owner_defaults() {
        // Byte-for-byte the shape `cognis cli init` emitted before this feature.
        let legacy = "\
repo:
  root: .
  ignore:
  - node_modules
  - .git
languages:
  enabled:
  - typescript
  - python
embedder:
  backend: local
  model: BAAI/bge-small-en-v1.5
  dim: 384
  batch_size: 32
reranker:
  backend: local
  model: bge-reranker-v2-m3
  enabled: false
graph:
  edge_resolver: lsp_then_heuristic
  max_depth: 5
mcp:
  transport:
  - stdio
  sse_port: 7464
  allow_tools:
  - diffuse_context
planner:
  default_max_tokens: 8000
  classifier: rule_based
security:
  redact_secrets: true
  taint_untrusted: true
  audit_log: .cognis/audit.log
eval:
  golden_set: .cognis/eval/golden.jsonl
";
        let c = Config::from_yaml_str(legacy).unwrap();
        // Absent section → documented defaults.
        assert_eq!(c.owner, OwnerConfig::default());
        // Absent `artifact` section keeps working too.
        assert_eq!(c.artifact, ArtifactConfig::default());
        // Explicit sections are still honoured.
        assert_eq!(c.embedder.dim, 384);
        assert_eq!(c.mcp.sse_port, 7464);
        assert_eq!(c.repo.ignore, vec!["node_modules", ".git"]);
    }

    #[test]
    fn partial_owner_yaml_fills_defaults() {
        let c = Config::from_yaml_str("owner:\n  watcher_queue_max: 8\n").unwrap();
        assert_eq!(c.owner.watcher_queue_max, 8);
        // unspecified keys keep defaults
        assert_eq!(c.owner.watcher_debounce_ms, 250);
        assert_eq!(c.owner.embed_batch_budget, 64);
        assert_eq!(
            c.owner.embed_backoff_ms,
            vec![1_000, 5_000, 30_000, 300_000]
        );
        assert_eq!(c.owner.idle_evict_after_s, 900);
        assert_eq!(c.owner.lease_ttl_s, 30);
    }

    #[test]
    fn owner_backoff_ladder_is_overridable() {
        let c = Config::from_yaml_str("owner:\n  embed_backoff_ms: [10, 20]\n").unwrap();
        assert_eq!(c.owner.embed_backoff_ms, vec![10, 20]);
    }

    #[test]
    fn owner_unknown_key_is_rejected() {
        assert!(Config::from_yaml_str("owner:\n  bogus: 1\n").is_err());
    }

    /// The ladder is bounded: attempts past its length clamp to the last rung.
    #[test]
    fn owner_backoff_is_bounded_and_clamps() {
        let o = OwnerConfig::default();
        assert_eq!(o.backoff_ms_for_attempt(0), 1_000);
        assert_eq!(o.backoff_ms_for_attempt(1), 5_000);
        assert_eq!(o.backoff_ms_for_attempt(2), 30_000);
        assert_eq!(o.backoff_ms_for_attempt(3), 300_000);
        // Past the end: clamped, never unbounded growth, never a panic.
        assert_eq!(o.backoff_ms_for_attempt(4), 300_000);
        assert_eq!(o.backoff_ms_for_attempt(u32::MAX), 300_000);
    }

    #[test]
    fn owner_backoff_empty_ladder_is_zero() {
        let o = OwnerConfig {
            embed_backoff_ms: Vec::new(),
            ..OwnerConfig::default()
        };
        assert_eq!(o.backoff_ms_for_attempt(0), 0);
        assert_eq!(o.backoff_ms_for_attempt(9), 0);
    }

    #[test]
    fn yaml_roundtrip() {
        let c = Config::default();
        let y = c.to_yaml().unwrap();
        assert_eq!(Config::from_yaml_str(&y).unwrap(), c);
    }

    #[test]
    fn partial_yaml_fills_defaults() {
        let c = Config::from_yaml_str("embedder:\n  dim: 768\n").unwrap();
        assert_eq!(c.embedder.dim, 768);
        // unspecified keys keep defaults
        assert_eq!(c.embedder.model, "BAAI/bge-small-en-v1.5");
        assert_eq!(c.graph.max_depth, 5);
    }

    #[test]
    fn unknown_key_is_rejected() {
        assert!(Config::from_yaml_str("bogus_section: 1\n").is_err());
    }
}
