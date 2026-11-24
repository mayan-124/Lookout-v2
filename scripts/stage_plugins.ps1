# Copies the built plugins and their manifests next to lookout.exe:
#   target\<profile>\plugins\<folder>\plugin.json + the plugin exe
# Build first: cargo build [--release] -p lookout-clipboard -p lookout-files-tools -p lookout-semantic -p lookout-office-text -p lookout-screenshot -p lookout-fastscan (never with --features review)
param([string]$Profile = "release")
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$target = Join-Path $root "target\$Profile"
$plugins = @(
    @{ Folder = "clipboard"; Exe = "lookout-clipboard.exe" },
    @{ Folder = "files-tools"; Exe = "lookout-files-tools.exe" },
    @{ Folder = "semantic"; Exe = "lookout-semantic.exe" },
    @{ Folder = "office-text"; Exe = "lookout-office-text.exe" },
    @{ Folder = "screenshot"; Exe = "lookout-screenshot.exe" }
)
foreach ($p in $plugins) {
    $dest = Join-Path $target "plugins\$($p.Folder)"
    New-Item -ItemType Directory -Force $dest | Out-Null
    Copy-Item (Join-Path $target $p.Exe) $dest -Force
    Copy-Item (Join-Path $root "plugins\$($p.Folder)\plugin.json") $dest -Force
    Write-Host "staged $($p.Folder) in $dest"
}
# The Disk page's read-only Fast scan helper (started with Windows' permission prompt, never a plugin):
#   target\<profile>\helpers\fastscan\lookout-fastscan.exe
$helperDest = Join-Path $target "helpers\fastscan"
New-Item -ItemType Directory -Force $helperDest | Out-Null
Copy-Item (Join-Path $target "lookout-fastscan.exe") $helperDest -Force
Write-Host "staged fastscan helper in $helperDest"
