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
        assert_eq!(c.mcp.allow_tools.len(), 8);
        assert!(c.security.redact_secrets);
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
