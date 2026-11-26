# Build agyfast-rs and install the Antigravity plugin and the CLI on Windows. Safe to re-run.
#   .\install.ps1              build, then install
#   .\install.ps1 -SkipBuild   install an existing target\release\agyfast-rs.exe
# If scripts are blocked: powershell -ExecutionPolicy Bypass -File .\install.ps1
param([switch]$SkipBuild)
$ErrorActionPreference = "Stop"
$root = $PSScriptRoot
$pluginDir = Join-Path $env:USERPROFILE ".gemini\config\plugins\agyfast-rs"
$cliDir = Join-Path $env:USERPROFILE ".local\bin"
$bin = Join-Path $root "target\release\agyfast-rs.exe"

$bundled = Join-Path $root "agyfast-rs.exe" # release archive: the prebuilt binary sits next to this script
if (Test-Path $bundled) { $bin = $bundled } elseif (-not $SkipBuild) { & (Join-Path $root "build.ps1") }
if (-not (Test-Path $bin)) { throw "$bin not found; run .\build.ps1 first" }

New-Item -ItemType Directory -Force -Path (Join-Path $pluginDir "bin"), (Join-Path $pluginDir "rules"), $cliDir | Out-Null
Copy-Item (Join-Path $root "plugin\plugin.json") $pluginDir -Force

# A running MCP server locks its .exe: move the old one aside instead of overwriting it.
function Install-Exe([string]$dest) {
  if (Test-Path $dest) {
    # Leftovers of earlier updates stay locked until Antigravity restarts: delete best-effort, move aside under a unique name.
    Get-ChildItem "$dest.old*" -ErrorAction SilentlyContinue | Remove-Item -Force -ErrorAction SilentlyContinue
    Move-Item $dest "$dest.old.$PID.$(Get-Random)" -Force
  }
  Copy-Item $bin $dest -Force
}
$pluginExe = Join-Path $pluginDir "bin\agyfast-rs.exe"
Install-Exe $pluginExe
Install-Exe (Join-Path $cliDir "agyfast-rs.exe")

# The bundled mcp_config.json and hooks.json start the binary through sh; on Windows call the .exe directly.
function Write-Json([string]$path, $value) {
  [IO.File]::WriteAllText($path, ($value | ConvertTo-Json -Depth 20), (New-Object Text.UTF8Encoding $false))
}
$mcp = Get-Content (Join-Path $root "plugin\mcp_config.json") -Raw | ConvertFrom-Json
$mcp.mcpServers.rs.command = $pluginExe
$mcp.mcpServers.rs.args = @("mcp")
Write-Json (Join-Path $pluginDir "mcp_config.json") $mcp
$hooks = Get-Content (Join-Path $root "plugin\hooks.json") -Raw | ConvertFrom-Json
$hooks.'agyfast-rs-snapshot'.SessionStart[0].command = "`"$pluginExe`" hook session-start"
Write-Json (Join-Path $pluginDir "hooks.json") $hooks

# AGENTS.md rule 7 runs the CLI as a POSIX path; in PowerShell a quoted path needs the call operator.
$rules = [IO.File]::ReadAllText((Join-Path $root "plugin\rules\AGENTS.md"))
$posixCmd = '"$HOME/.gemini/config/plugins/agyfast-rs/bin/agyfast-rs" cli worker-brief'
if (-not $rules.Contains($posixCmd)) { throw "plugin\rules\AGENTS.md no longer contains the worker-brief command; update install.ps1" }
[IO.File]::WriteAllText((Join-Path $pluginDir "rules\AGENTS.md"), $rules.Replace($posixCmd, "& `"$pluginExe`" cli worker-brief"), (New-Object Text.UTF8Encoding $false))

Write-Host "Installed plugin: $pluginDir"
Write-Host "Installed CLI:    $cliDir\agyfast-rs.exe"
if (-not (($env:PATH -split ';') -contains $cliDir)) { Write-Host "Add $cliDir to PATH to call agyfast-rs from a terminal." }
$gitSh = @("usr\bin", "bin") | ForEach-Object { Join-Path $env:ProgramFiles "Git\$_\sh.exe" } | Where-Object { Test-Path $_ }
if (-not (Get-Command sh -ErrorAction SilentlyContinue) -and -not $gitSh) {
  Write-Warning "sh was not found (not on PATH, not in Git for Windows). run_checks and apply_edits' verify will run commands through cmd.exe, where POSIX shell syntax (quotes, &&, pipes to sh tools) may not work: install Git for Windows."
}
Write-Host "Restart Antigravity, then check with: agy plugin list"
