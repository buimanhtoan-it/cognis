# Index eval fixture repos into .cognis/uckg.db for nightly eval / local validation.
$ErrorActionPreference = "Stop"

$Root = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
Set-Location $Root

if (-not $env:COGNIS_DB_PATH) {
    $env:COGNIS_DB_PATH = Join-Path $Root ".cognis\uckg.db"
}

python -m cognis.cli.main init

# Skip embeddings when offline or Hugging Face auth fails (lexical/structural index only).
# Example: $env:SKIP_EMBEDDINGS = "1"
if ($env:SKIP_EMBEDDINGS -eq "1") {
    Write-Host "SKIP_EMBEDDINGS=1 (lexical/structural index only)"
}
foreach ($repo in @("mini-ts-app", "mini-py-svc", "mini-go-svc")) {
    Write-Host "Indexing tests/fixtures/repos/$repo ..."
    if ($env:SKIP_EMBEDDINGS -eq "1") {
        python -m cognis.cli.main index --full --skip-embeddings "tests/fixtures/repos/$repo"
    } else {
        python -m cognis.cli.main index --full "tests/fixtures/repos/$repo"
    }
}

python -m cognis.cli.main health --json
