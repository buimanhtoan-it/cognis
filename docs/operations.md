# Self-Hosted Container Status

Cognis containers are a source-build deployment option, not a separate prebuilt
product channel. Polar delivers only the versioned editor ZIP, and GitHub does
not advertise a public container image as an end-user download.

## Current status

`deploy/compose.yaml` documents the intended two-service topology (`mcpd` plus
`indexd`), but this checkout currently has no root `Dockerfile`. Therefore the
Compose stack is not a ready-to-run supported install from a fresh clone.

Use the local source-built binary flow in [install.md](install.md) unless you
provide and maintain a Dockerfile that builds Cognis from this repository.

## Required source-built topology

A custom image must:

- build the pure-Rust `cognis` binary from the same source version;
- include or mount the semantic model assets;
- mount one target repository at `/workspace`;
- persist `/workspace/.cognis`;
- run `cognis mcpd` and `cognis indexd --repo-root /workspace`;
- expose MCP only according to the isolation rules in [security.md](security.md).

Prefer one repository per deployment. Do not reuse the stale public image names
that may remain in historical Compose examples.

## Local operational checks

The supported source-built binary checks are:

```bash
cognis bootstrap /path/to/workspace
cognis health
cognis mcpd
cognis indexd --repo-root /path/to/workspace
```

Audit data remains under `.cognis/audit.log`. See
[mcp-client-config.md](mcp-client-config.md) for client wiring and workspace
isolation.