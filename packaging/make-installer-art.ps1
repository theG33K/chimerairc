<#
.SYNOPSIS
  Draws the two pictures the installer shows: the banner across the top of its pages (493 x 58) and the picture on
  its first and last pages (493 x 312), both from the logo in docs\logo.png. Writes packaging\installer-banner.bmp
  and packaging\installer-dialog.bmp (24-bit, as the installer wants them). Nothing is installed.
#>
$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.Drawing
$root = Join-Path $PSScriptRoot ".."
$logo = [System.Drawing.Image]::FromFile((Resolve-Path (Join-Path $root "docs\logo.png")).Path)

function New-Canvas([int]$w, [int]$h) {
  $b = New-Object System.Drawing.Bitmap($w, $h, [System.Drawing.Imaging.PixelFormat]::Format24bppRgb)
  $g = [System.Drawing.Graphics]::FromImage($b)
  $g.SmoothingMode = "AntiAlias"; $g.InterpolationMode = "HighQualityBicubic"; $g.TextRenderingHint = "ClearTypeGridFit"; $g.PixelOffsetMode = "HighQuality"
  return @($b, $g)
}

# --- the banner: white, the name on the left, the logo on the right
$b, $g = New-Canvas 493 58
$g.Clear([System.Drawing.Color]::White)
$g.DrawImage($logo, 493 - 52, 5, 48, 48)
$g.Dispose()
$b.Save((Join-Path $PSScriptRoot "installer-banner.bmp"), [System.Drawing.Imaging.ImageFormat]::Bmp); $b.Dispose()

# --- the first and last pages: a dark panel on the left (164 wide) with the logo, white on the right
$b, $g = New-Canvas 493 312
$g.Clear([System.Drawing.Color]::White)
$panel = New-Object System.Drawing.Drawing2D.LinearGradientBrush((New-Object System.Drawing.Rectangle(0, 0, 164, 312)), [System.Drawing.Color]::FromArgb(0x16, 0x1a, 0x26), [System.Drawing.Color]::FromArgb(0x2c, 0x1f, 0x4a), 90)
$g.FillRectangle($panel, 0, 0, 164, 312)
$g.DrawImage($logo, 14, 40, 136, 136)
$font = New-Object System.Drawing.Font("Segoe UI", 15, [System.Drawing.FontStyle]::Bold)
$small = New-Object System.Drawing.Font("Segoe UI", 8.5)
$fmt = New-Object System.Drawing.StringFormat; $fmt.Alignment = "Center"
$g.DrawString("ChimeraIRC", $font, [System.Drawing.Brushes]::White, (New-Object System.Drawing.RectangleF(0, 188, 164, 30)), $fmt)
$g.DrawString("IRC for Windows", $small, (New-Object System.Drawing.SolidBrush([System.Drawing.Color]::FromArgb(0xb8, 0xc0, 0xd4))), (New-Object System.Drawing.RectangleF(0, 218, 164, 20)), $fmt)
$g.Dispose()
$b.Save((Join-Path $PSScriptRoot "installer-dialog.bmp"), [System.Drawing.Imaging.ImageFormat]::Bmp); $b.Dispose()
$logo.Dispose()
"wrote installer-banner.bmp and installer-dialog.bmp in $PSScriptRoot"
