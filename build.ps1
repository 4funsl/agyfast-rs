# Build agy-rs on Windows.
#   .\build.ps1          release build -> target\release\agy-rs.exe
#   .\build.ps1 -Check   also run the tests and clippy
param([switch]$Check)
$ErrorActionPreference = "Stop"
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
  throw "cargo not found. Install Rust from https://rustup.rs (it also needs the MSVC C++ build tools), then reopen the terminal."
}
Push-Location $PSScriptRoot
try {
  cargo build --release
  if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
  if ($Check) {
    cargo test
    if ($LASTEXITCODE -ne 0) { throw "cargo test failed" }
    cargo clippy --all-targets
    if ($LASTEXITCODE -ne 0) { throw "cargo clippy failed" }
  }
} finally { Pop-Location }
Write-Host "Built: $(Join-Path $PSScriptRoot 'target\release\agy-rs.exe')"
