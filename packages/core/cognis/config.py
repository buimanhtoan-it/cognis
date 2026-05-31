"""Typed configuration loader for cognis.

Implements task 2.1 of ``.kiro/specs/cognis/tasks.md``. The YAML schema mirrors
the design document's *Configuration* section verbatim:

.. code-block:: yaml

    repo:
      root: .
      ignore: [node_modules, .git, dist, target, __pycache__, .venv, reference]

    languages:
      enabled: [typescript, python, go]

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
      transport: [stdio]
      sse_port: 7464
      allow_tools: [diffuse_context, symbol_lookup, symbol_search, discover_symbols, semantic_search, resolve_symbols, dependency_trace, retrieve_context_capsule]

    planner:
      default_max_tokens: 8000
      classifier: rule_based

    security:
      redact_secrets: true
      taint_untrusted: true
      audit_log: .cognis/audit.log

    eval:
      golden_set: .cognis/eval/golden.jsonl

Defaults are baked in: instantiating ``Config()`` yields the values above. The
loader (`Config.load`) reads ``<repo_root>/.cognis/config.yaml`` when present,
applies any pending additive migrations in memory, and otherwise returns the
default ``Config``. ``Config.to_yaml`` produces a round-trip-safe dump used by
the CP-9 property test in task 2.3.
"""

from __future__ import annotations

from copy import deepcopy
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Final, Literal, cast

import yaml
from pydantic import BaseModel, ConfigDict, Field, field_validator

# ---------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------

CONFIG_DIR_NAME: Final[str] = ".cognis"
"""Per-repo runtime directory holding ``config.yaml``, ``uckg.db`` and friends."""

CONFIG_FILE_NAME: Final[str] = "config.yaml"
"""Filename of the YAML config inside ``.cognis/``."""

CONFIG_REVISION_FILE_NAME: Final[str] = "config.revision"
"""Sidecar file storing the additive config migration revision."""

CONFIG_REVISION: Final[int] = 1
"""Current additive migration revision for ``.cognis/config.yaml``."""


# Literal aliases keep the enum surface in one place and feed mypy --strict.
EmbedderBackend = Literal["local", "voyage", "openai"]
RerankerBackend = Literal["local"]
EdgeResolver = Literal["lsp_only", "heuristic_only", "lsp_then_heuristic"]
McpTransport = Literal["stdio", "sse"]
PlannerClassifier = Literal["rule_based", "small_lm"]
McpToolName = Literal[
    "diffuse_context",
    "symbol_lookup",
    "symbol_search",
    "discover_symbols",
    "semantic_search",
    "resolve_symbols",
    "dependency_trace",
    "retrieve_context_capsule",
]

DEFAULT_REPO_IGNORE: Final[tuple[str, ...]] = (
    "node_modules",
    ".git",
    "dist",
    "target",
    "__pycache__",
    ".venv",
    "reference",
)
DEFAULT_MCP_ALLOW_TOOLS: Final[tuple[McpToolName, ...]] = (
    "diffuse_context",
    "symbol_lookup",
    "symbol_search",
    "discover_symbols",
    "semantic_search",
    "resolve_symbols",
    "dependency_trace",
    "retrieve_context_capsule",
)
_LEGACY_EMBEDDER_MODEL_ALIASES: Final[dict[str, str]] = {
    "bge-small-en-v1.5": "BAAI/bge-small-en-v1.5",
}


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class ConfigMigrationReport:
    """Outcome of one additive config migration attempt."""

    revision_from: int
    revision_to: int
    changes: tuple[str, ...]
    wrote_config: bool


def _parse_yaml_mapping(text: str) -> dict[str, Any]:
    """Parse a YAML document whose root must be a mapping."""
    raw: object = yaml.safe_load(text) if text.strip() else {}
    if raw is None:
        raw = {}
    if not isinstance(raw, dict):
        raise TypeError(f"cognis config root must be a mapping, got {type(raw).__name__}")
    return cast(dict[str, Any], raw)


def _config_revision_path(repo_root_or_cognis_dir: str | Path = ".") -> Path:
    """Return the additive migration revision sidecar path."""
    base = Path(repo_root_or_cognis_dir)
    if base.name != CONFIG_DIR_NAME:
        base = base / CONFIG_DIR_NAME
    return base / CONFIG_REVISION_FILE_NAME


