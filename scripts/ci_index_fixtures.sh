#!/usr/bin/env bash
# Index eval fixture repos into .cognis/uckg.db for nightly eval / CI.
set -eu

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

export COGNIS_DB_PATH="${COGNIS_DB_PATH:-$ROOT/.cognis/uckg.db}"

cargo run --release -p cognis -- init

# export SKIP_EMBEDDINGS=1 to skip Hugging Face download (lexical/structural only).
SKIP_ARGS=()
if [ "${SKIP_EMBEDDINGS:-}" = "1" ]; then
  SKIP_ARGS=(--skip-embeddings)
fi

for repo in mini-ts-app mini-py-svc mini-go-svc; do
  repo_path="tests/fixtures/repos/${repo}"
  if [ ! -d "${repo_path}" ]; then
    echo "Skipping ${repo_path} (not present)"
    continue
  fi
  echo "Indexing ${repo_path} ..."
  cargo run --release -p cognis -- index --full "${SKIP_ARGS[@]}" "${repo_path}"
done

cargo run --release -p cognis -- health --json
