# Registers a Windows scheduled task that keeps FluxDock running.
#
#   powershell -ExecutionPolicy Bypass -File scripts/install-watchdog.ps1
#   powershell -ExecutionPolicy Bypass -File scripts/install-watchdog.ps1 -Remove
#
# The task starts FluxDock at sign in and checks every minute afterwards. If the
# process is gone it starts it again. Task Scheduler is told never to run a
# second copy, and FluxDock enforces single instance itself, so the check is a
# no-op while it is already running.
#
# Quitting from the tray menu writes a marker that this task honours, so a
# deliberate exit stays closed. Ending the process from Task Manager does not
# write that marker and the widget comes back within a minute, which is the
# point: an unexpected death should not need a human.

param([switch]$Remove)

$ErrorActionPreference = "Stop"
$taskName = "FluxDock Watchdog"

if ($Remove) {
    if (Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue) {
        Unregister-ScheduledTask -TaskName $taskName -Confirm:$false
        Write-Host "Watchdog removed."
    } else {
        Write-Host "Watchdog was not registered."
    }
    return
}

$exe = Join-Path $env:LOCALAPPDATA "FluxDock\fluxdock.exe"
if (-not (Test-Path $exe)) {
    $repoExe = Join-Path (Split-Path -Parent $PSScriptRoot) "src-tauri\target\release\fluxdock.exe"
    if (Test-Path $repoExe) {
        $exe = $repoExe
    } else {
        throw "fluxdock.exe not found. Install FluxDock first, or build it."
    }
}

$marker = Join-Path $env:APPDATA "FluxDock\stay-closed"

# Single quotes inside so the command is evaluated when the task runs.
$command = @"
if (-not (Test-Path '$marker')) { if (-not (Get-Process fluxdock -ErrorAction SilentlyContinue)) { Start-Process '$exe' } }
"@
$encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($command))

$action = New-ScheduledTaskAction -Execute "powershell.exe" `
    -Argument "-NoProfile -WindowStyle Hidden -EncodedCommand $encoded"

$atLogon = New-ScheduledTaskTrigger -AtLogOn -User $env:USERNAME

# Omitting RepetitionDuration is what Task Scheduler reads as "indefinitely".
# Passing TimeSpan::MaxValue is rejected as out of range.
$repeat = New-ScheduledTaskTrigger -Once -At (Get-Date).AddMinutes(1) `
    -RepetitionInterval (New-TimeSpan -Minutes 1)

$settings = New-ScheduledTaskSettingsSet `
    -AllowStartIfOnBatteries `
    -DontStopIfGoingOnBatteries `
    -StartWhenAvailable `
    -MultipleInstances IgnoreNew `
    -ExecutionTimeLimit ([TimeSpan]::Zero)

Register-ScheduledTask -TaskName $taskName `
    -Action $action `
    -Trigger @($atLogon, $repeat) `
    -Settings $settings `
    -Description "Restarts FluxDock if it stops unexpectedly." `
    -Force | Out-Null

Write-Host "Watchdog registered as '$taskName'."
Write-Host "  Watching: $exe"
Write-Host "  Checks every minute, and at sign in."
Write-Host "  Quit from the tray menu to keep it closed; remove with -Remove."
