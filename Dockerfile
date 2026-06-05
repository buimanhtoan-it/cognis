# cognis Docker image — Task 18.4
#
# Builds a production-ready image with:
#   - cognis-mcpd (MCP server)
#   - cognis-indexd (file watcher + indexer daemon)
#   - cognis-cli (operator commands)
#   - bge-small-en-v1.5 embedding model pre-cached
#
# Usage:
#   docker build -t cognis-engine:0.3.1 .
#   docker run -v /your/repo:/workspace -e COGNIS_DB_PATH=/workspace/.cognis/uckg.db cognis-engine:0.3.1 cognis-mcpd
#
# For development (with source bind-mount):
#   docker run -v $(pwd):/app -v /your/repo:/workspace cognis-engine:dev cognis-cli health

# ---------------------------------------------------------------------------
# Stage 1: Build dependencies
# ---------------------------------------------------------------------------
FROM python:3.11-slim AS builder

WORKDIR /build

# Install build tools.
RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential \
    git \
    && rm -rf /var/lib/apt/lists/*

# Copy source and install.
COPY pyproject.toml README.md LICENSE CHANGELOG.md ./
COPY packages/ packages/
COPY apps/ apps/

RUN pip install --no-cache-dir --upgrade pip && \
    pip install --no-cache-dir \
        ".[indexer,embed-local,vector,tokenizers,mcp]"

# Pre-download and cache bge-small-en-v1.5 so the container starts offline.
RUN python -c "\
from sentence_transformers import SentenceTransformer; \
model = SentenceTransformer('BAAI/bge-small-en-v1.5'); \
print('bge-small-en-v1.5 cached successfully')"

# ---------------------------------------------------------------------------
# Stage 2: Runtime image
# ---------------------------------------------------------------------------
FROM python:3.11-slim AS runtime

# Create non-root user for security.
RUN useradd -m -u 1000 -s /bin/bash cognis

WORKDIR /app

# Copy installed packages from builder.
COPY --from=builder /usr/local/lib/python3.11 /usr/local/lib/python3.11
COPY --from=builder /usr/local/bin/cognis-cli /usr/local/bin/cognis-cli
COPY --from=builder /usr/local/bin/cognis-mcpd /usr/local/bin/cognis-mcpd
COPY --from=builder /usr/local/bin/cognis-indexd /usr/local/bin/cognis-indexd

# Copy pre-cached model from builder's Hugging Face cache.
COPY --from=builder /root/.cache/huggingface /home/cognis/.cache/huggingface
RUN chown -R cognis:cognis /home/cognis/.cache

# Switch to non-root user.
USER cognis

# Default workspace mount point.
VOLUME ["/workspace"]

# Environment defaults.
ENV COGNIS_DB_PATH=/workspace/.cognis/uckg.db \
    COGNIS_AUDIT_LOG=/workspace/.cognis/audit.log \
    COGNIS_LOG_LEVEL=INFO \
    PYTHONUNBUFFERED=1 \
    HOME=/home/cognis

# Healthcheck via CLI.
HEALTHCHECK --interval=30s --timeout=15s --start-period=30s --retries=3 \
    CMD python -m cognis.cli.main health --json | python -c "import sys,json; d=json.load(sys.stdin); sys.exit(0 if d['overall']!='fail' else 1)"

# Default command: start the MCP server.
CMD ["cognis-mcpd"]

# Labels.
LABEL org.opencontainers.image.title="cognis" \
      org.opencontainers.image.description="Software Cognition Engine — MCP-native context capsules" \
      org.opencontainers.image.source="https://github.com/buimanhtoan-it/cognis" \
      org.opencontainers.image.licenses="Apache-2.0"
