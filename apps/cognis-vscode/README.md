<p align="center">
  <img src="https://raw.githubusercontent.com/buimanhtoan-it/cognis/main/assets/logo.png" alt="Cognis logo" width="96" />
</p>

<h1 align="center">Cognis for VS Code &amp; Cursor</h1>

<p align="center">
  Local, private, AI semantic code search for your MCP agent — powered by
  <b>CSAR</b> graph-diffusion retrieval.
</p>

---

Cognis indexes your repository locally and exposes it to AI coding agents
(Claude, Cursor, Copilot Chat, any MCP client) as a set of precise retrieval
tools. Instead of dumping raw files into the model, it returns the *right*
symbols, call chains, and task-focused context — and it recovers the **full
flow** of code that plain embedding search misses.

Everything runs on your machine. Your code is never uploaded anywhere.

> **This extension is the control panel.** The actual indexing/search engine is
> a small Python backend — the extension installs and manages it for you in one
> click. All you may need is Python 3.11+ on your machine.

---

## Setup (do this once)

The only thing you may need is **Python 3.11+** on your machine. Everything else
is one click.

### Step 1 — Make sure Python 3.11+ is available

```bash
python --version
```

If it's missing or below 3.11, install it from
[python.org](https://www.python.org/downloads/) (on Windows, keep the default
"Add Python to PATH" option checked). Cognis will find it automatically.

### Step 2 — Install the backend (one click)

Open the **Cognis sidebar panel** and click **Install backend**. Cognis
downloads the prebuilt, self-contained `cognis` engine binary for your platform
from the GitHub Release, checksum-verifies it, and stores it privately — no
terminal, no `pip`, no Python, no compiler. When it finishes the panel advances
on its own.

### Step 3 — Set up the workspace

Click **Set Up Workspace** in the panel (Cognis also offers this right after the
backend installs), or run **Cognis: Set Up Workspace** from the Command Palette
(`Ctrl/Cmd+Shift+P`).

This single action:
- creates the workspace config under `.cognis/`,
- writes the MCP configuration for your editor,
- starts indexing your code in the background,
- and reports health when done.

Cognis does **not** create `.cognis/` until you explicitly start setup — opening
a folder never writes anything. After setup, in a git repo, it automatically
adds `.cognis/` to your `.gitignore` (it holds the local index DB, caches, and
audit log — files you shouldn't commit) and tells you it did.

**Reload your editor / MCP host** once when prompted so the Cognis tools appear
in your AI chat. You're done.

---

## Using it

Once setup finishes, your AI agent gains these tools automatically. Just ask it
to work on your code — it will call them as needed. The most important one:

| Tool | Use it for |
| --- | --- |
| **`diffuse_context`** | **Flagship.** "Understand / trace this flow." Returns the relevant region *and its call flow* in one shot. |
| `discover_symbols` | Find candidates by name or meaning (hybrid search). |
| `semantic_search` | Concept/intent search over embeddings. |
| `symbol_lookup` / `symbol_search` | Resolve or list symbols by name. |
| `dependency_trace` | Walk callers/callees from a known symbol. |
| `retrieve_context_capsule` | One-call task context (bugfix / feature / explain). |

You don't call these by hand — your agent does. You just chat normally
("why does login time out?", "add pagination to /users") and Cognis feeds it
the right context.

The **Cognis sidebar panel** shows live indexing status: what's queued, what's
indexing now, and overall health.

---

## Connect MCP (write mcp.json)

Already indexed but want to (re)wire your editor, or connect a second MCP
client to the same workspace? Use **Connect MCP**:

- Click **Connect MCP** in the panel when the index is built but MCP isn't
  wired yet, or
- run **Cognis: Connect MCP (write mcp.json)** from the Command Palette.

Cognis writes the real workspace `mcp.json` for your detected editor and **opens
it** so you can see exactly what changed, then offers a one-click **Reload
Window**. For wiring a client Cognis didn't write to (a custom MCP host), the
reference guide format — collected environment, the exact `mcpServers` JSON, and
per-host reload steps — is still available.

---

## Pause &amp; resume index sync

By default Cognis **auto-syncs**: it indexes file changes as you save. To stop
that temporarily (e.g. during a huge rebase, or to free CPU):

- Click **Pause sync** in the panel's *Index Status* section, or
- run **Cognis: Pause Index Sync** from the Command Palette.

While paused, Cognis keeps answering AI queries from the last-synced index but
stops tracking new changes — and it won't auto-restart on reload or file save.
Click **Resume sync** (or run **Cognis: Resume Index Sync**) to turn auto-sync
back on. The default is always-on auto-sync.

---

## Rebuild Index

Index looking stale or wrong (after a big branch switch, an upgrade, or a
corrupted database)? Reset it:

- Click **Rebuild index** in the sidebar panel's *Index Status* section, or
- run **Cognis: Rebuild Index** from the Command Palette.

It stops indexing, deletes the stored index (database, sidecars, capsule cache),
and rebuilds from scratch. **Your config and MCP wiring are kept.** A
confirmation prompt appears first because a full rebuild can take a few minutes
on large repos.

---

## Removing Cognis

Cognis writes to three places: the local `.cognis/` index inside each repo, your
editor's MCP config (global `~/.cursor/mcp.json` by default, shared across
repos), and the Python backend it installed for you. The panel's **Danger zone**
(bottom of the sidebar) cleans these up — no terminal needed:

