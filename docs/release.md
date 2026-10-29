# Release Guide

This guide documents how maintainers cut a versioned Cognis release across the Rust workspace, GitHub Releases, and the VS Code / Cursor `.vsix` extension.

## 1. Version Lockstep

`[workspace.package].version` in the root `Cargo.toml` is the canonical version source of truth. Keep these files in exact lockstep:

- `Cargo.toml` (`[workspace.package].version`)
- `apps/cognis-mcp/package.json` (`version`)
- `apps/cognis-mcp/package-lock.json` (both root `version` fields)
- `CHANGELOG.md`

Also keep `CONTRACT_VERSION` (`crates/cognis-core/src/contract.rs`) and `EXPECTED_CONTRACT_VERSION` (`apps/cognis-mcp/src/contract.ts`) synchronized whenever CLI or MCP JSON schemas change.

## 2. Run Pre-Release Verification Gates

From the repository root (with test override env vars cleared):

```powershell
Remove-Item Env:\COGNIS_DB_PATH,Env:\COGNIS_MCP_FIXTURE,Env:\COGNIS_INDEXD_STATUS_PATH -ErrorAction SilentlyContinue
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo build --workspace
cargo test --workspace
```

From `apps/cognis-mcp`:

```powershell
npm ci
npm run compile
npm run lint
npm test
npm run package
```

## 3. Tag and Trigger GitHub Release

Once all Rust and TypeScript gates are green on `main`, push an annotated tag:

```bash
git tag -a v0.1.0 -m "Release v0.1.0"
git push origin v0.1.0
```

`.github/workflows/release.yml` automatically:

1. Builds self-contained platform binaries (`x86_64-unknown-linux-gnu`, `aarch64-apple-darwin`, `x86_64-pc-windows-msvc`) via `cargo xtask verify-release --features release-full` with `.sha256` sidecars.
2. Provisions checksum-verified `bge-small-en-v1.5` ONNX semantic model assets (`model.onnx`, `tokenizer.json`, `pooling.json` + `.sha256` sidecars).
3. Runs an end-to-end semantic MCP smoke gate against the compiled release binary.
4. Publishes the GitHub Release assets consumed by the extension's managed **Install engine** flow.

## 4. Attach / Publish the VSIX

Upload the verified `apps/cognis-mcp/cognis-mcp-<version>.vsix` artifact to the GitHub Release (and/or publish to the VS Code Marketplace / Open VSX Registry).

## 5. Rollback Procedure

If a post-release defect is discovered:

1. Mark the affected GitHub Release as **Pre-release** (or retract broken assets).
2. Land the fix with a regression test on `main`.
3. Bump the patch version (`0.1.x`), rerun all gates in Section 2, and push a new annotated tag.
