#!/usr/bin/env bash
# One-shot Cognis workspace setup: init + index + health (see docs/quickstart.md).
set -eu

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

TARGET="${1:-.}"
SKIP_ARGS=()
if [ "${SKIP_EMBEDDINGS:-}" = "1" ]; then
  SKIP_ARGS=(--skip-embeddings)
fi

cargo run --release -p cognis -- bootstrap "${TARGET}" "${SKIP_ARGS[@]}"
