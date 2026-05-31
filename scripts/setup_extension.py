"""Build the Cognis VS Code extension after clone or dependency changes."""

from __future__ import annotations

import argparse
import shutil
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
EXTENSION_DIR = ROOT / "apps" / "cognis-vscode"


def _require_npm() -> str:
    npm = shutil.which("npm")
    if npm is None:
        raise SystemExit(
            "npm was not found on PATH. Install Node.js 18+ to build apps/cognis-vscode."
        )
    return npm


def _run(npm: str, *args: str) -> None:
    cmd = [npm, *args]
    print(f"+ {' '.join(cmd)}  (cwd={EXTENSION_DIR})")
    subprocess.run(cmd, cwd=EXTENSION_DIR, check=True)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--package",
        action="store_true",
        help="Also run `npm run package` to produce a .vsix after compile.",
    )
    parser.add_argument(
        "--skip-install",
        action="store_true",
        help="Skip `npm install` (use when node_modules is already present).",
    )
    args = parser.parse_args(argv)

    if not EXTENSION_DIR.is_dir():
        raise SystemExit(f"Extension directory not found: {EXTENSION_DIR}")

    npm = _require_npm()
    if not args.skip_install:
        _run(npm, "install")
    _run(npm, "run", "compile")
    if args.package:
        _run(npm, "run", "package")
        print("Extension package ready under apps/cognis-vscode/*.vsix")
    else:
        print("Extension compiled to apps/cognis-vscode/out/")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