def read_config_revision(repo_root_or_cognis_dir: str | Path = ".") -> int:
    """Return the stored config migration revision, or ``0`` when absent."""
    path = _config_revision_path(repo_root_or_cognis_dir)
    if not path.exists():
        return 0
    try:
        return max(0, int(path.read_text(encoding="utf-8").strip() or "0"))
    except ValueError:
        return 0


def write_config_revision(
    repo_root_or_cognis_dir: str | Path = ".",
    revision: int = CONFIG_REVISION,
) -> Path:
    """Persist the config migration revision sidecar."""
    path = _config_revision_path(repo_root_or_cognis_dir)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(f"{revision}\n", encoding="utf-8")
    return path


def _ensure_section(mapping: dict[str, Any], key: str) -> dict[str, Any]:
    """Return ``mapping[key]`` as a mutable mapping, creating it when absent."""
    value = mapping.get(key)
    if value is None:
        value = {}
        mapping[key] = value
    if not isinstance(value, dict):
        raise TypeError(f"cognis config section {key!r} must be a mapping")
    return cast(dict[str, Any], value)


def _append_missing_defaults(target: list[Any], defaults: tuple[str, ...]) -> list[str]:
    """Append every missing default entry exactly once, preserving user order."""
    missing = [value for value in defaults if value not in target]
    target.extend(missing)
    return missing


def _migrate_raw_config(
    raw: dict[str, Any],
    revision_from: int,
) -> tuple[dict[str, Any], list[str]]:
    """Apply additive config migrations to raw YAML data."""
    migrated = cast(dict[str, Any], deepcopy(raw))
    changes: list[str] = []

    if revision_from < 1:
        repo = _ensure_section(migrated, "repo")
        ignore_value = repo.get("ignore")
        if ignore_value is None:
            repo["ignore"] = list(DEFAULT_REPO_IGNORE)
            changes.append("repo.ignore: added default ignore entries including `reference`")
        elif not isinstance(ignore_value, list):
            raise TypeError("cognis config section `repo.ignore` must be a list")
        else:
            missing_ignore = _append_missing_defaults(ignore_value, DEFAULT_REPO_IGNORE)
            if missing_ignore:
                changes.append(
                    "repo.ignore: added " + ", ".join(f"`{item}`" for item in missing_ignore)
                )

        mcp = _ensure_section(migrated, "mcp")
        allow_tools_value = mcp.get("allow_tools")
        if allow_tools_value is None:
            mcp["allow_tools"] = list(DEFAULT_MCP_ALLOW_TOOLS)
            changes.append("mcp.allow_tools: added current default tool allowlist")
        elif not isinstance(allow_tools_value, list):
            raise TypeError("cognis config section `mcp.allow_tools` must be a list")
        else:
            missing_tools = _append_missing_defaults(
                allow_tools_value,
                tuple(DEFAULT_MCP_ALLOW_TOOLS),
            )
            if missing_tools:
                changes.append(
                    "mcp.allow_tools: added " + ", ".join(f"`{tool}`" for tool in missing_tools)
                )

        embedder = migrated.get("embedder")
        if embedder is not None:
            if not isinstance(embedder, dict):
                raise TypeError("cognis config section `embedder` must be a mapping")
            model = embedder.get("model")
            if isinstance(model, str):
                replacement = _LEGACY_EMBEDDER_MODEL_ALIASES.get(model)
                if replacement is not None and replacement != model:
                    embedder["model"] = replacement
                    changes.append(f"embedder.model: renamed `{model}` to `{replacement}`")

    return migrated, changes


# ---------------------------------------------------------------------------
# Section models
# ---------------------------------------------------------------------------

# A single ConfigDict reused by every section: forbid unknown keys (typos
# should fail loudly), enforce assignment-time validation, and freeze instances
# so the loaded ``Config`` can be safely shared across threads.
_SECTION_MODEL_CONFIG: Final[ConfigDict] = ConfigDict(
    extra="forbid",
    validate_assignment=True,
    frozen=True,
)


class RepoConfig(BaseModel):
    """``repo:`` section. Defines repo root and ignore patterns."""

    model_config = _SECTION_MODEL_CONFIG

    root: str = "."
    ignore: list[str] = Field(default_factory=lambda: list(DEFAULT_REPO_IGNORE))


class LanguagesConfig(BaseModel):
    """``languages:`` section. MVP supports TS/Python/Go (REQ-IDX-1)."""

    model_config = _SECTION_MODEL_CONFIG

    enabled: list[str] = Field(default_factory=lambda: ["typescript", "python", "go"])


