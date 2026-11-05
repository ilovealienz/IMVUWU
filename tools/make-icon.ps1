# Builds assets/icon.ico (16-256 px) and assets/icon-window.png (64 px) from
# assets/icon.png, a square transparent PNG of the logo (the source design is
# assets/icon.svg). Run after changing the logo:
#   powershell -File tools\make-icon.ps1 -Png assets\icon.png -Ico assets\icon.ico -Window assets\icon-window.png
# -BoldSmall: for a logo with thin lines, sizes up to 32 px (and the window
# icon) use a bolder copy, its outline thickened by drawing the logo, tinted
# to its outline colour, around a circle of offsets.
param([string]$Png, [string]$Ico, [string]$Window, [string]$Preview, [switch]$BoldSmall)
Add-Type -AssemblyName System.Drawing
$src = [Drawing.Bitmap]::FromFile($Png)

# The darkest opaque colour in the logo = its outline colour.
$best = $null; $bestSum = 999
for ($x = 0; $x -lt $src.Width; $x += 2) {
    for ($y = 0; $y -lt $src.Height; $y += 2) {
        $c = $src.GetPixel($x, $y)
        if ($c.A -gt 250 -and ($c.R + $c.G + $c.B) -lt $bestSum) { $best = $c; $bestSum = $c.R + $c.G + $c.B }
    }
}

function Bold([int]$radius) {
    $w = $src.Width; $h = $src.Height
    $b = New-Object Drawing.Bitmap $w, $h, ([Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [Drawing.Graphics]::FromImage($b)
    $g.Clear([Drawing.Color]::Transparent)
    # Tint everything to the outline colour, keeping alpha.
    $m = New-Object Drawing.Imaging.ColorMatrix
    $m.Matrix00 = 0; $m.Matrix11 = 0; $m.Matrix22 = 0; $m.Matrix33 = 1; $m.Matrix44 = 1
    $m.Matrix40 = $best.R / 255.0; $m.Matrix41 = $best.G / 255.0; $m.Matrix42 = $best.B / 255.0
    $attr = New-Object Drawing.Imaging.ImageAttributes
    $attr.SetColorMatrix($m)
    $rect = New-Object Drawing.Rectangle 0, 0, $w, $h
    for ($a = 0; $a -lt 360; $a += 15) {
        foreach ($r in @($radius, [int]($radius / 2))) {
            $dx = [int]([Math]::Cos($a * [Math]::PI / 180) * $r)
            $dy = [int]([Math]::Sin($a * [Math]::PI / 180) * $r)
            $g.DrawImage($src, (New-Object Drawing.Rectangle $dx, $dy, $w, $h), 0, 0, $w, $h, [Drawing.GraphicsUnit]::Pixel, $attr)
        }
    }
    $g.DrawImage($src, $rect)
    $g.Dispose()
    return $b
}

function Shrink($image, [int]$n) {
    $b = New-Object Drawing.Bitmap $n, $n, ([Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [Drawing.Graphics]::FromImage($b)
    $g.InterpolationMode = [Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
    $g.PixelOffsetMode = [Drawing.Drawing2D.PixelOffsetMode]::HighQuality
    $g.CompositingMode = [Drawing.Drawing2D.CompositingMode]::SourceCopy
    $g.DrawImage($image, 0, 0, $n, $n)
    $g.Dispose()
    return $b
}

$sizes = 16, 24, 32, 48, 64, 128, 256
$bold = if ($BoldSmall) { @{ 16 = (Bold 14); 24 = (Bold 10); 32 = (Bold 6) } } else { @{} }
$frames = @()
foreach ($n in $sizes) {
    $source = if ($bold.ContainsKey($n)) { $bold[$n] } else { $src }
    $frames += , (Shrink $source $n)
}

# ICO with PNG frames (Windows Vista+ reads PNG frames at every size).
$out = New-Object IO.MemoryStream
$w = New-Object IO.BinaryWriter $out
$w.Write([uint16]0); $w.Write([uint16]1); $w.Write([uint16]$sizes.Count)
$pngs = $frames | ForEach-Object { $ms = New-Object IO.MemoryStream; $_.Save($ms, [Drawing.Imaging.ImageFormat]::Png); , $ms.ToArray() }
$offset = 6 + 16 * $sizes.Count
for ($k = 0; $k -lt $sizes.Count; $k++) {
    $n = $sizes[$k]; $d = if ($n -ge 256) { 0 } else { $n }
    $w.Write([byte]$d); $w.Write([byte]$d); $w.Write([byte]0); $w.Write([byte]0)
    $w.Write([uint16]1); $w.Write([uint16]32); $w.Write([uint32]$pngs[$k].Length); $w.Write([uint32]$offset)
    $offset += $pngs[$k].Length
}
foreach ($p in $pngs) { $w.Write($p) }
$w.Flush()
[IO.File]::WriteAllBytes($Ico, $out.ToArray())

# The window/taskbar icon: 64 px, slightly bolder (shown at 16-48 px).
if ($Window) { (Shrink $(if ($BoldSmall) { Bold 6 } else { $src }) 64).Save($Window, [Drawing.Imaging.ImageFormat]::Png) }

if ($Preview) {
    $o = New-Object Drawing.Bitmap 520, 180
    $g = [Drawing.Graphics]::FromImage($o)
    $g.Clear([Drawing.Color]::FromArgb(43, 32, 26))
    $g.InterpolationMode = [Drawing.Drawing2D.InterpolationMode]::NearestNeighbor
    $g.PixelOffsetMode = [Drawing.Drawing2D.PixelOffsetMode]::Half
    $x = 5
    for ($k = 0; $k -lt 4; $k++) {
        $n = $sizes[$k]; $z = [int](112 / $n)
        $g.DrawImage($frames[$k], (New-Object Drawing.Rectangle $x, 5, ($n * $z), ($n * $z)))
        $g.DrawImage($frames[$k], $x, 150, $n, $n)
        $x += $n * $z + 12
    }
    $o.Save($Preview)
}
"outline colour: R=$($best.R) G=$($best.G) B=$($best.B); wrote $Ico"
