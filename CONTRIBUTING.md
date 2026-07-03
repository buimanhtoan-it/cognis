# Contributing to cognis

Thank you for contributing to `cognis`. The engine is a pure-Rust Cargo
workspace — there is no Python, `pip`, or virtualenv anywhere in the build.

## Development setup

You need the [Rust toolchain](https://rustup.rs) (stable) and Git. Node.js 18+
is only required if you work on the VS Code / Cursor extension.

```bash
git clone https://github.com/buimanhtoan-it/cognis
cd cognis
cargo build --workspace
```

On Windows, the one-step helper builds the workspace and packages the extension:

```powershell
.\scripts\setup-dev.ps1
```

The workspace layout:

- `crates/*` — engine libraries (`cognis-core`, `cognis-store`, `cognis-embed`,
  `cognis-indexer`, `cognis-retrieval`, `cognis-csar`, `cognis-mcp`,
  `cognis-eval`)
- `bins/*` — the runtime surfaces (`cognis-cli`, `cognis-indexd`, `cognis-mcpd`)
  plus the single multi-call `cognis` binary
- `xtask` — build / distribution automation (`cargo xtask dist`)
- `native/csar-rs` — the CSAR kernel `cdylib`
- `apps/cognis-vscode` — the TypeScript VS Code / Cursor extension

## Day-to-day workflow

Standard Rust tooling drives every check:

```bash
cargo test --workspace      # unit + property (CSAR theorems T1–T5) + parity tests
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
```

For the extension, work from its own directory:

```bash
cd apps/cognis-vscode
npm install
npm test
```

## Before opening a pull request

Run the full local checks from the repo root:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

If you touched the extension, also run `npm test` (and, if relevant,
`npm run test:host`) under `apps/cognis-vscode`.

## Pull request expectations

- Keep each pull request focused on a single problem or feature.
- Update documentation when changing installation, configuration, or operational behavior.
- Add or update tests when the change affects runtime behavior.
- Do not commit secrets, `.cognis/` runtime state, generated reports, or local scratch files.

## Release process

Maintainers should follow [docs/release.md](docs/release.md). Releases are
tag-driven through `.github/workflows/release.yml`, which builds the
per-platform single-binary matrix.

## Code of conduct

Be respectful, specific, and constructive in issues, reviews, and commits.
