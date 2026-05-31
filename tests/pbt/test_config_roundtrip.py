"""Property test: ``Config`` YAML round-trip preserves all fields (task 2.3).

**Validates: Requirements REQ-IDX-2** (incremental update reads config from
disk between processes, so on-disk YAML must reload with zero semantic drift)
and the *NFR Portability* clause (cognis must boot identically on every
platform from the same ``.cognis/config.yaml``).

Numbering aligns with design.md ``Correctness Properties``: this is the
*configuration* preamble to **CP-9** (capsule schema and source completeness),
which governs the same serialize/parse discipline for capsules. The capsule
machinery isn't built yet, but the round-trip invariant on ``Config`` is the
pattern we'll reuse there, hence "CP-9 prep" in tasks.md.

Property:

::

    ∀ valid Config cfg, Config.from_yaml_str(cfg.to_yaml()) == cfg
"""

from __future__ import annotations

import pytest
from cognis.config import (
    Config,
    EmbedderConfig,
    EvalConfig,
    GraphConfig,
    LanguagesConfig,
    McpConfig,
    PlannerConfig,
    RepoConfig,
    RerankerConfig,
    SecurityConfig,
)
from hypothesis import given
from hypothesis import strategies as st

# ---------------------------------------------------------------------------
# Primitive strategies
# ---------------------------------------------------------------------------

# Printable ASCII (0x20..0x7E) keeps the generator focused on the round-trip
# property itself and deliberately stresses PyYAML's quoting of YAML-special
# tokens — ``yes``/``no``/``on``/``off`` (booleans in YAML 1.1), bare ``1``
# (int), ``~``/``null`` (null), and the structural sigils ``:``, ``#``, ``|``,
# ``>``, ``[``, ``]``, ``{``, ``}``, ``&``, ``*``, ``!``, ``%``, ``@`` — all
# of which must survive a serialize/parse cycle as plain strings. Control
# chars and Unicode aren't excluded for safety; they're excluded because they
# don't add coverage for "no semantic drift".
_TEXT: st.SearchStrategy[str] = st.text(
    alphabet=st.characters(min_codepoint=0x20, max_codepoint=0x7E),
    max_size=30,
)
_TEXT_LIST: st.SearchStrategy[list[str]] = st.lists(_TEXT, max_size=6)


# ---------------------------------------------------------------------------
# Section strategies — each clamped to the Pydantic field constraints
# (Literal enums, ``ge``/``le`` bounds, non-empty list validators).
# ---------------------------------------------------------------------------

_REPO = st.builds(
    RepoConfig,
    root=_TEXT,
    ignore=_TEXT_LIST,
)

_LANGUAGES = st.builds(
    LanguagesConfig,
    enabled=_TEXT_LIST,
)

_EMBEDDER = st.builds(
    EmbedderConfig,
    backend=st.sampled_from(["local", "voyage", "openai"]),
    model=_TEXT,
    dim=st.integers(min_value=1, max_value=8192),
    batch_size=st.integers(min_value=1, max_value=4096),
)

_RERANKER = st.builds(
    RerankerConfig,
    backend=st.sampled_from(["local"]),
    model=_TEXT,
    enabled=st.booleans(),
)

_GRAPH = st.builds(
    GraphConfig,
    edge_resolver=st.sampled_from(
        ["lsp_only", "heuristic_only", "lsp_then_heuristic"],
    ),
    max_depth=st.integers(min_value=1, max_value=8),
)

_MCP = st.builds(
    McpConfig,
    # ``mcp.transport`` has a non-empty validator (see McpConfig._transport_non_empty).
    transport=st.lists(
        st.sampled_from(["stdio", "sse"]),
        min_size=1,
        max_size=2,
    ),
    sse_port=st.integers(min_value=1, max_value=65535),
    allow_tools=st.lists(
        st.sampled_from(
            [
                "diffuse_context",
                "symbol_lookup",
                "symbol_search",
                "discover_symbols",
                "semantic_search",
                "resolve_symbols",
                "dependency_trace",
                "retrieve_context_capsule",
            ],
        ),
        max_size=8,
    ),
)

_PLANNER = st.builds(
    PlannerConfig,
    default_max_tokens=st.integers(min_value=1, max_value=32000),
    classifier=st.sampled_from(["rule_based", "small_lm"]),
)

_SECURITY = st.builds(
    SecurityConfig,
    redact_secrets=st.booleans(),
    taint_untrusted=st.booleans(),
    audit_log=_TEXT,
)

_EVAL = st.builds(
    EvalConfig,
    golden_set=_TEXT,
)


# ---------------------------------------------------------------------------
# Top-level Config strategy
# ---------------------------------------------------------------------------

_CONFIG: st.SearchStrategy[Config] = st.builds(
    Config,
    repo=_REPO,
    languages=_LANGUAGES,
    embedder=_EMBEDDER,
    reranker=_RERANKER,
    graph=_GRAPH,
    mcp=_MCP,
    planner=_PLANNER,
    security=_SECURITY,
    eval=_EVAL,
)


# ---------------------------------------------------------------------------
# Property
# ---------------------------------------------------------------------------


@pytest.mark.pbt
@given(cfg=_CONFIG)
def test_config_yaml_roundtrip_preserves_all_fields(cfg: Config) -> None:
    """**Validates: Requirements REQ-IDX-2** and NFR Portability (CP-9 prep).

    Any value the Pydantic schema accepts must survive a ``to_yaml`` →
    ``from_yaml_str`` cycle at the semantic level (structural equality, not
    literal YAML text).
    """
    serialized = cfg.to_yaml()
    restored = Config.from_yaml_str(serialized)

    # Primary invariant: semantic equality across one full round trip.
    assert restored == cfg

    # Stability: a second serialize of the restored object reproduces the
    # same YAML bytes. Catches asymmetric drift where the first dump's text
    # differs from the canonical form (e.g. quoting that flips on the
    # second pass), which would otherwise hide behind ``Config`` equality.
    assert restored.to_yaml() == serialized
