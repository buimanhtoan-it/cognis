"""Unit tests for ``cognis.config`` (task 2.1).

Covers:

- Defaults match the design.md *Configuration* section.
- ``Config.load`` returns defaults when ``.cognis/config.yaml`` is absent.
- Round-trip ``to_yaml`` → ``from_yaml_str`` preserves all values.
- Validation rejects unknown keys, out-of-range ints, and bad enum values.
- Frozen models reject in-place mutation (config is shared across threads).

The CP-9 round-trip property test lives in ``tests/pbt/`` (task 2.3).
"""

from __future__ import annotations

from pathlib import Path

import pytest
import yaml
from cognis.config import (
    CONFIG_DIR_NAME,
    CONFIG_FILE_NAME,
    CONFIG_REVISION,
    Config,
    EmbedderConfig,
    GraphConfig,
    McpConfig,
    PlannerConfig,
    migrate_config_file,
    read_config_revision,
    write_config_revision,
)
from pydantic import ValidationError

# ---------------------------------------------------------------------------
# Defaults
# ---------------------------------------------------------------------------


@pytest.mark.unit
def test_default_config_matches_design_section() -> None:
    """``Config()`` equals the design.md Configuration block, key-for-key."""
    cfg = Config()

    assert cfg.repo.root == "."
    assert cfg.repo.ignore == [
        "node_modules",
        ".git",
        "dist",
        "target",
        "__pycache__",
        ".venv",
        "reference",
    ]

    assert cfg.languages.enabled == ["typescript", "python", "go"]

    assert cfg.embedder.backend == "local"
    assert cfg.embedder.model == "BAAI/bge-small-en-v1.5"
    assert cfg.embedder.dim == 384  # MVP pin per design Q-2 resolution
    assert cfg.embedder.batch_size == 32

    assert cfg.reranker.backend == "local"
    assert cfg.reranker.model == "bge-reranker-v2-m3"
    assert cfg.reranker.enabled is False

    assert cfg.graph.edge_resolver == "lsp_then_heuristic"
    assert cfg.graph.max_depth == 5

    assert cfg.mcp.transport == ["stdio"]
    assert cfg.mcp.sse_port == 7464
    assert cfg.mcp.allow_tools == [
        "diffuse_context",
        "symbol_lookup",
        "symbol_search",
        "discover_symbols",
        "semantic_search",
        "resolve_symbols",
        "dependency_trace",
        "retrieve_context_capsule",
    ]

    assert cfg.planner.default_max_tokens == 8000
    assert cfg.planner.classifier == "rule_based"

    assert cfg.security.redact_secrets is True
    assert cfg.security.taint_untrusted is True
    assert cfg.security.audit_log == ".cognis/audit.log"

    assert cfg.eval.golden_set == ".cognis/eval/golden.jsonl"


@pytest.mark.unit
def test_default_classmethod_equivalent_to_constructor() -> None:
    assert Config.default() == Config()


# ---------------------------------------------------------------------------
# load() behavior
# ---------------------------------------------------------------------------


@pytest.mark.unit
def test_load_returns_defaults_when_no_file(tmp_path: Path) -> None:
    """A fresh repo with no ``.cognis/`` boots on baked-in defaults (NFR Portability)."""
    assert Config.load(tmp_path) == Config.default()


@pytest.mark.unit
def test_load_reads_yaml_from_default_location(tmp_path: Path) -> None:
    cognis_dir = tmp_path / CONFIG_DIR_NAME
    cognis_dir.mkdir()
    (cognis_dir / CONFIG_FILE_NAME).write_text(
        yaml.safe_dump(
            {
                "embedder": {"batch_size": 64},
                "planner": {"default_max_tokens": 4000},
            }
        ),
        encoding="utf-8",
    )

    cfg = Config.load(tmp_path)

    assert cfg.embedder.batch_size == 64
    assert cfg.planner.default_max_tokens == 4000
    # Untouched sections still defaulted.
    assert cfg.repo.root == "."
    assert cfg.mcp.allow_tools[0] == "diffuse_context"


