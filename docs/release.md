# Release Guide

This document is for maintainers preparing a tagged release.

## Before starting

Complete the full pre-release checks:

1. Run the test suite:
   ```bash
   make test
   pytest -m integration --maxfail=5
   ```
2. Run lint and type checks:
   ```bash
   make lint typecheck
   ```
3. Run MCP conformance:
   ```bash
   cognis-cli mcp-conformance
   ```
4. Confirm the release notes and `CHANGELOG.md` are up to date.
5. Update the version in `pyproject.toml`.

If you track release quality gates, review the current eval baseline in
`docs/eval/phase1-baseline.md`.

## Versioning

`cognis` follows [Semantic Versioning](https://semver.org/):

- stable releases: `MAJOR.MINOR.PATCH`
- pre-releases: `0.1.0.dev0`, `0.1.0a1`, `0.1.0rc1`

## Release steps

### 1. Update the version

Edit `pyproject.toml`:

```toml
[project]
version = "0.3.0"
```

### 2. Update the changelog

Add a new dated section to `CHANGELOG.md` describing the release contents.

### 3. Build the artifacts

Build the Python distribution:

```bash
python -m pip install build
python -m build
```

Expected outputs:

- `dist/cognis-<version>.tar.gz`
- `dist/cognis-<version>-py3-none-any.whl`

Build and push the container image:

```bash
docker build -t cognis-engine:0.3.0 -t cognis-engine:latest .
docker push ghcr.io/buimanhtoan-it/cognis-engine:0.3.0
docker push ghcr.io/buimanhtoan-it/cognis-engine:latest
```

### 4. Publish the Python package

If PyPI publishing is enabled for the repository:

```bash
python -m pip install twine
twine upload dist/*
```

You can also trigger the automated release workflow by pushing a `v*` tag.
See [.github/workflows/release.yml](../.github/workflows/release.yml).

### 5. Create the Git tag

```bash
git add -A
git commit -m "chore: release v0.3.0"
git tag -a v0.3.0 -m "Release v0.3.0"
git push origin main --tags
```

### 6. Create GitHub Release

```bash
gh release create v0.3.0 \
    --title "cognis v0.3.0" \
    --notes-file docs/release-notes-v0.3.0.md \
    dist/*.tar.gz dist/*.whl
```

## Docker Image Publishing

The Dockerfile is at the project root. Images are published to:
`ghcr.io/buimanhtoan-it/cognis-engine:<version>`

```bash
docker build -t ghcr.io/buimanhtoan-it/cognis-engine:0.3.0 .
echo $GITHUB_TOKEN | docker login ghcr.io -u USERNAME --password-stdin
docker push ghcr.io/buimanhtoan-it/cognis-engine:0.3.0
```

## Post-Release

- [ ] Update `README.md` badge with new version
- [ ] Post announcement to relevant channels
- [ ] Open `[Unreleased]` section in `CHANGELOG.md` for next release
- [ ] File issues for any known regressions or deferred features

## Rollback Procedure

If a critical bug is found post-release:

1. Yank the PyPI release: `pip install twine && twine upload --skip-existing dist/* && twine yank cognis-engine==0.3.0`
2. Retract the Docker image: `docker manifest rm ghcr.io/buimanhtoan-it/cognis-engine:0.3.0`
3. Fix the bug, increment patch version, re-release as `0.3.1`
