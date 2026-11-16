param([string]$Source, [string]$OutIco, [string]$OutPng)
# Turns an image (webp, png, ...) into a multi-size .ico: the near-black area outside the rounded square becomes
# transparent, sizes 16-128 are stored as 32-bit bitmaps and 256 as PNG.
Add-Type -AssemblyName PresentationCore, WindowsBase, System.Drawing

# --- decode with WIC (handles webp when the Windows codec is there), re-encode as PNG in memory
$dec = [System.Windows.Media.Imaging.BitmapDecoder]::Create([Uri]$Source, [System.Windows.Media.Imaging.BitmapCreateOptions]::PreservePixelFormat, [System.Windows.Media.Imaging.BitmapCacheOption]::OnLoad)
$enc = New-Object System.Windows.Media.Imaging.PngBitmapEncoder
$enc.Frames.Add($dec.Frames[0])
$ms = New-Object System.IO.MemoryStream
$enc.Save($ms); $ms.Position = 0
$src = New-Object System.Drawing.Bitmap($ms)
"decoded $($src.Width) x $($src.Height)"

# --- a square 32bpp ARGB copy
$n = [Math]::Max($src.Width, $src.Height)
$big = New-Object System.Drawing.Bitmap($n, $n, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
$g = [System.Drawing.Graphics]::FromImage($big); $g.DrawImage($src, 0, 0, $n, $n); $g.Dispose()

# --- flood fill the near-black outside from the four corners and make it transparent
$rect = New-Object System.Drawing.Rectangle(0, 0, $n, $n)
$data = $big.LockBits($rect, [System.Drawing.Imaging.ImageLockMode]::ReadWrite, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
$stride = $data.Stride
$bytes = New-Object byte[] ($stride * $n)
[System.Runtime.InteropServices.Marshal]::Copy($data.Scan0, $bytes, 0, $bytes.Length)
$seen = New-Object bool[] ($n * $n)
$stack = New-Object System.Collections.Generic.Stack[int]
foreach ($c in @(0, ($n - 1), (($n - 1) * $n), ($n * $n - 1))) { $stack.Push($c) }
$limit = 45
$cleared = 0
while ($stack.Count -gt 0) {
  $p = $stack.Pop()
  if ($seen[$p]) { continue }
  $seen[$p] = $true
  $x = $p % $n; $y = [int][Math]::Floor($p / $n)
  $o = $y * $stride + $x * 4
  if ([Math]::Max([Math]::Max($bytes[$o], $bytes[$o + 1]), $bytes[$o + 2]) -gt $limit) { continue }
  $bytes[$o + 3] = 0; $cleared++
  if ($x -gt 0) { $stack.Push($p - 1) }
  if ($x -lt $n - 1) { $stack.Push($p + 1) }
  if ($y -gt 0) { $stack.Push($p - $n) }
  if ($y -lt $n - 1) { $stack.Push($p + $n) }
}
[System.Runtime.InteropServices.Marshal]::Copy($bytes, 0, $data.Scan0, $bytes.Length)
$big.UnlockBits($data)
"made $cleared outside pixels transparent"

function Scaled([int]$s) {
  $b = New-Object System.Drawing.Bitmap($s, $s, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
  $gg = [System.Drawing.Graphics]::FromImage($b)
  $gg.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
  $gg.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::HighQuality
  $gg.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
  $gg.CompositingMode = [System.Drawing.Drawing2D.CompositingMode]::SourceCopy
  $gg.DrawImage($big, 0, 0, $s, $s); $gg.Dispose()
  return $b
}

# --- entries
$sizes = 16, 24, 32, 48, 64, 128, 256
$entries = @()
foreach ($s in $sizes) {
  $b = Scaled $s
  if ($s -ge 256) {
    $m = New-Object System.IO.MemoryStream
    $b.Save($m, [System.Drawing.Imaging.ImageFormat]::Png)
    $entries += , @($s, $m.ToArray())
  } else {
    # 32-bit DIB, bottom-up, followed by an all-visible 1-bit mask
    $rect2 = New-Object System.Drawing.Rectangle(0, 0, $s, $s)
    $d = $b.LockBits($rect2, [System.Drawing.Imaging.ImageLockMode]::ReadOnly, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $raw = New-Object byte[] ($d.Stride * $s)
    [System.Runtime.InteropServices.Marshal]::Copy($d.Scan0, $raw, 0, $raw.Length)
    $b.UnlockBits($d)
    $maskStride = [int]([Math]::Ceiling($s / 32.0) * 4)
    $out = New-Object System.IO.MemoryStream
    $w = New-Object System.IO.BinaryWriter($out)
    $w.Write([uint32]40); $w.Write([int32]$s); $w.Write([int32]($s * 2)); $w.Write([uint16]1); $w.Write([uint16]32)
    $w.Write([uint32]0); $w.Write([uint32]($s * $s * 4 + $maskStride * $s)); $w.Write([int32]0); $w.Write([int32]0); $w.Write([uint32]0); $w.Write([uint32]0)
    for ($y = $s - 1; $y -ge 0; $y--) { $w.Write($raw, $y * $d.Stride, $s * 4) }
    $w.Write((New-Object byte[] ($maskStride * $s)))
    $w.Flush()
    $entries += , @($s, $out.ToArray())
  }
  if ($s -eq 256 -and $OutPng) { $b.Save($OutPng, [System.Drawing.Imaging.ImageFormat]::Png) }
}

# --- the .ico file
$file = New-Object System.IO.MemoryStream
$fw = New-Object System.IO.BinaryWriter($file)
$fw.Write([uint16]0); $fw.Write([uint16]1); $fw.Write([uint16]$entries.Count)
$offset = 6 + 16 * $entries.Count
foreach ($e in $entries) {
  $s = $e[0]; $data2 = $e[1]
  $fw.Write([byte]($(if ($s -ge 256) { 0 } else { $s }))); $fw.Write([byte]($(if ($s -ge 256) { 0 } else { $s })))
  $fw.Write([byte]0); $fw.Write([byte]0); $fw.Write([uint16]1); $fw.Write([uint16]32)
  $fw.Write([uint32]$data2.Length); $fw.Write([uint32]$offset)
  $offset += $data2.Length
}
foreach ($e in $entries) { $fw.Write($e[1]) }
$fw.Flush()
[System.IO.File]::WriteAllBytes($OutIco, $file.ToArray())
"wrote $OutIco ($($file.Length) bytes, sizes: $($sizes -join ', '))"
