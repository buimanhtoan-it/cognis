# Release Notes — cognis v0.2.1

> **Historical Python-era release.** The pip/prerequisite instructions below
> are archival. Follow [install.md](install.md) for the current pure-Rust product
> and Polar ZIP/source-build distribution.
>
> **Onboarding release.** Makes first-time setup in the VS Code / Cursor
> extension safe and self-guiding: a prerequisite checklist with one-click
> installs, no surprise `.cognis/` creation, and a `.gitignore` reminder.

## Highlights

### Prerequisite checklist in the extension panel

The Cognis sidebar now shows a **Prerequisites** checklist at the top. Each
backend component the extension needs — code parsers (tree-sitter), local
embeddings (sentence-transformers), vector search (sqlite-vec), the MCP server
(fastmcp), and tokenizers (tiktoken) — appears with an installed/missing marker.
Missing items get a per-item **Install** button (plus **Install all**) that runs
the correct `pip install` in a terminal; **Re-check** refreshes the state.

**Set Up for AI** is now blocked until the required components are installed, so
a fresh user can never end up with a half-provisioned workspace that can't index
or serve. Backed by a new `cognis-cli doctor --json` command that the extension
consumes.

### No surprise `.cognis/` creation

Opening a folder no longer writes anything. The extension only provisions
`.cognis/` (config, index DB, caches) when you explicitly run **Set Up for AI**.
Activation still auto-manages workspaces that are already configured, exactly as
before.

### `.gitignore` reminder

After setup, in a git repository, the extension offers to add `.cognis/` to your
`.gitignore` (with a "Don't ask again" option). The `.cognis/` directory holds
the local index database, capsule cache, and audit log — machine-specific files
that should never be committed.

## Carried forward from v0.2.0

The v0.2.0 reliability fixes remain in place: semantic search no longer hangs on
first use over MCP stdio, the MCP server warms heavy imports on the main thread,
and `cognis-indexd` writes its status file atomically and releases connections
cleanly. The cross-app end-to-end test suite (`tests/e2e/`) and contract
snapshots continue to guard against drift. See
[release-notes-v0.2.0.md](release-notes-v0.2.0.md).

## Getting started

```bash
pip install cognis[indexer,embed-local,tokenizers,mcp]
cd /your/repo
cognis-cli init
cognis-cli index --full .
cognis-cli health
cognis-mcpd  # or configure via docs/mcp-client-config.md
```

In the editor, open the Cognis panel, satisfy the prerequisite checklist, then
click **Set Up for AI**.

## Changelog

See [CHANGELOG.md](../CHANGELOG.md) for the full change history.

## License

Apache-2.0. See `LICENSE`.
