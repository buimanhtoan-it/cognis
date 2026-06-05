# Release Notes — cognis v0.3.0

> **Zero-friction onboarding release.** First-time setup in the VS Code / Cursor
> extension is now one click end to end: Cognis installs and manages its own
> Python backend, guides you through a fixed setup path, keeps `.cognis/` out of
> git automatically, and can fully remove itself when you're done.

## Highlights

### One-click backend install — no terminal, no `pip`

Click **Install backend** in the Cognis panel and the extension creates a
private Python environment it manages for you and installs the backend into it.
No terminal, no `pip` commands, no choosing a Python environment. When it
finishes, Cognis offers to set up the workspace immediately.

If you prefer your own environment, set `cognis.pythonPath` and Cognis installs
the `cognis` package into that environment instead of creating a managed one.

### One-click, safe removal

The panel's **Danger zone** removes Cognis without touching a terminal:

- **Remove from this workspace** — stops indexing, disconnects this repo's MCP
  entry, and deletes this repo's `.cognis/`.
- **Remove everything (prepare to uninstall)** — also strips every `cognis-*`
  server from your editor's MCP config (all repos) *and* uninstalls the managed
  backend, so nothing is left orphaned after you uninstall the extension.

Removal never touches your source code. With a bring-your-own Python, it only
uninstalls the `cognis` package — your environment is kept.

### A clear setup path

The panel shows a fixed 4-step stepper — **Backend → Components → Index synced →
AI connected** — so you always see where you are and the single next action. A
fresh machine gets an explicit "Install the Cognis backend" state instead of a
setup button that fails with an import error. The status bar uses a short,
stable vocabulary (Indexing / Ready / Action needed / Not set up).

### `.gitignore` handled for you

After setup, in a git repository, Cognis automatically adds `.cognis/` to your
`.gitignore` (idempotent) and shows a non-blocking notice. The `.cognis/`
directory holds the local index database, capsule cache, and audit log —
machine-specific files that should never be committed.

### Plainer wording

User-facing copy no longer uses the term "interpreter." **Repair Setup** is now
**Troubleshoot & Repair** and **Clear Index & Re-index** is now **Rebuild
Index** (command IDs are unchanged, so existing keybindings still work).

## Carried forward from v0.2.1

The v0.2.1 prerequisite checklist, no-surprise `.cognis/` creation, and the
`cognis-cli doctor --json` command remain in place, along with the v0.2.0
reliability fixes for MCP stdio startup and `cognis-indexd` status writes. See
[release-notes-v0.2.1.md](release-notes-v0.2.1.md).

## Getting started

In the editor: open the Cognis panel, click **Install backend**, then **Set Up
for AI**. That's it.

Prefer the CLI / your own environment?

```bash
pip install cognis-engine[indexer,embed-local,vector,tokenizers,mcp]
cd /your/repo
cognis-cli init
cognis-cli index --full .
cognis-cli health
cognis-mcpd  # or configure via docs/mcp-client-config.md
```

## Changelog

See [CHANGELOG.md](../CHANGELOG.md) for the full change history.

## License

Apache-2.0. See `LICENSE`.
