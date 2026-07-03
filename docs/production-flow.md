# Deployment Flow

This document helps you choose the shortest path from a fresh checkout to a
working `cognis` environment.

## Choose a starting path

### Local editor workflow

Use this path when you want VS Code or Cursor to handle setup and MCP
configuration.

1. Install and open the editor extension, then click **Install backend** in the
   Cognis panel — it downloads the prebuilt `cognis` binary for your platform
   (no terminal, no Python). To build the extension `.vsix` from source:
   ```bash
   cd apps/cognis-vscode && npm install && npm run package
   ```
2. Open the target repository in VS Code or Cursor.
3. Run **Cognis: Set Up Workspace**.
4. If the workspace later drifts, run **Cognis: Troubleshoot & Repair**.

See [../apps/cognis-vscode/README.md](../apps/cognis-vscode/README.md) for the
extension workflow.

### Local CLI workflow

Use this path when you prefer terminal commands or are configuring a tool that
does not use the extension.

1. Get the `cognis` binary — download a prebuilt release and put it on your
   `PATH`, or build from source with `cargo build --release` (see
   [install.md](install.md)).
2. Bootstrap the repository you want to index:
   ```bash
   cognis bootstrap .
   ```
3. Start the MCP server:
   ```bash
   cognis mcpd
   ```
4. Optional: start the watcher to keep the index current:
   ```bash
   cognis indexd --repo-root .
   ```

### Docker Compose workflow

Use this path for a persistent self-hosted deployment.

1. Prepare the workspace on the host:
   ```bash
   cognis bootstrap .
   ```
2. Start the services:
   ```bash
   docker compose -f deploy/compose.yaml up -d
   ```
3. Confirm health:
   ```bash
   cognis health
   ```

For detailed operational steps, see [operations.md](operations.md).

## Optional: skip embeddings on the first run

If you want a faster first pass or are working without model downloads, you can
defer semantic embeddings:

```bash
cognis bootstrap . --skip-embeddings
```

This gives you lexical and structural retrieval immediately. Re-run indexing
without `--skip-embeddings` when you are ready to enable semantic search.

## Minimum acceptance checks

Before treating the setup as ready for daily use, confirm all of the following:

- `cognis health` reports `overall: ok`
- `.cognis/uckg.db` exists and is writable
- the index contains symbols from the target repository
- MCP tools respond successfully from the client you configured

## What can wait

These tasks are useful, but they are not required for the initial rollout:

- running eval baselines
- tuning retrieval quality
- changing the embedder model
- enabling reranking
- adjusting ignore rules beyond the defaults

## After the initial rollout

Once the basic flow works:

1. Review `.cognis/config.yaml` for language and path settings.
2. Re-index without `--skip-embeddings` if you initially deferred embeddings.
3. Add the MCP configuration to the client your team uses most often.
4. If you are operating a shared environment, document your upgrade and restart steps.
