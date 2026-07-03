<#
build.ps1 — build the native CSAR shared library.

cognis's native core is Rust (see docs/native-core-rust.md §1.2 for why Rust over
C++). This builds the `csar-rs` crate as a cdylib and stages the result at
native/build/csar_native.dll, where the Python ctypes loader
(cognis_retrieval/_native.py) finds it.

Requires the Rust toolchain (rustup). On Windows the `x86_64-pc-windows-gnu`
host is used so no MSVC is required.

Run from the repo root:  pwsh -File native/build.ps1

(The earlier C++ slice under native/csar/ is superseded by the Rust crate and
kept only as a reference port; it is not built here.)
#>
$ErrorActionPreference = "Stop"
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$crate = Join-Path $here "csar-rs"
$outDir = Join-Path $here "build"
New-Item -ItemType Directory -Force -Path $outDir | Out-Null

$cargo = "$env:USERPROFILE\.cargo\bin\cargo.exe"
if (-not (Test-Path $cargo)) {
  $c = Get-Command cargo -ErrorAction SilentlyContinue
  if ($c) { $cargo = $c.Source }
}
if (-not (Test-Path $cargo) -and -not (Get-Command cargo -ErrorAction SilentlyContinue)) {
  Write-Error @"
[build] Rust toolchain (cargo) not found.

Install (user-space, no admin):
  Invoke-WebRequest https://static.rust-lang.org/rustup/dist/x86_64-pc-windows-gnu/rustup-init.exe -OutFile `$env:TEMP\rustup-init.exe
  & `$env:TEMP\rustup-init.exe --default-host x86_64-pc-windows-gnu --default-toolchain stable --profile minimal -y

Then re-run:  pwsh -File native/build.ps1
"@
  exit 1
}

Write-Host "[build] cargo build --release  ($crate)"
# cargo writes normal progress to stderr; under Windows PowerShell with
# ErrorActionPreference=Stop that can surface as a NativeCommandError even on
# success. Gate on $LASTEXITCODE instead.
$prevEAP = $ErrorActionPreference
$ErrorActionPreference = "Continue"
& $cargo build --release --manifest-path (Join-Path $crate "Cargo.toml") 2>&1 | ForEach-Object { Write-Host $_ }
$code = $LASTEXITCODE
$ErrorActionPreference = $prevEAP
if ($code -ne 0) { throw "[build] cargo build failed (exit $code)" }

# Stage the produced cdylib as native/build/csar_native.dll
$produced = Join-Path $crate "target\release\csar_native.dll"
if (-not (Test-Path $produced)) {
  # Fallback names across platforms.
  $alt = Get-ChildItem (Join-Path $crate "target\release") -Filter "*csar_native*" -ErrorAction SilentlyContinue |
    Where-Object { $_.Extension -in ".dll", ".so", ".dylib" } | Select-Object -First 1
  if ($alt) { $produced = $alt.FullName }
}
if (-not (Test-Path $produced)) { throw "[build] could not find produced cdylib in target/release" }

$dest = Join-Path $outDir "csar_native.dll"
Copy-Item $produced $dest -Force
Write-Host "[build] OK -> $dest"
