#!/usr/bin/env python3
"""Build the standalone Cognis installer bundle (the prebuilt distribution).

Packages the VS Code / Cursor extension (`.vsix`) plus install instructions and
the license into a single zip download, so a user can install without the source
setup.

The output goes to ``dist/`` which is **git-ignored** — it is a build product,
never committed. The open-source engine is installed by the extension itself on
first run (one click), so the bundle stays small and does not embed a Python
runtime.

Usage (from repo root):

    python scripts/build_installer.py            # build dist/cognis-build-<ver>.zip
    python scripts/build_installer.py --clean    # wipe dist/ first

Requirements: Node.js + npm (for `vsce`). The script runs `npm install` only if
``node_modules`` is missing, then compiles and packages the extension.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import zipfile
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
EXT_DIR = REPO_ROOT / "apps" / "cognis-vscode"
DIST_DIR = REPO_ROOT / "dist"


def _run(cmd: list[str], cwd: Path) -> None:
    """Run a command, echoing it, and abort the build on failure."""
    print(f"$ {' '.join(cmd)}  (cwd={cwd})")
    # shell=True on Windows so npm/npx (.cmd shims) resolve on PATH.
    completed = subprocess.run(cmd, cwd=str(cwd), shell=(os.name == "nt"))
    if completed.returncode != 0:
        sys.exit(f"command failed ({completed.returncode}): {' '.join(cmd)}")


def _read_version() -> str:
    data = json.loads((EXT_DIR / "package.json").read_text(encoding="utf-8"))
    return str(data["version"])


def _build_vsix(version: str) -> Path:
    """Compile + package the extension, returning the produced .vsix path."""
    if not (EXT_DIR / "node_modules").exists():
        _run(["npm", "install"], cwd=EXT_DIR)
    _run(["npm", "run", "compile"], cwd=EXT_DIR)
    _run(["npx", "vsce", "package", "--allow-missing-repository"], cwd=EXT_DIR)
    vsix = EXT_DIR / f"cognis-vscode-{version}.vsix"
    if not vsix.exists():
        sys.exit(f"expected {vsix} after packaging, but it was not found")
    return vsix


def _install_guide(version: str) -> str:
    return f"""# Cognis — Install Guide (v{version})

Thank you for your purchase. This bundle contains the prebuilt Cognis extension
for VS Code and Cursor.

## What's in this bundle

- `cognis-vscode-{version}.vsix` — the Cognis extension (the product).
- `LICENSE.txt` — your commercial license terms.
- `INSTALL.md` — this guide.

## Requirements

- VS Code 1.85+ or Cursor (any MCP-capable editor).
- Python 3.11+ available on your machine. The extension installs and manages the
  rest of the engine for you, in one click — no terminal, no pip.
- **Internet access on first setup.** Clicking *Install backend* downloads the
  Cognis engine and its dependencies (~1-2 GB, including local ML models) from
  PyPI into a private environment. This is a one-time download; everything runs
  locally afterward and your code never leaves your machine.

## Install (2 minutes)

1. Open VS Code or Cursor.
2. Open the Extensions view (Ctrl/Cmd+Shift+X).
3. Click the `...` menu → **Install from VSIX...**
4. Select `cognis-vscode-{version}.vsix` from this folder.
5. Open the **Cognis** panel in the sidebar and click **Install backend**.
   Cognis sets up its private environment automatically (this build installs the
   matching engine `cognis-engine=={version}`, so your setup is reproducible).
6. Open your project and click **Set Up for AI**.
7. Reload the editor when prompted. Done — your AI agent can now search your
   code with Cognis.

## Updating to a new version

When you receive a new bundle (a fix or a newer release), updating takes about a
minute:

1. Download the new bundle and unzip it.
2. In VS Code/Cursor: open Extensions (Ctrl/Cmd+Shift+X) -> the `...` menu ->
   **Install from VSIX...** -> pick the new `cognis-vscode-{version}.vsix`.
   Installing over the previous version is fine — you do not need to uninstall
   first.
