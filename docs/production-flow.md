# Deployment Flow

This document helps you choose the shortest path from a fresh checkout to a
working `cognis` environment.

## Choose a starting path

### Local editor workflow

Use this path when you want VS Code or Cursor to handle setup and MCP
configuration.

1. Install the Python backend:
   ```bash
   python -m pip install -e ".[indexer,embed-local,vector,tokenizers,mcp]"
   ```
2. Build and install the editor extension from the `cognis` repository root:
   ```bash
   python scripts/setup_extension.py --package
   ```
3. Open the target repository in VS Code or Cursor.
4. Select the same Python interpreter used for the backend install.
5. Run **Cognis: Set Up for AI**.
6. If the workspace later drifts, run **Cognis: Repair Setup**.

See [../apps/cognis-vscode/README.md](../apps/cognis-vscode/README.md) for the
extension workflow.

### Local CLI workflow

Use this path when you prefer terminal commands or are configuring a tool that
does not use the extension.

1. Install the Python backend:
   ```bash
   python -m pip install -e ".[indexer,embed-local,vector,tokenizers,mcp]"
   ```
2. Bootstrap the repository you want to index:
   ```bash
   cognis-cli bootstrap .
   ```
   On Windows, use `python -m cognis.cli.main bootstrap .` if `cognis-cli` is
   not on `PATH`.
3. Start the MCP server:
   ```bash
   cognis-mcpd
   ```
4. Optional: start the watcher to keep the index current:
   ```bash
   cognis-indexd --repo-root .
   ```

### Docker Compose workflow

Use this path for a persistent self-hosted deployment.

1. Prepare the workspace on the host:
   ```bash
   cognis-cli bootstrap .
   ```
2. Start the services:
   ```bash
   cognis-cli up
   ```
3. Confirm health:
   ```bash
   cognis-cli health
   ```

For detailed operational steps, see [operations.md](operations.md).

## Optional: skip embeddings on the first run

If you want a faster first pass or are working without model downloads, you can
defer semantic embeddings:

```bash
cognis-cli bootstrap . --skip-embeddings
```

This gives you lexical and structural retrieval immediately. Re-run indexing
without `--skip-embeddings` when you are ready to enable semantic search.

## Minimum acceptance checks

Before treating the setup as ready for daily use, confirm all of the following:

- `cognis-cli health` reports `overall: ok`
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
