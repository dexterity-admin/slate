# Renders the Slate icon - an "S" built from pipe: two straight runs joined by
# return bends, with flanged ends - on a deep slate tile, at the standard
# sizes, and packs them into a PNG-compressed .ico. Also writes a 512 px PNG
# preview for the README and website.
param(
    [string]$Out = "$PSScriptRoot\..\res\slate.ico",
    [string]$Preview = "$PSScriptRoot\..\res\slate-512.png"
)
Add-Type -AssemblyName System.Drawing

function New-Round([float]$x, [float]$y, [float]$w, [float]$h, [float]$r) {
    $p = New-Object System.Drawing.Drawing2D.GraphicsPath
    $d = 2 * $r
    $p.AddArc($x, $y, $d, $d, 180, 90)
    $p.AddArc($x + $w - $d, $y, $d, $d, 270, 90)
    $p.AddArc($x + $w - $d, $y + $h - $d, $d, $d, 0, 90)
    $p.AddArc($x, $y + $h - $d, $d, $d, 90, 90)
    $p.CloseFigure()
    $p
}

# The S centreline in a 256-unit box: top run, left bend, middle run,
# right bend, bottom run.
function New-Pipe([float]$k) {
    $p = New-Object System.Drawing.Drawing2D.GraphicsPath
    $p.AddLine(188 * $k, 66 * $k, 112 * $k, 66 * $k)
    $p.AddArc(80 * $k, 66 * $k, 64 * $k, 64 * $k, 270, -180)
    $p.AddLine(112 * $k, 130 * $k, 144 * $k, 130 * $k)
    $p.AddArc(112 * $k, 130 * $k, 64 * $k, 64 * $k, 270, 180)
    $p.AddLine(144 * $k, 194 * $k, 68 * $k, 194 * $k)
    $p
}

