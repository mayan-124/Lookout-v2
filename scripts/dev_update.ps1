# One command to rebuild and reinstall Lookout for the current user (dev machines).
#   powershell -File scripts\dev_update.ps1
# Stops Lookout and its helpers, builds release, stages plugins + fast-scan helper,
# copies the files over the install folder (settings, index and data are not touched), restarts.
$ErrorActionPreference = "Stop"
$sw = [Diagnostics.Stopwatch]::StartNew()
$root = Split-Path -Parent $PSScriptRoot
$installDir = Join-Path $env:LOCALAPPDATA "Lookout"
$step = "start"
function Run-Native([string]$what) { if ($LASTEXITCODE -ne 0) { throw "$what failed (exit $LASTEXITCODE)" } }
try {
    Set-Location $root

    $step = "close running Lookout"
    Get-Process | Where-Object { $_.Name -eq "lookout" -or $_.Name -like "lookout-*" } | Stop-Process -Force -ErrorAction SilentlyContinue
    Start-Sleep -Milliseconds 700

    $step = "build (release, incremental)"
    $env:CARGO_INCREMENTAL = "1"
    cargo build --release -p lookout-clipboard -p lookout-files-tools -p lookout-semantic -p lookout-office-text -p lookout-screenshot -p lookout-fastscan
    Run-Native "cargo build plugins"
    if (-not (Test-Path (Join-Path $root "ui\node_modules"))) {
        npm --prefix ui ci
        Run-Native "npm ci"
    }
    npm --prefix ui run tauri build -- --no-bundle
    Run-Native "tauri build"

    $step = "stage plugins and fast-scan helper"
    & (Join-Path $PSScriptRoot "stage_plugins.ps1") release
    if (-not $?) { throw "stage_plugins.ps1 failed" }

    $step = "install into $installDir"
    New-Item -ItemType Directory -Force $installDir | Out-Null
    $rel = Join-Path $root "target\release"
    Copy-Item (Join-Path $rel "lookout.exe") $installDir -Force
    foreach ($d in "plugins", "helpers") {
        Copy-Item (Join-Path $rel $d) $installDir -Recurse -Force
    }

    $step = "restart Lookout"
    Start-Process (Join-Path $installDir "lookout.exe")

    $ver = (Get-Content (Join-Path $root "ui\src-tauri\tauri.conf.json") -Raw | ConvertFrom-Json).version
    $commit = (git rev-parse --short HEAD)
    Write-Host ("Lookout {0} ({1}) installed in {2:n0}s" -f $ver, $commit, $sw.Elapsed.TotalSeconds)
} catch {
    Write-Host "FAILED at step '$step': $($_.Exception.Message)" -ForegroundColor Red
    exit 1
}
