#!/usr/bin/env bash
# First-time developer setup after cloning the cognis repo.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

echo "Building the Rust workspace"
cargo build --workspace

echo "Building VS Code / Cursor extension"
(
  cd apps/cognis-vscode
  npm install
  npm run package
)

cat <<EOF

Setup complete.
  Install VSIX : apps/cognis-vscode/cognis-vscode-*.vsix
  Dev checks   : cargo test --workspace
EOF
