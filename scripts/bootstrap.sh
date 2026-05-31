#!/usr/bin/env bash
# One-shot cognis setup: init + index + health (see docs/production-flow.md).
set -eu

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

TARGET="${1:-.}"
SKIP_ARGS=()
if [ "${SKIP_EMBEDDINGS:-}" = "1" ]; then
  SKIP_ARGS=(--skip-embeddings)
fi

python -m cognis.cli.main bootstrap "${TARGET}" "${SKIP_ARGS[@]}"