class EmbedderConfig(BaseModel):
    """``embedder:`` section. Default local bge-small with 384-dim pin."""

    model_config = _SECTION_MODEL_CONFIG

    backend: EmbedderBackend = "local"
    model: str = "BAAI/bge-small-en-v1.5"
    dim: int = Field(default=384, ge=1, le=8192)
    batch_size: int = Field(default=32, ge=1, le=4096)


class RerankerConfig(BaseModel):
    """``reranker:`` section. Phase 2 feature; disabled at MVP."""

    model_config = _SECTION_MODEL_CONFIG

    backend: RerankerBackend = "local"
    model: str = "bge-reranker-v2-m3"
    enabled: bool = False


class GraphConfig(BaseModel):
    """``graph:`` section. Edge-resolution strategy and traversal cap."""

    model_config = _SECTION_MODEL_CONFIG

    edge_resolver: EdgeResolver = "lsp_then_heuristic"
    # Hard cap 8 per design "Hard limits"; default 5 per design "Configuration".
    max_depth: int = Field(default=5, ge=1, le=8)


class McpConfig(BaseModel):
    """``mcp:`` section. Transport list and tool allowlist."""

    model_config = _SECTION_MODEL_CONFIG

    transport: list[McpTransport] = Field(
        default_factory=lambda: list[McpTransport](("stdio",)),
    )
    sse_port: int = Field(default=7464, ge=1, le=65535)
    allow_tools: list[McpToolName] = Field(
        default_factory=lambda: list[McpToolName](DEFAULT_MCP_ALLOW_TOOLS),
    )

    @field_validator("transport")
    @classmethod
    def _transport_non_empty(cls, value: list[McpTransport]) -> list[McpTransport]:
        if not value:
            raise ValueError("mcp.transport must contain at least one transport")
        return value


class PlannerConfig(BaseModel):
    """``planner:`` section. Token budget cap and classifier strategy."""

    model_config = _SECTION_MODEL_CONFIG

    # Hard cap 32k per design "Hard limits"; default 8k per design "Configuration".
    default_max_tokens: int = Field(default=8000, ge=1, le=32000)
    classifier: PlannerClassifier = "rule_based"


class SecurityConfig(BaseModel):
    """``security:`` section. Redaction toggles and audit-log path."""

    model_config = _SECTION_MODEL_CONFIG

    redact_secrets: bool = True
    taint_untrusted: bool = True
    audit_log: str = ".cognis/audit.log"


class EvalConfig(BaseModel):
    """``eval:`` section. Path to the golden query set."""

    model_config = _SECTION_MODEL_CONFIG

    golden_set: str = ".cognis/eval/golden.jsonl"


# ---------------------------------------------------------------------------
# Top-level Config
# ---------------------------------------------------------------------------


