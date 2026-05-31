#!/usr/bin/env bash
# First-time developer setup after cloning the cognis repo.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

PY="${ROOT}/.venv/bin/python"
if [[ ! -x "$PY" ]]; then
  echo "Creating virtual environment in .venv"
  python3 -m venv .venv
fi

echo "Installing Python backend and dev tools"
"$PY" -m pip install -e ".[indexer,embed-local,vector,tokenizers,mcp]"
"$PY" -m pip install --group dev

echo "Refreshing branding assets (best effort)"
"$PY" scripts/prepare-branding.py || true

echo "Building VS Code / Cursor extension"
"$PY" scripts/setup_extension.py --package

cat <<EOF

Setup complete.
  Activate venv : source .venv/bin/activate
  Install VSIX  : apps/cognis-vscode/cognis-vscode-*.vsix
  Dev checks    : make test
EOF
