# First-time developer setup after cloning the cognis repo.
$ErrorActionPreference = "Stop"

$Root = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
Set-Location $Root

$Py = if (Test-Path ".\.venv\Scripts\python.exe") {
    ".\.venv\Scripts\python.exe"
} else {
    "python"
}

if (-not (Test-Path ".\.venv")) {
    Write-Host "Creating virtual environment in .venv"
    & python -m venv .venv
    $Py = ".\.venv\Scripts\python.exe"
}

Write-Host "Installing Python backend and dev tools"
& $Py -m pip install -e ".[indexer,embed-local,vector,tokenizers,mcp]"
& $Py -m pip install --group dev

Write-Host "Refreshing branding assets (best effort)"
& $Py scripts/prepare-branding.py

Write-Host "Building VS Code / Cursor extension"
& $Py scripts/setup_extension.py --package

Write-Host ""
Write-Host "Setup complete."
Write-Host "  Activate venv : .\.venv\Scripts\Activate.ps1"
Write-Host "  Install VSIX  : apps/cognis-vscode/cognis-vscode-*.vsix"
Write-Host "  Dev checks    : invoke test  or  make test"