@pytest.mark.unit
def test_load_migrates_legacy_defaults_in_memory(tmp_path: Path) -> None:
    cognis_dir = tmp_path / CONFIG_DIR_NAME
    cognis_dir.mkdir()
    (cognis_dir / CONFIG_FILE_NAME).write_text(
        yaml.safe_dump(
            {
                "repo": {"ignore": ["node_modules", ".git"]},
                "mcp": {"allow_tools": ["symbol_lookup", "semantic_search"]},
                "embedder": {"model": "bge-small-en-v1.5"},
            }
        ),
        encoding="utf-8",
    )

    cfg = Config.load(tmp_path)

    assert "reference" in cfg.repo.ignore
    assert "discover_symbols" in cfg.mcp.allow_tools
    assert "resolve_symbols" in cfg.mcp.allow_tools
    assert cfg.embedder.model == "BAAI/bge-small-en-v1.5"


@pytest.mark.unit
def test_load_honors_current_revision_user_overrides(tmp_path: Path) -> None:
    cognis_dir = tmp_path / CONFIG_DIR_NAME
    cognis_dir.mkdir()
    (cognis_dir / CONFIG_FILE_NAME).write_text(
        yaml.safe_dump(
            {
                "repo": {"ignore": ["custom-cache"]},
                "mcp": {"allow_tools": ["symbol_lookup"]},
            }
        ),
        encoding="utf-8",
    )
    write_config_revision(cognis_dir)

    cfg = Config.load(tmp_path)

    assert cfg.repo.ignore == ["custom-cache"]
    assert cfg.mcp.allow_tools == ["symbol_lookup"]


@pytest.mark.unit
def test_migrate_config_file_writes_additive_changes_and_revision(tmp_path: Path) -> None:
    cognis_dir = tmp_path / CONFIG_DIR_NAME
    cognis_dir.mkdir()
    cfg_path = cognis_dir / CONFIG_FILE_NAME
    cfg_path.write_text(
        yaml.safe_dump(
            {
                "repo": {"ignore": ["node_modules", ".git", "custom-cache"]},
                "mcp": {"allow_tools": ["symbol_lookup"]},
                "planner": {"default_max_tokens": 4321},
                "embedder": {"model": "bge-small-en-v1.5"},
            }
        ),
        encoding="utf-8",
    )

    report = migrate_config_file(tmp_path)

    assert report.wrote_config is True
    assert report.revision_from == 0
    assert report.revision_to == CONFIG_REVISION
    assert any("repo.ignore" in change for change in report.changes)
    assert any("mcp.allow_tools" in change for change in report.changes)
    assert read_config_revision(cognis_dir) == CONFIG_REVISION

    migrated = Config.from_yaml(cfg_path)
    assert migrated.planner.default_max_tokens == 4321
    assert "custom-cache" in migrated.repo.ignore
    assert "reference" in migrated.repo.ignore
    assert "discover_symbols" in migrated.mcp.allow_tools
    assert "resolve_symbols" in migrated.mcp.allow_tools
    assert migrated.embedder.model == "BAAI/bge-small-en-v1.5"


@pytest.mark.unit
def test_load_handles_empty_yaml(tmp_path: Path) -> None:
    cognis_dir = tmp_path / CONFIG_DIR_NAME
    cognis_dir.mkdir()
    (cognis_dir / CONFIG_FILE_NAME).write_text("", encoding="utf-8")

    assert Config.load(tmp_path) == Config.default()


@pytest.mark.unit
def test_from_yaml_str_rejects_non_mapping_root() -> None:
    with pytest.raises(TypeError):
        Config.from_yaml_str("- one\n- two\n")


# ---------------------------------------------------------------------------
# Round-trip
# ---------------------------------------------------------------------------


