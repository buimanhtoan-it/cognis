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
> a small Python backend you install once (one command). Follow the **Setup**
> below — the extension cannot work without the backend.

---

## Setup (do this once)

You need **two** things installed: this extension (done ✅) and the Cognis
Python backend. Total time: ~3 minutes.

### Step 1 — Install Python 3.11 or newer

Check what you have:

```bash
python --version
```

If it's below 3.11, install it from [python.org](https://www.python.org/downloads/)
(on Windows, keep the default "Add Python to PATH" option checked).

### Step 2 — Install the Cognis backend

Open a terminal and create an isolated environment so Cognis doesn't touch your
system Python:

```bash
# 1. create a virtual environment
python -m venv ~/.cognis-venv

# 2. activate it
#    macOS / Linux:
source ~/.cognis-venv/bin/activate
#    Windows PowerShell:
#    & "$HOME\.cognis-venv\Scripts\Activate.ps1"

# 3. install Cognis with all features
pip install "cognis[indexer,embed-local,vector,tokenizers,mcp]"
```

That's the whole backend. Verify it:

```bash
cognis-cli --version
```

> Prefer Docker or installing from source? See the
> [full install guide](https://github.com/buimanhtoan-it/cognis#quick-start).

### Step 3 — Point the extension at that Python

The extension runs the backend through a specific Python interpreter. Tell it
which one:

1. Open the folder/repo you want to index in VS Code or Cursor.
2. Open **Settings** → search `cognis.pythonPath`.
3. Set it to the interpreter from Step 2:
   - macOS / Linux: `~/.cognis-venv/bin/python`
   - Windows: `%USERPROFILE%\.cognis-venv\Scripts\python.exe`

(If Cognis lives in your selected workspace interpreter already, you can leave
this blank.)

### Step 4 — Set up the workspace

Run **Cognis: Set Up for AI** from the Command Palette
(`Ctrl/Cmd+Shift+P`), or click **Set Up for AI** in the Cognis sidebar panel.

This single action:
- creates the workspace config under `.cognis/`,
- writes the MCP configuration for your editor,
- starts indexing your code in the background,
- and reports health when done.

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

## Clear & Re-index

Index looking stale or wrong (after a big branch switch, an upgrade, or a
corrupted database)? Reset it:

- Click **Clear & Re-index** in the sidebar panel's *Index Status* section, or
- run **Cognis: Clear Index & Re-index** from the Command Palette.

It stops indexing, deletes the stored index (database, sidecars, capsule cache),
and rebuilds from scratch. **Your config and MCP wiring are kept.** A
confirmation prompt appears first because a full rebuild can take a few minutes
on large repos.

---

## Troubleshooting

If anything looks off, run **Cognis: Repair Setup** (or the **Repair Setup**
button in the panel). It re-checks Python, config, MCP wiring, and indexing,
then tells you the next step.

| Symptom | Fix |
| --- | --- |
| "Python / cognis backend not found" | Re-check Step 2, then set `cognis.pythonPath` (Step 3) and run **Repair Setup**. |
| AI tools don't appear in chat | Reload your editor / MCP host. If still missing, run **Repair Setup**. |
| Indexing or config errors | Run **Repair Setup**; open **Cognis: Show Output** for details. |
| Degraded health | Open **Cognis: Show Health**, then **Repair Setup**. |

Full logs are always in **Cognis: Show Output** and the health report.

---

## Settings

| Setting | Default | Description |
| --- | --- | --- |
| `cognis.pythonPath` | `""` | Python executable for the backend. Set this to your Cognis venv if it differs from the workspace interpreter. |
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
- Python 3.11+ with the Cognis backend (Step 2).
- Languages indexed today: **TypeScript / JavaScript, Python, Go**.

---

## Links

- **Source, docs & issues:** [github.com/buimanhtoan-it/cognis](https://github.com/buimanhtoan-it/cognis)
- **How CSAR works (the math):** [docs/csar.md](https://github.com/buimanhtoan-it/cognis/blob/main/docs/csar.md)
- **MCP client setup:** [docs/mcp-client-config.md](https://github.com/buimanhtoan-it/cognis/blob/main/docs/mcp-client-config.md)
- **License:** Apache-2.0

---

<p align="center"><sub>Built for developers who want their AI agent to actually understand the codebase.</sub></p>