function Render([int]$s) {
    $bmp = [System.Drawing.Bitmap]::new($s, $s, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = 'AntiAlias'
    $g.PixelOffsetMode = 'HighQuality'
    $g.Clear([System.Drawing.Color]::Transparent)
    $k = $s / 256.0
    $small = $s -le 24

    # Tile.
    $m = [math]::Max(0.5, 8 * $k)
    $tile = New-Round $m $m ($s - 2 * $m) ($s - 2 * $m) (56 * $k)
    $tr = [System.Drawing.RectangleF]::new(0, 0, $s, $s)
    $bg = [System.Drawing.Drawing2D.LinearGradientBrush]::new($tr, [System.Drawing.Color]::FromArgb(255, 36, 46, 66), [System.Drawing.Color]::FromArgb(255, 14, 19, 30), 60)
    $g.FillPath($bg, $tile)
    if (-not $small) {
        $rim = [System.Drawing.Pen]::new([System.Drawing.Color]::FromArgb(70, 140, 170, 230), [float]([math]::Max(1, 3 * $k)))
        $g.DrawPath($rim, $tile)
    }

    $pipe = New-Pipe $k
    $w = if ($small) { 46 * $k } else { 38 * $k }
    $pr = [System.Drawing.RectangleF]::new(60 * $k, 40 * $k, 140 * $k, 180 * $k)
    # Body: dark edge, then a lighter core, then a specular streak.
    $edge = [System.Drawing.Pen]::new([System.Drawing.Color]::FromArgb(255, 30, 78, 186), [float]$w)
    $edge.LineJoin = 'Round'
    $g.DrawPath($edge, $pipe)
    $coreBrush = [System.Drawing.Drawing2D.LinearGradientBrush]::new($pr, [System.Drawing.Color]::FromArgb(255, 120, 190, 255), [System.Drawing.Color]::FromArgb(255, 58, 120, 236), 90)
    $core = [System.Drawing.Pen]::new($coreBrush, [float]($w * 0.68))
    $core.LineJoin = 'Round'
    $g.DrawPath($core, $pipe)
    if (-not $small) {
        $hi = [System.Drawing.Pen]::new([System.Drawing.Color]::FromArgb(150, 235, 246, 255), [float]($w * 0.16))
        $hi.LineJoin = 'Round'
        $m2 = New-Object System.Drawing.Drawing2D.Matrix
        $m2.Translate(-$w * 0.16, -$w * 0.16)
        $hp = $pipe.Clone(); $hp.Transform($m2)
        $g.DrawPath($hi, $hp)
        # Flanges at both open ends, and weld seams where runs meet bends.
        $fl = [System.Drawing.Drawing2D.LinearGradientBrush]::new($pr, [System.Drawing.Color]::FromArgb(255, 214, 226, 240), [System.Drawing.Color]::FromArgb(255, 128, 146, 170), 0)
        $fw = 13 * $k; $fh = $w * 1.42
        foreach ($fx in @(188, 68)) {
            $fy = if ($fx -eq 188) { 66 } else { 194 }
            $x0 = if ($fx -eq 188) { $fx * $k - $fw * 0.25 } else { $fx * $k - $fw * 0.75 }
            $fp = New-Round $x0 ($fy * $k - $fh / 2) $fw $fh (4 * $k)
            $g.FillPath($fl, $fp)
        }
        $seam = [System.Drawing.Pen]::new([System.Drawing.Color]::FromArgb(120, 20, 50, 120), [float]([math]::Max(1, 3 * $k)))
        foreach ($pt in @(@(112, 66), @(112, 130), @(144, 130), @(144, 194))) {
            $g.DrawLine($seam, $pt[0] * $k, ($pt[1] * $k - $w / 2), $pt[0] * $k, ($pt[1] * $k + $w / 2))
        }
    }
    $g.Dispose()
    $bmp
}

$sizes = 16, 20, 24, 32, 40, 48, 64, 256
$pngs = @()
foreach ($s in $sizes) {
    $bmp = Render $s
    $ms = New-Object System.IO.MemoryStream
    if ($s -ge 256) {
        $bmp.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
    } else {
        # Classic icon entry: BITMAPINFOHEADER, bottom-up BGRA rows, AND mask.
        $w = New-Object System.IO.BinaryWriter($ms)
        $w.Write([uint32]40); $w.Write([int32]$s); $w.Write([int32]($s * 2))
        $w.Write([uint16]1); $w.Write([uint16]32); $w.Write([uint32]0)
        $w.Write([uint32]($s * $s * 4)); $w.Write([int32]0); $w.Write([int32]0); $w.Write([uint32]0); $w.Write([uint32]0)
        for ($y = $s - 1; $y -ge 0; $y--) {
            for ($x = 0; $x -lt $s; $x++) {
                $c = $bmp.GetPixel($x, $y)
                $w.Write([byte]$c.B); $w.Write([byte]$c.G); $w.Write([byte]$c.R); $w.Write([byte]$c.A)
            }
        }
        $maskRow = [int]([math]::Ceiling($s / 32.0) * 4)
        $w.Write((New-Object byte[] ($maskRow * $s)))
        $w.Flush()
    }
    $pngs += , $ms.ToArray()
    $bmp.Dispose()
}
$big = Render 512
$big.Save($Preview, [System.Drawing.Imaging.ImageFormat]::Png)
$big.Dispose()

New-Item -ItemType Directory -Force (Split-Path $Out) | Out-Null
$fs = [System.IO.File]::Create($Out)
$bw = New-Object System.IO.BinaryWriter($fs)
$bw.Write([uint16]0); $bw.Write([uint16]1); $bw.Write([uint16]$sizes.Count)
$offset = 6 + 16 * $sizes.Count
for ($i = 0; $i -lt $sizes.Count; $i++) {
    $s = $sizes[$i]; $len = $pngs[$i].Length
    $dim = if ($s -ge 256) { 0 } else { $s }
    $bw.Write([byte]$dim); $bw.Write([byte]$dim)
    $bw.Write([byte]0); $bw.Write([byte]0); $bw.Write([uint16]1); $bw.Write([uint16]32)
    $bw.Write([uint32]$len); $bw.Write([uint32]$offset)
    $offset += $len
}
foreach ($p in $pngs) { $bw.Write($p) }
$bw.Close()
"wrote $Out ($((Get-Item $Out).Length) bytes) and $Preview"

