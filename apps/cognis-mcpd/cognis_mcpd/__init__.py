"""``cognis-mcpd`` FastMCP server package.

Exposes MCP tools via a FastMCP stdio server:
- diffuse_context — CSAR spreading-activation retrieval (flagship; recovers
  full code flow in one round trip)
- discover_symbols — hybrid lexical + semantic discovery (RRF)
- symbol_search — ranked multi-result symbol discovery (lexical)
- symbol_lookup — resolve a single known symbol
- semantic_search — concept/intent search with enriched payloads
- resolve_symbols — batch hydrate up to 50 symbols
- dependency_trace — traverse callers or callees on the call graph
- retrieve_context_capsule — task-oriented context package (CSAR-powered)

See :mod:`cognis_mcpd.server` for server setup and :mod:`cognis_mcpd.tools`
for tool implementations.
"""
