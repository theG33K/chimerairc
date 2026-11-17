<#
.SYNOPSIS
  Draws the toolbar icons for the dark theme (light strokes and bright accents, anti-aliased with an alpha channel)
  and writes them as 32-bit bitmap strips: crates\chimerairc-gui\assets\toolbar_dark_16.bmp, _32.bmp and _48.bmp.

.DESCRIPTION
  Each strip has 18 slots, in the same order as HydraIRC's toolbar.bmp that the light theme still uses, so a button
  keeps its slot number. Only the slots the toolbar uses are drawn (0-7, 9 and 15). Run it from anywhere; it needs
  Windows PowerShell or PowerShell 7 on Windows (System.Drawing). Nothing is installed.
#>
$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.Drawing
$out = Join-Path $PSScriptRoot "..\crates\chimerairc-gui\assets"

function Col([string]$hex, [int]$alpha = 255) { $c = [System.Drawing.ColorTranslator]::FromHtml($hex); [System.Drawing.Color]::FromArgb($alpha, $c.R, $c.G, $c.B) }
function Use-Palette([string]$theme) {
  if ($theme -eq "dark") {
    $script:LINE = Col "#d6dce6"; $script:FILL = Col "#343a44"; $script:BAR = Col "#4f6f9f"; $script:BLUE = Col "#5aa9ff"; $script:GREEN = Col "#4fd16a"
    $script:RED = Col "#ff6b6b"; $script:AMBER = Col "#f2b84b"; $script:WHITE = Col "#ffffff"; $script:GREY = Col "#8b94a3"
  } else {
    $script:LINE = Col "#46505f"; $script:FILL = Col "#eef2f8"; $script:BAR = Col "#6d97d6"; $script:BLUE = Col "#2f7de0"; $script:GREEN = Col "#2fa84a"
    $script:RED = Col "#e04848"; $script:AMBER = Col "#e09a1a"; $script:WHITE = Col "#ffffff"; $script:GREY = Col "#7d8795"
  }
}

function Pts($pts, $s) { $a = @(); for ($i = 0; $i -lt $pts.Count; $i += 2) { $a += New-Object System.Drawing.PointF(([single]($pts[$i] * $s)), ([single]($pts[$i + 1] * $s))) }; return ,([System.Drawing.PointF[]]$a) }
function RRect([single]$x, [single]$y, [single]$w, [single]$h, [single]$r) {
  $p = New-Object System.Drawing.Drawing2D.GraphicsPath; $d = $r * 2
  $p.AddArc($x, $y, $d, $d, 180, 90); $p.AddArc($x + $w - $d, $y, $d, $d, 270, 90)
  $p.AddArc($x + $w - $d, $y + $h - $d, $d, $d, 0, 90); $p.AddArc($x, $y + $h - $d, $d, $d, 90, 90); $p.CloseFigure(); return $p
}