class Config(BaseModel):
    """Top-level cognis config. All sections have sensible defaults."""

    model_config = ConfigDict(
        extra="forbid",
        validate_assignment=True,
        frozen=True,
    )

    repo: RepoConfig = Field(default_factory=RepoConfig)
    languages: LanguagesConfig = Field(default_factory=LanguagesConfig)
    embedder: EmbedderConfig = Field(default_factory=EmbedderConfig)
    reranker: RerankerConfig = Field(default_factory=RerankerConfig)
    graph: GraphConfig = Field(default_factory=GraphConfig)
    mcp: McpConfig = Field(default_factory=McpConfig)
    planner: PlannerConfig = Field(default_factory=PlannerConfig)
    security: SecurityConfig = Field(default_factory=SecurityConfig)
    eval: EvalConfig = Field(default_factory=EvalConfig)

    # ------------------------------------------------------------------
    # Loaders
    # ------------------------------------------------------------------

    @classmethod
    def default(cls) -> Config:
        """Return a ``Config`` populated entirely from baked-in defaults."""
        return cls()

    @classmethod
    def load(cls, repo_root: str | Path = ".") -> Config:
        """Load ``<repo_root>/.cognis/config.yaml`` or return defaults if absent.

        Missing config file is **not** an error: per requirements NFR
        Portability, ``cognis`` must boot on a fresh repo with zero setup.
        Task 2.2's ``cognis-cli init`` materializes ``.cognis/`` on demand.
        """
        path = Path(repo_root) / CONFIG_DIR_NAME / CONFIG_FILE_NAME
        if not path.exists():
            return cls.default()
        raw = _parse_yaml_mapping(path.read_text(encoding="utf-8"))
        migrated_raw, _changes = _migrate_raw_config(
            raw,
            read_config_revision(path.parent),
        )
        return cls.model_validate(migrated_raw)

    @classmethod
    def from_yaml(cls, path: str | Path) -> Config:
        """Load and validate a config from a YAML file path."""
        data = Path(path).read_text(encoding="utf-8")
        return cls.from_yaml_str(data)

    @classmethod
    def from_yaml_str(cls, text: str) -> Config:
        """Load and validate a config from a YAML string."""
        return cls.model_validate(_parse_yaml_mapping(text))

    # ------------------------------------------------------------------
    # Serializers
    # ------------------------------------------------------------------

    def to_dict(self) -> dict[str, Any]:
        """Return a plain-dict representation suitable for YAML serialization.

        ``mode='json'`` ensures every leaf is a YAML-safe primitive (str, int,
        float, bool, list, dict, None) so :func:`yaml.safe_dump` never falls
        back to Python-specific tags.
        """
        return self.model_dump(mode="json")

    def to_yaml(self) -> str:
        """Serialize to a YAML document. Round-trip safe with :meth:`from_yaml_str`."""
        return yaml.safe_dump(
            self.to_dict(),
            sort_keys=False,
            default_flow_style=False,
            allow_unicode=True,
        )

    def write(self, path: str | Path) -> Path:
        """Write the config to ``path`` (creating parent dirs). Returns the path."""
        target = Path(path)
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(self.to_yaml(), encoding="utf-8")
        return target


def detect_config_drift(repo_root: str | Path = ".") -> list[str]:
    """Return pending additive migration steps for the on-disk config."""
    cfg_path = Path(repo_root) / CONFIG_DIR_NAME / CONFIG_FILE_NAME
    if not cfg_path.exists():
        return []
    raw = _parse_yaml_mapping(cfg_path.read_text(encoding="utf-8"))
    revision = read_config_revision(cfg_path.parent)
    _, changes = _migrate_raw_config(raw, revision)
    return changes


def migrate_config_file(repo_root: str | Path = ".") -> ConfigMigrationReport:
    """Apply additive config migrations to ``<repo_root>/.cognis/config.yaml``."""
    repo_root = Path(repo_root)
    cfg_path = repo_root / CONFIG_DIR_NAME / CONFIG_FILE_NAME
    if not cfg_path.exists():
        return ConfigMigrationReport(0, 0, tuple(), False)

    raw = _parse_yaml_mapping(cfg_path.read_text(encoding="utf-8"))
    revision_from = read_config_revision(cfg_path.parent)
    migrated_raw, changes = _migrate_raw_config(raw, revision_from)
    Config.model_validate(migrated_raw)

    wrote_config = False
    if changes:
        cfg_path.write_text(
            yaml.safe_dump(
                migrated_raw,
                sort_keys=False,
                default_flow_style=False,
                allow_unicode=True,
            ),
            encoding="utf-8",
        )
        wrote_config = True

    revision_to = revision_from
    if revision_from < CONFIG_REVISION:
        write_config_revision(cfg_path.parent, CONFIG_REVISION)
        revision_to = CONFIG_REVISION

    return ConfigMigrationReport(
        revision_from=revision_from,
        revision_to=revision_to,
        changes=tuple(changes),
        wrote_config=wrote_config,
    )


__all__ = [
    "CONFIG_DIR_NAME",
    "CONFIG_FILE_NAME",
    "CONFIG_REVISION",
    "CONFIG_REVISION_FILE_NAME",
    "DEFAULT_MCP_ALLOW_TOOLS",
    "DEFAULT_REPO_IGNORE",
    "Config",
    "ConfigMigrationReport",
    "EdgeResolver",
    "EmbedderBackend",
    "EmbedderConfig",
    "EvalConfig",
    "GraphConfig",
    "LanguagesConfig",
    "McpConfig",
    "McpToolName",
    "McpTransport",
    "PlannerClassifier",
    "PlannerConfig",
    "RepoConfig",
    "RerankerBackend",
    "RerankerConfig",
    "SecurityConfig",
    "detect_config_drift",
    "migrate_config_file",
    "read_config_revision",
    "write_config_revision",
]
