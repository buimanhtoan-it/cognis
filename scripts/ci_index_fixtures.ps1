# Index eval fixture repos into .cognis/uckg.db for nightly eval / local validation.
$ErrorActionPreference = "Stop"

$Root = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
Set-Location $Root

if (-not $env:COGNIS_DB_PATH) {
    $env:COGNIS_DB_PATH = Join-Path $Root ".cognis\uckg.db"
}

cargo run --release -p cognis -- init

# Skip embeddings when offline or Hugging Face auth fails (lexical/structural index only).
# Example: $env:SKIP_EMBEDDINGS = "1"
if ($env:SKIP_EMBEDDINGS -eq "1") {
    Write-Host "SKIP_EMBEDDINGS=1 (lexical/structural index only)"
}
foreach ($repo in @("mini-ts-app", "mini-py-svc", "mini-go-svc")) {
    $repoPath = "tests/fixtures/repos/$repo"
    if (-not (Test-Path $repoPath)) {
        Write-Host "Skipping $repoPath (not present)"
        continue
    }
    Write-Host "Indexing $repoPath ..."
    if ($env:SKIP_EMBEDDINGS -eq "1") {
        cargo run --release -p cognis -- index --full --skip-embeddings $repoPath
    } else {
        cargo run --release -p cognis -- index --full $repoPath
    }
}

cargo run --release -p cognis -- health --json