# every shape is described on a 16 x 16 grid and scaled to the strip's size
function Draw-Icon($g, [int]$slot, [single]$s) {
  $ox = $slot * 16 * $s
  $g.ResetTransform(); $g.TranslateTransform($ox, 0)
  $pen = { param($c, [single]$w = 1.0) New-Object System.Drawing.Pen($c, [single]($w * $s)) }
  $brush = { param($c) New-Object System.Drawing.SolidBrush($c) }
  $stroke = & $pen $LINE 1.0
  $rr = { param($x, $y, $w, $h, $r) RRect ($x * $s) ($y * $s) ($w * $s) ($h * $s) ($r * $s) }
  $monitor = {
    $p = & $rr 1.5 1.5 11 8 1.2; $g.FillPath((& $brush $FILL), $p); $g.DrawPath($stroke, $p)
    $g.DrawLine($stroke, [single](7 * $s), [single](9.5 * $s), [single](7 * $s), [single](12 * $s))
    $g.DrawLine($stroke, [single](4.5 * $s), [single](12.5 * $s), [single](9.5 * $s), [single](12.5 * $s))
  }
  $badge = { param($fill, $kind)
    $g.FillEllipse((& $brush $fill), [single](8.5 * $s), [single](8 * $s), [single](7 * $s), [single](7 * $s))
    $w = & $pen $WHITE 1.3
    $cx = 12 * $s; $cy = 11.5 * $s
    if ($kind -eq "plus") { $g.DrawLine($w, [single]($cx - 2 * $s), [single]$cy, [single]($cx + 2 * $s), [single]$cy); $g.DrawLine($w, [single]$cx, [single]($cy - 2 * $s), [single]$cx, [single]($cy + 2 * $s)) }
    elseif ($kind -eq "x") { $g.DrawLine($w, [single]($cx - 1.7 * $s), [single]($cy - 1.7 * $s), [single]($cx + 1.7 * $s), [single]($cy + 1.7 * $s)); $g.DrawLine($w, [single]($cx + 1.7 * $s), [single]($cy - 1.7 * $s), [single]($cx - 1.7 * $s), [single]($cy + 1.7 * $s)) }
    else { $g.FillPolygon((& $brush $WHITE), (Pts @(11, 9.6, 14, 11.5, 11, 13.4) $s)) }   # a play triangle: connect
  }
  $window = { param($x, $y, $w, $h)
    $p = & $rr $x $y $w $h 0.8; $g.FillPath((& $brush $FILL), $p)
    $g.FillRectangle((& $brush $BAR), [single](($x + 0.5) * $s), [single](($y + 0.5) * $s), [single](($w - 1) * $s), [single](2.2 * $s))
    $g.DrawPath($stroke, $p)
  }
  switch ($slot) {
    0 { & $monitor; & $badge $GREEN "plus" }
    1 { & $monitor; & $badge $BLUE "play" }
    2 { & $monitor; & $badge $RED "x" }
    3 { & $window 1 1.5 10 8; & $window 5 5.5 10 8.5 }                         # cascade
    4 { & $window 1.5 1.5 13 13; $g.DrawLine($stroke, [single](8 * $s), [single](4 * $s), [single](8 * $s), [single](14.5 * $s)) }   # tile, side by side
    5 { & $window 1.5 1.5 13 13; $g.DrawLine($stroke, [single](1.5 * $s), [single](9 * $s), [single](14.5 * $s), [single](9 * $s)) }  # tile, one over the other
    6 { & $window 2 1.5 12 9; $g.FillRectangle((& $brush $AMBER), [single](3 * $s), [single](12.2 * $s), [single](7 * $s), [single](2 * $s)) }  # minimize all
    7 {                                                                          # people
      $g.FillEllipse((& $brush $GREY), [single](9.2 * $s), [single](3 * $s), [single](4.6 * $s), [single](4.6 * $s))
      $p2 = & $rr 8.2 8.6 7.2 6 3; $g.FillPath((& $brush $GREY), $p2)
      $g.FillEllipse((& $brush $BLUE), [single](2.6 * $s), [single](2 * $s), [single](5 * $s), [single](5 * $s))
      $p1 = & $rr 1 8 8.4 7 3.2; $g.FillPath((& $brush $BLUE), $p1); $g.DrawPath((& $pen $FILL 0.8), $p1)
    }
    9 {                                                                          # a log: page with lines and a red dot
      $page = Pts @(3, 1.5, 10, 1.5, 13, 4.5, 13, 14.5, 3, 14.5) $s
      $g.FillPolygon((& $brush $FILL), $page); $g.DrawPolygon($stroke, $page)
      $g.DrawLine($stroke, [single](10 * $s), [single](1.5 * $s), [single](10 * $s), [single](4.5 * $s)); $g.DrawLine($stroke, [single](10 * $s), [single](4.5 * $s), [single](13 * $s), [single](4.5 * $s))
      $thin = & $pen $GREY 0.9
      foreach ($y in 6.2, 8.4) { $g.DrawLine($thin, [single](5 * $s), [single]($y * $s), [single](10.5 * $s), [single]($y * $s)) }
      $g.FillEllipse((& $brush $RED), [single](7.6 * $s), [single](9.8 * $s), [single](5 * $s), [single](5 * $s))
    }
    15 {                                                                         # transfers: one arrow down, one up
      $g.FillPolygon((& $brush $GREEN), (Pts @(1.5, 8, 5, 8, 5, 2, 7, 2, 7, 8, 10.5, 8, 6, 13.5) $s))
      $g.FillPolygon((& $brush $AMBER), (Pts @(9.5, 8, 11.5, 8, 11.5, 14.5, 13.5, 14.5, 13.5, 8, 15.5, 8, 12.5, 2.5) $s))
    }
  }
  $g.ResetTransform()
}

foreach ($theme in "dark", "light") { Use-Palette $theme
foreach ($size in 16, 32, 48) {
  $s = $size / 16.0
  $bmp = New-Object System.Drawing.Bitmap(([int](18 * $size)), $size, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
  $g = [System.Drawing.Graphics]::FromImage($bmp)
  $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
  $g.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
  $g.Clear([System.Drawing.Color]::Transparent)
  foreach ($slot in 0, 1, 2, 3, 4, 5, 6, 7, 9, 15) { Draw-Icon $g $slot $s }
  $g.Dispose()
  $path = Join-Path $out "toolbar_${theme}_$size.bmp"
  $bmp.Save($path, [System.Drawing.Imaging.ImageFormat]::Bmp)
  $bmp.Dispose()
  # the image list wants premultiplied alpha: colour = colour x alpha / 255 (the file holds BGRA after a 54 byte header)
  $bytes = [IO.File]::ReadAllBytes($path)
  $offset = [BitConverter]::ToUInt32($bytes, 10)
  for ($i = $offset; $i + 3 -lt $bytes.Length; $i += 4) {
    $a = $bytes[$i + 3]
    if ($a -lt 255) { $bytes[$i] = [byte]([int]$bytes[$i] * $a / 255); $bytes[$i + 1] = [byte]([int]$bytes[$i + 1] * $a / 255); $bytes[$i + 2] = [byte]([int]$bytes[$i + 2] * $a / 255) }
  }
  [IO.File]::WriteAllBytes($path, $bytes)
  "wrote $path"
}
}