- **Remove from this workspace** — stops indexing, removes *this repo's* MCP
  entry, and deletes this repo's `.cognis/`. Other indexed repos keep working.
  Command: **Cognis: Remove from Workspace**.
- **Remove everything (prepare to uninstall)** — does the above, strips **every**
  `cognis-*` server from your MCP config, *and* uninstalls the engine binary and
  semantic model Cognis installed. After this you can uninstall the extension
  with nothing left behind.
  Command: **Cognis: Remove Everything (Prepare for Uninstall)**.

Both leave your **source code** untouched.

---

## Troubleshooting

If anything looks off, run **Cognis: Troubleshoot & Repair** (or the
**Troubleshoot** button in the panel). It re-checks the engine, config, MCP
wiring, and indexing, then tells you the next step.

| Symptom | Fix |
| --- | --- |
| "Install the Cognis backend" | Click **Install backend** in the panel — Cognis sets it up automatically. |
| "Cognis backend not ready" | Click **Install backend** (or **Reinstall backend**) in the panel. |
| AI tools don't appear in chat | Reload your editor / MCP host. If still missing, run **Troubleshoot & Repair**. |
| Indexing or config errors | Run **Troubleshoot & Repair**; open **Cognis: Show Output** for details. |
| Degraded health | Open **Cognis: Show Health**, then **Troubleshoot & Repair**. |
| Filing a bug report | Run **Cognis: Show Diagnostics Log** — a structured JSON trace of every flow, command, and backend call (with timings) you can attach. Set `cognis.logLevel` to `debug` for more detail. |

Full logs are always in **Cognis: Show Output**, the structured **Cognis: Show
Diagnostics Log**, and the health report.

---

## Settings

| Setting | Default | Description |
| --- | --- | --- |
| `cognis.autoManageOnActivate` | `true` | Inspect and repair the workspace on activation. |
| `cognis.autoStartLiveIndexing` | `true` | Start live indexing during auto-manage. |
| `cognis.autoIndexOnFileChange` | `true` | Re-index automatically when you save files. |
| `cognis.promptBeforeMcpWrite` | `true` | Confirm before writing MCP config during auto-manage. |
| `cognis.mcpHost` | `auto` | Target host for generated MCP config (`auto`, `cursor`, `vscode`, `claude`). |
| `cognis.pollHealthSeconds` | `30` | Health refresh interval while indexing runs. |
| `cognis.mcpSoftTimeoutSeconds` | `0` | Override `COGNIS_MCP_SOFT_TIMEOUT_S`; `0` keeps defaults. |
| `cognis.mcpHardTimeoutSeconds` | `0` | Override `COGNIS_MCP_HARD_TIMEOUT_S`; `0` keeps defaults. |
| `cognis.mcpDiscoverSemanticTimeoutSeconds` | `0` | Override `COGNIS_MCP_DISCOVER_SEMANTIC_TIMEOUT_S`; `0` keeps defaults. |
| `cognis.mcpSemanticCooldownSeconds` | `0` | Override `COGNIS_MCP_SEMANTIC_COOLDOWN_S`; `0` keeps defaults. |
| `cognis.logLevel` | `info` | Verbosity of the diagnostics log (**Cognis: Show Diagnostics Log**). Use `debug` to capture every CLI call + command with timings when filing an issue. |

On Windows, generated MCP config uses a safer automatic timeout budget for the
first semantic query unless you override these explicitly.

---

## Privacy &amp; security

- **100% local.** Indexing, embeddings, and search run on your machine. No code
  leaves your computer.
- **Secrets scrubbed.** API keys, JWTs, PEM headers, and `password=` patterns
  are redacted *before* indexing — originals are never stored.
- **Untrusted content tagged.** Comments and docstrings are marked untrusted
  before reaching the model.
- Every MCP tool call is logged locally to `.cognis/audit.log` (hashed args).

---

## Requirements

- VS Code 1.85+ or Cursor (any MCP-capable editor).
- Python 3.11+ available on your machine (the backend installs in one click).
- Languages indexed today: **TypeScript / JavaScript, Python, Go, C#, Java**.

---

## Links

- **Source, docs & issues:** [github.com/buimanhtoan-it/cognis](https://github.com/buimanhtoan-it/cognis)
- **How CSAR works (the math):** [docs/csar.md](https://github.com/buimanhtoan-it/cognis/blob/main/docs/csar.md)
- **MCP client setup:** [docs/mcp-client-config.md](https://github.com/buimanhtoan-it/cognis/blob/main/docs/mcp-client-config.md)
- **License:** Commercial — see [LICENSE.txt](LICENSE.txt)

---

<p align="center"><sub>Built for developers who want their AI agent to actually understand the codebase.</sub></p>