@pytest.mark.unit
def test_default_config_roundtrips_through_yaml() -> None:
    cfg = Config.default()
    restored = Config.from_yaml_str(cfg.to_yaml())
    assert restored == cfg


@pytest.mark.unit
def test_custom_config_roundtrips_through_yaml() -> None:
    cfg = Config(
        repo={"root": "src", "ignore": [".git", "build"]},  # type: ignore[arg-type]
        embedder={"backend": "voyage", "model": "voyage-code-3", "dim": 1024, "batch_size": 16},  # type: ignore[arg-type]
        mcp={  # type: ignore[arg-type]
            "transport": ["stdio", "sse"],
            "sse_port": 9000,
            "allow_tools": ["symbol_lookup"],
        },
    )
    restored = Config.from_yaml_str(cfg.to_yaml())
    assert restored == cfg


@pytest.mark.unit
def test_to_dict_returns_only_yaml_safe_primitives() -> None:
    """``to_dict`` must use ``model_dump(mode='json')`` so YAML never tags Python objects."""
    cfg = Config.default()
    dumped = cfg.to_dict()

    def _walk(value: object) -> None:
        if isinstance(value, dict):
            for k, v in value.items():
                assert isinstance(k, str)
                _walk(v)
        elif isinstance(value, list):
            for item in value:
                _walk(item)
        else:
            assert isinstance(value, (str, int, float, bool, type(None)))

    _walk(dumped)


@pytest.mark.unit
def test_write_creates_parent_dirs(tmp_path: Path) -> None:
    target = tmp_path / "nested" / "deep" / "config.yaml"
    Config.default().write(target)

    assert target.exists()
    assert Config.from_yaml(target) == Config.default()


# ---------------------------------------------------------------------------
# Validation
# ---------------------------------------------------------------------------


@pytest.mark.unit
def test_unknown_top_level_key_rejected() -> None:
    with pytest.raises(ValidationError):
        Config.from_yaml_str("unknown_section: {}\n")


@pytest.mark.unit
def test_unknown_section_key_rejected() -> None:
    with pytest.raises(ValidationError):
        Config.from_yaml_str("embedder:\n  bogus: 1\n")


@pytest.mark.unit
def test_invalid_embedder_backend_rejected() -> None:
    with pytest.raises(ValidationError):
        EmbedderConfig(backend="cohere")  # type: ignore[arg-type]


@pytest.mark.unit
def test_invalid_edge_resolver_rejected() -> None:
    with pytest.raises(ValidationError):
        GraphConfig(edge_resolver="ml_only")  # type: ignore[arg-type]


@pytest.mark.unit
@pytest.mark.parametrize("bad_depth", [0, -1, 9, 100])
def test_graph_max_depth_bounds(bad_depth: int) -> None:
    with pytest.raises(ValidationError):
        GraphConfig(max_depth=bad_depth)


@pytest.mark.unit
@pytest.mark.parametrize("bad_tokens", [0, -1, 32001])
def test_planner_max_tokens_bounds(bad_tokens: int) -> None:
    with pytest.raises(ValidationError):
        PlannerConfig(default_max_tokens=bad_tokens)


@pytest.mark.unit
def test_mcp_transport_must_be_non_empty() -> None:
    with pytest.raises(ValidationError):
        McpConfig(transport=[])


@pytest.mark.unit
def test_mcp_allow_tools_must_be_known_names() -> None:
    with pytest.raises(ValidationError):
        McpConfig(allow_tools=["read_files"])  # type: ignore[list-item]


# ---------------------------------------------------------------------------
# Immutability — Config is shared across daemons (mcpd, indexd, cli)
# ---------------------------------------------------------------------------


@pytest.mark.unit
def test_config_is_frozen() -> None:
    cfg = Config.default()
    with pytest.raises(ValidationError):
        cfg.repo.root = "/tmp"  # type: ignore[misc]
    with pytest.raises(ValidationError):
        cfg.embedder.dim = 768  # type: ignore[misc]
