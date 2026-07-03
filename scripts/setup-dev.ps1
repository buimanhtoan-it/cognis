# First-time developer setup after cloning the cognis repo.
$ErrorActionPreference = "Stop"

$Root = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
Set-Location $Root

Write-Host "Building the Rust workspace"
cargo build --workspace

Write-Host "Building VS Code / Cursor extension"
Push-Location apps/cognis-vscode
npm install
npm run package
Pop-Location

Write-Host ""
Write-Host "Setup complete."
Write-Host "  Install VSIX : apps/cognis-vscode/cognis-vscode-*.vsix"
Write-Host "  Dev checks   : cargo test --workspace"
