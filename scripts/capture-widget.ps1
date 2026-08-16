# Captures the running widget straight from its own window handle, so the image
# is correct even when something is drawn on top of it.
param([string]$Out)

Add-Type -AssemblyName System.Drawing
Add-Type @"
using System;
using System.Runtime.InteropServices;
public class Cap {
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr l);
  public delegate bool EnumProc(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr hdc, uint flags);
  public struct RECT { public int Left, Top, Right, Bottom; }
}
"@

if (-not $Out) {
    $repo = Split-Path -Parent $PSScriptRoot
    $Out = Join-Path $repo "tests\screenshots\live-widget.png"
}

$proc = Get-Process fluxdock -ErrorAction SilentlyContinue
if (-not $proc) { Write-Output "FluxDock is not running"; exit 1 }
$script:targetPid = $proc.Id
$script:found = [IntPtr]::Zero
$script:rect = New-Object Cap+RECT

$cb = [Cap+EnumProc] {
    param($h, $l)
    $wpid = 0
    [void][Cap]::GetWindowThreadProcessId($h, [ref]$wpid)
    if ($wpid -eq $script:targetPid -and [Cap]::IsWindowVisible($h)) {
        $r = New-Object Cap+RECT
        [void][Cap]::GetWindowRect($h, [ref]$r)
        if (($r.Right - $r.Left) -ge 100 -and ($r.Bottom - $r.Top) -ge 30) {
            $script:found = $h
            $script:rect = $r
        }
    }
    return $true
}
[void][Cap]::EnumWindows($cb, [IntPtr]::Zero)

if ($script:found -eq [IntPtr]::Zero) { Write-Output "Widget window not found"; exit 1 }

$w = $script:rect.Right - $script:rect.Left
$h = $script:rect.Bottom - $script:rect.Top
$bmp = New-Object System.Drawing.Bitmap($w, $h)
$g = [System.Drawing.Graphics]::FromImage($bmp)
# The window is transparent, so a dark ground is painted first.
$g.Clear([System.Drawing.Color]::FromArgb(255, 12, 12, 16))
$hdc = $g.GetHdc()
$ok = [Cap]::PrintWindow($script:found, $hdc, 2)  # PW_RENDERFULLCONTENT
$g.ReleaseHdc($hdc)
$g.Dispose()

$dir = Split-Path -Parent $Out
if (-not (Test-Path $dir)) { New-Item -ItemType Directory -Path $dir | Out-Null }
$bmp.Save($Out, [System.Drawing.Imaging.ImageFormat]::Png)
$bmp.Dispose()

Write-Output "Captured $($w)x$($h) to $Out (PrintWindow=$ok)"
