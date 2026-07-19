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

> **This extension is the control panel.** The indexing/search engine is one
> self-contained Rust `cognis` binary. No Python, `pip`, or virtual environment
> is required.

---

## Setup (do this once)

Choose one distribution path:

- **Prebuilt:** buy one versioned ZIP from Polar and install the `.vsix` inside
  it. Polar does not deliver a separate VSIX, license key, or activation.
- **Source:** clone the public repository and run
  `cargo build --release -p cognis --bin cognis --features onnx-download`, then
  package this extension with `npm install` and `npm run package`. Set
  `cognis.binaryPath` to the source-built `cognis` binary.

Both paths provide the same functionality under Apache-2.0. The Polar purchase
pays for the ready-to-install package and delivery, not feature access.

### Step 1 — Install the engine

With the Polar build, open the **Cognis sidebar panel** and click **Install
engine**. Cognis fetches the matching release-managed Rust binary and semantic
model, verifies their SHA-256 checksums, and stores them privately. With a
source build, set `cognis.binaryPath` to `target/release/cognis` (or
`cognis.exe`) and provide local model assets as described in
[`assets/models/README.md`](../../assets/models/README.md).

### Step 2 — Set up the workspace

Click **Set Up Workspace** in the panel (Cognis also offers this right after the
engine installs), or run **Cognis: Set Up Workspace** from the Command Palette
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

Cognis writes the real **workspace** `mcp.json` for your detected editor by
default (`cognis.mcpConfigScope = workspace`) and **opens it** so you can see
exactly what changed, then offers a one-click **Reload Window**. Workspace scope
avoids host × repository idle daemon fan-out from a global multi-repo config.
For wiring a client Cognis didn't write to (a custom MCP host), the reference
guide format — collected environment, the exact `mcpServers` JSON, and per-host
reload steps — is still available in the repo docs.


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
editor's MCP config (workspace `mcp.json` by default; global host config only if
you opt into `cognis.mcpConfigScope = global`), and the managed engine binary +
model it installed for you. The panel's **Danger zone** (bottom of the sidebar)
cleans these up — no terminal needed:

- **Remove from this workspace** — stops indexing, removes *this repo's* MCP
  entry, and deletes this repo's `.cognis/`. Other indexed repos keep working.
  Command: **Cognis: Remove from Workspace**.
- **Remove everything (prepare to uninstall)** — does the above, strips **every**
  `cognis-*` server from your MCP config, *and* uninstalls the engine binary and
  semantic model Cognis installed. After this you can uninstall the extension
  with nothing left behind.
  Command: **Cognis: Remove Everything (Prepare for Uninstall)**.

Both leave your **source code** untouched. Non-Cognis MCP servers are preserved.


---

## Troubleshooting

If anything looks off, run **Cognis: Troubleshoot & Repair** (or the
**Troubleshoot** button in the panel). It re-checks the engine, config, MCP
wiring, and indexing, then tells you the next step.

| Symptom | Fix |
| --- | --- |
| "Install the Cognis engine" | Click **Install engine** in the panel — Cognis sets it up automatically. |
| "Cognis engine not ready" | Click **Install engine** (or **Reinstall engine**) in the panel. |
| AI tools don't appear in chat | Reload your editor / MCP host. If still missing, run **Troubleshoot & Repair**. |
| Many idle Cognis/`mcpd` processes or high RAM | Keep `cognis.mcpConfigScope=workspace` and `cognis.mcpStdioMode=proxy`; migrate/remove stale global `cognis-*` entries only for closed repos. See [MCP client setup](https://github.com/buimanhtoan-it/cognis/blob/main/docs/mcp-client-config.md). |
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
| `cognis.mcpHost` | `auto` | Target host for generated MCP config (`auto`, `cursor`, `vscode`, `kiro`, `claude`). |
| `cognis.mcpConfigScope` | `workspace` | Write MCP config inside the repo (default) or opt in to global host config (fan-out risk). |
| `cognis.mcpStdioMode` | `proxy` | Thin model-free stdio proxy to one heavy daemon per repo (`heavy` = legacy per-connection daemon). |
| `cognis.mcpSharedHttpEnabled` | `false` | Reversible shared-HTTP gate (default OFF; failed checks keep thin-proxy stdio). |
| `cognis.mcpWarmSemanticOnStartup` | `false` | Generated config explicitly sets lazy ONNX loading (`COGNIS_MCP_WARM_SEMANTIC_ON_STARTUP=0`); enable for eager `=1`. Direct launches with the variable absent remain eager. |
| `cognis.pollHealthSeconds` | `30` | Health refresh interval while indexing runs. |
| `cognis.mcpSoftTimeoutSeconds` | `0` | Override `COGNIS_MCP_SOFT_TIMEOUT_S`; `0` keeps defaults. |
| `cognis.mcpHardTimeoutSeconds` | `0` | Override `COGNIS_MCP_HARD_TIMEOUT_S`; `0` keeps defaults. |
| `cognis.mcpDiscoverSemanticTimeoutSeconds` | `0` | Override `COGNIS_MCP_DISCOVER_SEMANTIC_TIMEOUT_S`; `0` keeps defaults. |
| `cognis.mcpSemanticCooldownSeconds` | `0` | Override `COGNIS_MCP_SEMANTIC_COOLDOWN_S`; `0` keeps defaults. |
| `cognis.logLevel` | `info` | Verbosity of the diagnostics log (**Cognis: Show Diagnostics Log**). Use `debug` to capture every CLI call + command with timings when filing an issue. |

On Windows, generated MCP config uses a safer automatic timeout budget for the
first semantic query unless you override these explicitly. Workspace scope +
thin proxy are the defaults that avoid idle host × repository process fan-out.
Details: [docs/mcp-client-config.md](https://github.com/buimanhtoan-it/cognis/blob/main/docs/mcp-client-config.md).


---

## Privacy &amp; security

- **100% local.** Indexing, embeddings, and search run on your machine. No code
  leaves your computer.
- **Secrets scrubbed.** API keys, JWTs, PEM headers, and `password=` patterns
  are redacted *before* indexing — originals are never stored.
- **Untrusted content tagged.** Comments and docstrings are marked untrusted
  before reaching the model.
- Every MCP tool call is logged locally to `.cognis/audit.log` (hashed args).
- Shared HTTP (optional, gate default OFF) is loopback-bound with repository
  identity and model-fingerprint checks; see
  [docs/security.md](https://github.com/buimanhtoan-it/cognis/blob/main/docs/security.md).


---

## Requirements

- VS Code 1.85+ or Cursor (any MCP-capable editor).
- Network access on first managed install, or a local source-built engine and
  semantic model assets.
- Rust stable and Node.js 18+ only when building from source.
- Languages indexed today: **TypeScript / JavaScript, Python, Go, C#, Java**.

---

## Links

- **Source, docs & issues:** [github.com/buimanhtoan-it/cognis](https://github.com/buimanhtoan-it/cognis)
- **How CSAR works (the math):** [docs/csar.md](https://github.com/buimanhtoan-it/cognis/blob/main/docs/csar.md)
- **MCP client setup:** [docs/mcp-client-config.md](https://github.com/buimanhtoan-it/cognis/blob/main/docs/mcp-client-config.md)
- **License:** Apache-2.0 — see [LICENSE.txt](LICENSE.txt)

---

<p align="center"><sub>Built for developers who want their AI agent to actually understand the codebase.</sub></p>
