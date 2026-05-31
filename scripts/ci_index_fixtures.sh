#!/usr/bin/env bash
# Index eval fixture repos into .cognis/uckg.db for nightly eval / CI.
set -eu

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

export COGNIS_DB_PATH="${COGNIS_DB_PATH:-$ROOT/.cognis/uckg.db}"

python -m cognis.cli.main init

# export SKIP_EMBEDDINGS=1 to skip Hugging Face download (lexical/structural only).
SKIP_ARGS=()
if [ "${SKIP_EMBEDDINGS:-}" = "1" ]; then
  SKIP_ARGS=(--skip-embeddings)
fi

for repo in mini-ts-app mini-py-svc mini-go-svc; do
  echo "Indexing tests/fixtures/repos/${repo} ..."
  python -m cognis.cli.main index --full "${SKIP_ARGS[@]}" "tests/fixtures/repos/${repo}"
done

python -m cognis.cli.main health --json