3. **Reload** the editor when prompted (a "Reload Window" button appears, or run
   *Developer: Reload Window* from the command palette).
4. Open the **Cognis** panel and click **Install backend** once (or run
   *Cognis: Install backend* from the command palette). Cognis upgrades the
   engine to match this version (`cognis-engine=={version}`). This is much
   faster than the first install — the large machine-learning dependencies are
   already cached, so only the small engine package is fetched.

Your already-indexed workspaces keep working — you do **not** need to re-index
after an update. If the panel ever looks stuck, run *Cognis: Show Output* from
the command palette to see what it is doing.

## Need the source-build (expert) path instead?

Cognis is also available as an open-source engine you can install and wire by
hand. That route is for experts comfortable with Python environments and MCP
configuration. Most users should prefer this one-click bundle. See the project
README for the manual path.

## Support

Questions or issues: buimanhtoan.it@gmail.com
"""


def _check_pypi_published(version: str) -> bool:
    """Return True if cognis-engine==version is downloadable from PyPI.

    The sold bundle's "Install backend" button does `pip install
    cognis-engine[...]==<version>`. If that version is not on PyPI yet, the
    bundle is dead on arrival — so we check the simple JSON API and warn loudly.
    Network failures are treated as "unknown" (warn, do not block).
    """
    import json
    import urllib.error
    import urllib.request

    url = f"https://pypi.org/pypi/cognis-engine/{version}/json"
    try:
        with urllib.request.urlopen(url, timeout=10) as resp:
            return resp.status == 200 and bool(json.load(resp))
    except urllib.error.HTTPError as exc:
        if exc.code == 404:
            return False
        return False
    except Exception:
        # Offline / DNS / timeout: can't confirm. Don't block the build.
        print("  (could not reach PyPI to verify publication — skipping check)")
        return True


def build(clean: bool) -> Path:
    version = _read_version()

    # Preflight: the bundle's one-click backend install pulls cognis-engine==<v>
    # from PyPI. Selling a bundle whose engine isn't published yet means the
    # buyer's "Install backend" fails. Warn before producing the artifact.
    if not _check_pypi_published(version):
        print(
            f"\n  WARNING: cognis-engine=={version} was not found on PyPI.\n"
            f"  The bundle's 'Install backend' step will FAIL for buyers until you\n"
            f"  publish it (tag v{version} with PYPI_API_TOKEN set, or `twine upload`).\n"
            f"  Building anyway so you can test locally.\n"
        )

    if clean and DIST_DIR.exists():
        shutil.rmtree(DIST_DIR)
    DIST_DIR.mkdir(parents=True, exist_ok=True)

    vsix = _build_vsix(version)

    staging = DIST_DIR / f"cognis-build-{version}"
    if staging.exists():
        shutil.rmtree(staging)
    staging.mkdir(parents=True)

    shutil.copy2(vsix, staging / vsix.name)
    shutil.copy2(EXT_DIR / "LICENSE.txt", staging / "LICENSE.txt")
    (staging / "INSTALL.md").write_text(_install_guide(version), encoding="utf-8")

    archive = DIST_DIR / f"cognis-build-{version}.zip"
    if archive.exists():
        archive.unlink()
    with zipfile.ZipFile(archive, "w", zipfile.ZIP_DEFLATED) as zf:
        for path in sorted(staging.rglob("*")):
            if path.is_file():
                zf.write(path, path.relative_to(staging.parent))

    size_kb = archive.stat().st_size / 1024
    print()
    print(f"[OK] Built installer bundle: {archive}  ({size_kb:.1f} KB)")
    print(f"     Staging folder: {staging}")
    print("     This is git-ignored (dist/). Upload the .zip to your distribution channel.")
    return archive


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--clean", action="store_true", help="Remove dist/ before building.")
    args = parser.parse_args()
    build(clean=args.clean)


if __name__ == "__main__":
    main()
