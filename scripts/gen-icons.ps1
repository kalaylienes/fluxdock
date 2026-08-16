# Generates the application icons and the four tray badge states.
# Run from the repository root: powershell -File scripts/gen-icons.ps1
Add-Type -AssemblyName System.Drawing

$root = Split-Path -Parent $PSScriptRoot
$icons = Join-Path $root "src-tauri\icons"
if (-not (Test-Path $icons)) { New-Item -ItemType Directory -Path $icons | Out-Null }

function New-RoundedPath([single]$x, [single]$y, [single]$w, [single]$h, [single]$r) {
    $p = New-Object System.Drawing.Drawing2D.GraphicsPath
    $d = $r * 2
    $p.AddArc($x, $y, $d, $d, 180, 90)
    $p.AddArc($x + $w - $d, $y, $d, $d, 270, 90)
    $p.AddArc($x + $w - $d, $y + $h - $d, $d, $d, 0, 90)
    $p.AddArc($x, $y + $h - $d, $d, $d, 90, 90)
    $p.CloseFigure()
    return $p
}

function New-AppIcon([int]$size) {
    $bmp = New-Object System.Drawing.Bitmap($size, $size, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $g.Clear([System.Drawing.Color]::Transparent)

    $s = [single]$size
    $bg = New-RoundedPath 0 0 $s $s ($s * 0.22)
    $bgBrush = New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(255, 27, 27, 27))
    $g.FillPath($bgBrush, $bg)

    $barH = $s * 0.115
    $left = $s * 0.17
    $right = $s * 0.83
    $rows = @(
        @{ y = $s * 0.35; frac = 0.78; c1 = [System.Drawing.Color]::FromArgb(255, 193, 95, 60); c2 = [System.Drawing.Color]::FromArgb(255, 217, 119, 87) },
        @{ y = $s * 0.57; frac = 0.45; c1 = [System.Drawing.Color]::FromArgb(255, 59, 130, 246); c2 = [System.Drawing.Color]::FromArgb(255, 88, 166, 255) }
    )
    foreach ($r in $rows) {
        $trackPath = New-RoundedPath $left ([single]$r.y) ($right - $left) $barH ($barH / 2)
        $trackBrush = New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(40, 255, 255, 255))
        $g.FillPath($trackBrush, $trackPath)
        $trackBrush.Dispose()

        $w = ($right - $left) * $r.frac
        $fillPath = New-RoundedPath $left ([single]$r.y) $w $barH ($barH / 2)
        $rect = New-Object System.Drawing.RectangleF($left, [single]$r.y, [Math]::Max($w, 1), $barH)
        $fillBrush = New-Object System.Drawing.Drawing2D.LinearGradientBrush($rect, $r.c1, $r.c2, 0.0)
        $g.FillPath($fillBrush, $fillPath)
        $fillBrush.Dispose()
        $fillPath.Dispose()
        $trackPath.Dispose()
    }

    $g.Dispose()
    $bgBrush.Dispose()
    $bg.Dispose()
    return $bmp
}

function New-TrayIcon([int]$size, [System.Drawing.Color]$color, [bool]$muted) {
    $bmp = New-Object System.Drawing.Bitmap($size, $size, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $g.Clear([System.Drawing.Color]::Transparent)

    $s = [single]$size
    $pad = $s * 0.12
    $thick = $s * 0.16

    $pen = New-Object System.Drawing.Pen($color, $thick)
    $g.DrawEllipse($pen, $pad + $thick / 2, $pad + $thick / 2, $s - 2 * $pad - $thick, $s - 2 * $pad - $thick)
    $pen.Dispose()

    if (-not $muted) {
        $b = New-Object System.Drawing.SolidBrush($color)
        $d = $s * 0.24
        $g.FillEllipse($b, ($s - $d) / 2, ($s - $d) / 2, $d, $d)
        $b.Dispose()
    }

    $g.Dispose()
    return $bmp
}

function Save-Png([System.Drawing.Bitmap]$bmp, [string]$path) {
    $bmp.Save($path, [System.Drawing.Imaging.ImageFormat]::Png)
}

# Vista and later accept PNG payloads inside an ICO container, so the header is
# written by hand rather than going through Icon.FromHandle.
function Save-Ico([int[]]$sizes, [string]$path) {
    $streams = @()
    foreach ($sz in $sizes) {
        $b = New-AppIcon $sz
        $ms = New-Object System.IO.MemoryStream
        $b.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
        $streams += , @{ size = $sz; bytes = $ms.ToArray() }
        $ms.Dispose()
        $b.Dispose()
    }

    $fs = [System.IO.File]::Create($path)
    $bw = New-Object System.IO.BinaryWriter($fs)
    $bw.Write([UInt16]0)
    $bw.Write([UInt16]1)
    $bw.Write([UInt16]$streams.Count)

    $offset = 6 + 16 * $streams.Count
    foreach ($s in $streams) {
        $dim = if ($s.size -ge 256) { 0 } else { $s.size }
        $bw.Write([Byte]$dim)
        $bw.Write([Byte]$dim)
        $bw.Write([Byte]0)
        $bw.Write([Byte]0)
        $bw.Write([UInt16]1)
        $bw.Write([UInt16]32)
        $bw.Write([UInt32]$s.bytes.Length)
        $bw.Write([UInt32]$offset)
        $offset += $s.bytes.Length
    }
    foreach ($s in $streams) { $bw.Write($s.bytes) }
    $bw.Flush(); $bw.Dispose(); $fs.Dispose()
}

foreach ($pair in @(@(32, "32x32.png"), @(128, "128x128.png"), @(256, "128x128@2x.png"), @(512, "icon.png"))) {
    $b = New-AppIcon $pair[0]
    Save-Png $b (Join-Path $icons $pair[1])
    $b.Dispose()
}
Save-Ico @(16, 32, 48, 64, 128, 256) (Join-Path $icons "icon.ico")

$states = @{
    "tray-green.png" = @{ c = [System.Drawing.Color]::FromArgb(255, 74, 222, 128); m = $false }
    "tray-amber.png" = @{ c = [System.Drawing.Color]::FromArgb(255, 245, 184, 78); m = $false }
    "tray-red.png"   = @{ c = [System.Drawing.Color]::FromArgb(255, 239, 83, 80); m = $false }
    "tray-gray.png"  = @{ c = [System.Drawing.Color]::FromArgb(255, 150, 150, 150); m = $true }
}
foreach ($k in $states.Keys) {
    $b = New-TrayIcon 32 $states[$k].c $states[$k].m
    Save-Png $b (Join-Path $icons $k)
    $b.Dispose()
}

Write-Output "Icons written to $icons"
