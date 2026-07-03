# One-shot cognis setup: init + index + health (see docs/production-flow.md).
$ErrorActionPreference = "Stop"

$Root = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
Set-Location $Root

$Target = if ($args.Count -gt 0) { $args[0] } else { "." }

if ($env:SKIP_EMBEDDINGS -eq "1") {
    cargo run --release -p cognis -- bootstrap $Target --skip-embeddings
} else {
    cargo run --release -p cognis -- bootstrap $Target
}
