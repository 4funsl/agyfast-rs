# Build agy-rs and install the Antigravity plugin and the CLI on Windows. Safe to re-run.
#   .\install.ps1              build, then install
#   .\install.ps1 -SkipBuild   install an existing target\release\agy-rs.exe
# If scripts are blocked: powershell -ExecutionPolicy Bypass -File .\install.ps1
param([switch]$SkipBuild)
$ErrorActionPreference = "Stop"
$root = $PSScriptRoot
$pluginDir = Join-Path $env:USERPROFILE ".gemini\config\plugins\agy-rs"
$cliDir = Join-Path $env:USERPROFILE ".local\bin"
$bin = Join-Path $root "target\release\agy-rs.exe"

$bundled = Join-Path $root "agy-rs.exe" # release archive: the prebuilt binary sits next to this script
if (Test-Path $bundled) { $bin = $bundled } elseif (-not $SkipBuild) { & (Join-Path $root "build.ps1") }
if (-not (Test-Path $bin)) { throw "$bin not found; run .\build.ps1 first" }

New-Item -ItemType Directory -Force -Path (Join-Path $pluginDir "bin"), (Join-Path $pluginDir "rules"), $cliDir | Out-Null
Copy-Item (Join-Path $root "plugin\plugin.json") $pluginDir -Force
Copy-Item (Join-Path $root "plugin\rules\AGENTS.md") (Join-Path $pluginDir "rules") -Force

# A running MCP server locks its .exe: move the old one aside instead of overwriting it.
function Install-Exe([string]$dest) {
  if (Test-Path $dest) {
    $old = "$dest.old"
    Remove-Item $old -Force -ErrorAction SilentlyContinue
    Move-Item $dest $old -Force
  }
  Copy-Item $bin $dest -Force
}
$pluginExe = Join-Path $pluginDir "bin\agy-rs.exe"
Install-Exe $pluginExe
Install-Exe (Join-Path $cliDir "agy-rs.exe")

# The bundled mcp_config.json and hooks.json start the binary through sh; on Windows call the .exe directly.
function Write-Json([string]$path, $value) {
  [IO.File]::WriteAllText($path, ($value | ConvertTo-Json -Depth 20), (New-Object Text.UTF8Encoding $false))
}
$mcp = Get-Content (Join-Path $root "plugin\mcp_config.json") -Raw | ConvertFrom-Json
$mcp.mcpServers.rs.command = $pluginExe
$mcp.mcpServers.rs.args = @("mcp")
Write-Json (Join-Path $pluginDir "mcp_config.json") $mcp
$hooks = Get-Content (Join-Path $root "plugin\hooks.json") -Raw | ConvertFrom-Json
$hooks.'agy-rs-snapshot'.SessionStart[0].command = "`"$pluginExe`" hook session-start"
Write-Json (Join-Path $pluginDir "hooks.json") $hooks

Write-Host "Installed plugin: $pluginDir"
Write-Host "Installed CLI:    $cliDir\agy-rs.exe"
if (-not (($env:PATH -split ';') -contains $cliDir)) { Write-Host "Add $cliDir to PATH to call agy-rs from a terminal." }
if (-not (Get-Command sh -ErrorAction SilentlyContinue)) {
  Write-Warning "sh was not found on PATH. run_checks and apply_edits' verify run commands through sh: install Git for Windows and add its bin folder to PATH."
}
Write-Host "Restart Antigravity, then check with: agy plugin list"
