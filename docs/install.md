# Installation Guide

This guide covers installing the `cognis` engine, optional editor integration,
and the checks you should run before using it on a real codebase.

`cognis` ships as a **single static Rust binary** per platform. There is no
Python runtime, no `pip`, and no virtual environment to manage — the binary is
self-contained (SQLite is compiled in).

## Requirements

- Linux, macOS, or Windows on a supported architecture (see
  [distribution.md](distribution.md) for the target matrix)
- roughly 200 MB of free disk space for the binary and the local embedding model
- Node.js 18 or newer **only** if you plan to build the VS Code / Cursor
  extension from source
- the [Rust toolchain](https://rustup.rs) (stable) **only** if you build the
  engine from source instead of downloading a prebuilt binary

## Option A — prebuilt binary (recommended)

Download the `cognis` binary for your platform from the
[latest release](https://github.com/buimanhtoan-it/cognis/releases). Each
artifact ships with a `.sha256` sidecar so you can verify it.

```bash
# Verify the download (sha256sum -c format)
sha256sum -c cognis-<triple>.sha256

# Put it on PATH and mark it executable (POSIX)
chmod +x cognis-<triple>
mv cognis-<triple> /usr/local/bin/cognis
```

On Windows, verify with `Get-FileHash cognis-<triple>.exe -Algorithm SHA256` and
place `cognis.exe` somewhere on your `PATH`.

The single binary is multi-call (busybox-style): it behaves as the CLI, the MCP
server, or the indexing daemon depending on how it is invoked:

- `cognis cli …` (or any bare subcommand such as `cognis bootstrap .`)
- `cognis mcpd` — start the MCP server on stdio
- `cognis indexd …` — start the live-indexing daemon

It also works when installed or symlinked under the legacy names `cognis-cli`,
`cognis-mcpd`, and `cognis-indexd`, so existing `mcp.json` wiring keeps working.

## Option B — build from source

Building requires only the Rust toolchain and Git — no Python.

```bash
git clone https://github.com/buimanhtoan-it/cognis
cd cognis
cargo build --release
```

The build produces the single binary at `target/release/cognis` (or
`cognis.exe` on Windows). Copy it onto your `PATH`, or run it in place:

```bash
./target/release/cognis --version
./target/release/cognis health
```

To produce a stripped, distribution-ready artifact (and a `.sha256` sidecar),
use the packaging task:

```bash
cargo xtask dist                    # host target → dist/
cargo xtask dist --target <triple>  # a specific platform
```

See [distribution.md](distribution.md) for the full build matrix, cross-compile
notes, and the optional `onnx` / `onnx-download` features.

## Verify the installation

Run:

```bash
cognis --version
cognis health
```

If you installed under the legacy name instead, `cognis-cli --version` and
`cognis-cli health` behave identically.

It is normal for `health` to report warnings before you initialize a repository.
The first successful `init` or `bootstrap` run creates `.cognis/` and the local
database.

## Optional: build the VS Code / Cursor extension

For editor integration the recommended path is the prebuilt extension: install
the `.vsix`, open the Cognis panel, and click **Install backend** — the
extension downloads the prebuilt `cognis` binary for your platform (checksum
verified), so no terminal or compiler is needed.

To build the extension from source, package it from its own directory:

```bash
cd apps/cognis-vscode
npm install
npm run package
```

This creates `apps/cognis-vscode/cognis-vscode-<version>.vsix`.

Install the package in VS Code or Cursor:

1. Open the Extensions view.
2. Open the `...` menu.
3. Select **Install from VSIX...**
4. Choose `cognis-vscode-<version>.vsix`.
5. Open the target repository and run **Cognis: Set Up Workspace**.

The extension manages the `cognis` binary backend for you (download, version
drift detection, and `mcp.json` wiring). For extension-specific details, see
[../apps/cognis-vscode/README.md](../apps/cognis-vscode/README.md).

## sqlite-vec

`cognis` uses vector search for the semantic layer. The default binary ships an
**in-Rust BLOB + linear-scan fallback**, so vector search works out of the box
with no extension to install. For larger indexes you can opt into the faster
`vec0` loadable extension by pointing `COGNIS_SQLITE_VEC_PATH` at a `sqlite-vec`
build; the store loads it at runtime and falls back automatically if it cannot
load. Lexical and structural retrieval are unaffected either way.

## Docker deployment

For a persistent self-hosted deployment, use Docker Compose:

```bash
export WORKSPACE_HOST_PATH=/path/to/your/codebase
docker compose -f deploy/compose.yaml up -d
```

Published image tags use `ghcr.io/buimanhtoan-it/cognis-engine:<version>`.
Operational steps are documented in [operations.md](operations.md).

## Windows notes

The binary is self-contained, so the most common Python-era `PATH` problems no
longer apply. Place `cognis.exe` on your `PATH` (or invoke it by full path) and
you are done. If you mainly use the VS Code / Cursor extension, the **Install
backend** flow stages the binary under the extension's storage and wires it for
you — no manual `PATH` setup required.

## Upgrading

Replace the binary with the newer release (or rebuild from an updated checkout)
and re-run the health check:

```bash
cognis health
```

If `health` reports a version mismatch, run a full re-index:

```bash
cognis index --full .
```

## Uninstalling

Delete the `cognis` binary from wherever you placed it. Optional cleanup:

- remove `.cognis/` from repositories you indexed
- uninstall the `.vsix` from the editor if you no longer need it

## Troubleshooting

| Problem | What to check |
| --- | --- |
| `cognis` is not recognized | Confirm the binary is on `PATH`, or invoke it by full path |
| Health reports a version mismatch | Run `cognis index --full .` to rebuild the index |
| Vector search unavailable | The BLOB fallback always works; for `vec0`, check `COGNIS_SQLITE_VEC_PATH` |
| Permission errors under `.cognis/` | Verify the current user can create and write files in the repository |
| `COGNIS_DB_PATH` is incorrect | Re-run `cognis init` or inspect `.cognis/config.yaml` and your environment |
