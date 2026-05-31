# Self-Hosted Operations

This guide covers the supported self-hosted deployment model for `cognis`.

## Supported deployment model

The supported deployment path is Docker Compose using
[deploy/compose.yaml](../deploy/compose.yaml).

## Prerequisites

- Docker Engine 24+ with Compose v2
- a host directory containing the repository you want to index
- permission to write `.cognis/` inside that repository

## First deployment

From the `cognis` repository root:

```bash
export WORKSPACE_HOST_PATH=/path/to/your/codebase
docker compose -f deploy/compose.yaml build
docker compose -f deploy/compose.yaml run --rm mcpd cognis-cli init
docker compose -f deploy/compose.yaml run --rm mcpd cognis-cli index --full /workspace
docker compose -f deploy/compose.yaml up -d
```

What these commands do:

1. point the deployment at the repository you want to index
2. build the local image
3. create the `.cognis/` layout
4. run the first full index
5. start the MCP server and indexer daemon

## Health checks

Run the health check from the running container:

```bash
docker compose -f deploy/compose.yaml exec mcpd python -m cognis.cli.main health
```

Treat the deployment as ready only when:

- `.cognis/uckg.db` exists and is writable
- the database contains indexed symbols
- `index_version` matches the runtime version
- the MCP client can connect successfully

## Routine operations

### View logs

```bash
docker compose -f deploy/compose.yaml logs -f mcpd indexd
```

### Stop the deployment

```bash
docker compose -f deploy/compose.yaml down
```

### Restart the deployment

```bash
docker compose -f deploy/compose.yaml up -d
```

If you are working from a source checkout, you can also use:

```bash
cognis-cli up
cognis-cli down
```

## Audit trail

`cognis` writes an append-only audit log to:

```text
/workspace/.cognis/audit.log
```

Use this file to review MCP tool activity. Arguments are hashed before they are
written to the log.

## MCP client configuration

Once the containers are healthy, configure your MCP client to launch the server.
See [mcp-client-config.md](mcp-client-config.md).

## Upgrades

When upgrading the deployment:

1. pull or rebuild the new image
2. restart the services
3. run `cognis-cli health`
4. if the version check fails, run a full re-index:

```bash
docker compose -f deploy/compose.yaml exec mcpd python -m cognis.cli.main index --full /workspace
```

## Recovery notes

If the database becomes unusable after an interrupted upgrade or index run:

1. stop the deployment
2. back up `.cognis/`
3. remove the damaged database file
4. run `cognis-cli init` and `cognis-cli index --full /workspace` again
