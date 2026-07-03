# Release Guide

This document is for maintainers preparing a tagged release of the pure-Rust
`cognis` engine.

## Before starting

Complete the full pre-release checks from the repo root:

1. Run the workspace test suite:
   ```bash
   cargo test --workspace
   ```
2. Run lints and formatting:
   ```bash
   cargo clippy --workspace --all-targets -- -D warnings
   cargo fmt --all --check
   ```
3. Confirm the release notes and `CHANGELOG.md` are up to date.
4. Update the workspace version in `Cargo.toml` (`[workspace.package] version`).

If you track release quality gates, review the current eval baseline in
`docs/eval/phase1-baseline.md`.

## Versioning

`cognis` follows [Semantic Versioning](https://semver.org/):

- stable releases: `MAJOR.MINOR.PATCH`
- pre-releases: `0.8.0-rc.1`

The single source of truth is `[workspace.package].version` in `Cargo.toml`;
every crate and binary inherits it via `version.workspace = true`.

## Release steps

### 1. Update the version

Edit `Cargo.toml`:

```toml
[workspace.package]
version = "0.8.0"
```

Run `cargo build --workspace` so `Cargo.lock` picks up the new version, and
commit both files.

### 2. Update the changelog

Add a new dated section to `CHANGELOG.md` describing the release contents and
open a fresh `[Unreleased]` section.

### 3. Build the artifacts

Build the single-binary distribution (one static `cognis` binary per platform):

```bash
cargo xtask dist
```

Expected outputs (under `dist/`): the per-platform `cognis` binary plus its
`.sha256` checksum sidecar. See [docs/distribution.md](distribution.md) for the
build matrix and platform targets.

Build and push the container image:

```bash
docker build -t cognis-engine:0.8.0 -t cognis-engine:latest .
docker push ghcr.io/buimanhtoan-it/cognis-engine:0.8.0
docker push ghcr.io/buimanhtoan-it/cognis-engine:latest
```

### 4. Create the Git tag

```bash
git add -A
git commit -m "chore: release v0.8.0"
git tag -a v0.8.0 -m "Release v0.8.0"
git push origin main --tags
```

Pushing a `v*` tag triggers the automated release workflow, which builds the
cross-platform binary matrix and attaches the binaries + checksums to the
GitHub Release. See [.github/workflows/release.yml](../.github/workflows/release.yml).

### 5. Create / verify the GitHub Release

The workflow creates the release from the tag; verify the attached assets, or
create it manually:

```bash
gh release create v0.8.0 \
    --title "cognis v0.8.0" \
    --notes-file docs/release-notes-v0.3.0.md \
    dist/cognis-*
```

## Docker Image Publishing

The Dockerfile is at the project root. Images are published to:
`ghcr.io/buimanhtoan-it/cognis-engine:<version>`

```bash
docker build -t ghcr.io/buimanhtoan-it/cognis-engine:0.8.0 .
echo $GITHUB_TOKEN | docker login ghcr.io -u USERNAME --password-stdin
docker push ghcr.io/buimanhtoan-it/cognis-engine:0.8.0
```

## Post-Release

- [ ] Update `README.md` badge with the new version
- [ ] Post announcement to relevant channels
- [ ] Open `[Unreleased]` section in `CHANGELOG.md` for next release
- [ ] File issues for any known regressions or deferred features

## Rollback Procedure

If a critical bug is found post-release:

1. Mark the GitHub Release as a draft/pre-release (or delete it) so the binaries
   are no longer advertised as stable: `gh release edit v0.8.0 --prerelease`.
2. Retract the Docker image: `docker manifest rm ghcr.io/buimanhtoan-it/cognis-engine:0.8.0`
3. Fix the bug, increment the patch version, re-release as `0.8.1`.
