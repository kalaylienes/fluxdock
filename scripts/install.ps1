# One line install for FluxDock:
#
#   irm https://raw.githubusercontent.com/kalaylienes/fluxdock/main/scripts/install.ps1 | iex
#
# Downloads the latest release installer from GitHub and runs it silently.
# Installs per user, no administrator prompt.

$ErrorActionPreference = "Stop"
$repo = "kalaylienes/fluxdock"

Write-Host "Checking the latest FluxDock release..."
$release = Invoke-RestMethod "https://api.github.com/repos/$repo/releases/latest"
$asset = $release.assets | Where-Object { $_.name -like "*setup.exe" } | Select-Object -First 1
if (-not $asset) {
    throw "No installer asset found in the latest release ($($release.tag_name))."
}

$dest = Join-Path $env:TEMP $asset.name
Write-Host "Downloading $($asset.name) ($([math]::Round($asset.size / 1MB, 1)) MB)..."
Invoke-WebRequest -Uri $asset.browser_download_url -OutFile $dest -UseBasicParsing

Write-Host "Installing FluxDock $($release.tag_name)..."
$proc = Start-Process -FilePath $dest -ArgumentList "/S" -PassThru -Wait
Remove-Item $dest -ErrorAction SilentlyContinue

if ($proc.ExitCode -ne 0) {
    throw "Installer exited with code $($proc.ExitCode)."
}

$exe = Join-Path $env:LOCALAPPDATA "FluxDock\fluxdock.exe"
if (Test-Path $exe) {
    Start-Process -FilePath $exe
    Write-Host "FluxDock is installed and running. Look for the tray icon."
} else {
    Write-Host "FluxDock is installed. Start it from the Start Menu."
}
