# Distribution

Cognis uses one codebase and one end-user prebuilt product:

- **Polar:** one versioned `cognis-prebuilt-<version>.zip`.
- **GitHub:** public Apache-2.0 source, tags, documentation, release notes, and
  release infrastructure used by the managed installer.
- **Source users:** build the same engine and extension for free.

Polar does not deliver a separate VSIX, binary, container, license key, or
activation benefit. The purchase pays for prebuilding, packaging, delivery, and
support; it does not unlock features.

## ZIP contract

Every ZIP uploaded to Polar contains exactly:

```text
cognis-prebuilt-<version>.zip
  cognis-vscode-<version>.vsix
  INSTALL.md
  LICENSE.txt
```

`INSTALL.md` and filenames must match the extension and Cargo workspace version.
`LICENSE.txt` carries the Apache-2.0 notice. Do not include signing keys,
activation instructions, seller secrets, or a second nested product download.

The VSIX uses the managed install flow to fetch the matching platform engine and
semantic model assets from release infrastructure, verify each SHA-256 sidecar,
and stage them under editor global storage. Those release assets are an
implementation dependency of the ZIP, not a separately advertised end-user
product channel.

## Engine artifact

The engine is one Rust `cognis` binary per platform. It dispatches the CLI, MCP
server, and indexing daemon surfaces:

- `cognis cli ...` or bare CLI commands
- `cognis mcpd`
- `cognis indexd ...`

SQLite and FTS5 are bundled. Build with `--features onnx-download` to link ONNX
Runtime into a release binary. Model weights remain separate assets selected by
`COGNIS_ONNX_MODEL_DIR` or the managed installer.

Supported managed-install targets are:

| Platform | Target triple |
| --- | --- |
| Linux x64 | `x86_64-unknown-linux-gnu` |
| macOS Apple Silicon | `aarch64-apple-darwin` |
| Windows x64 | `x86_64-pc-windows-msvc` |

## Build from source

Source users need Git, stable Rust, and Node.js 18+ for the extension:

```bash
git clone https://github.com/buimanhtoan-it/cognis
cd cognis
cargo build --release -p cognis --bin cognis --features onnx-download
cd apps/cognis-vscode
npm install
npm run package
```

Set `cognis.binaryPath` to the resulting `target/release/cognis` binary. Obtain
local model files as documented in [`assets/models/README.md`](../assets/models/README.md)
and set `COGNIS_ONNX_MODEL_DIR` when semantic search is required.

Maintainers can stage checksummed engine inputs with:

```bash
cargo xtask dist --features onnx-download
```

## Release integrity

Before uploading the Polar ZIP:

1. keep Cargo, extension, lockfile, site config, ZIP name, and INSTALL version in
   lockstep;
2. run the engine and extension CI gates;
3. build from a clean extension `out/` directory;
4. inspect the ZIP and nested VSIX;
5. verify there is no license-key command, activation code, secret, or stale
   artifact;
6. verify the managed engine/model release assets exist for the exact extension
   version;
7. upload only the final ZIP to Polar.

Checksums protect downloaded bytes. Artifact signing/provenance can be added to
the release infrastructure without changing the one-ZIP business model.