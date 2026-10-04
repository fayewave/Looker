<#
.SYNOPSIS
    Regenerates every MSIX logo asset (all scales + targetsizes), the file-type icon, and AppIcon.ico
    from the brand mark SVG, and copies cleaned brand SVGs (mark, wordmark, full logo) into Assets/Brand for
    in-app vector use (SvgImageSource). Run from the repo root:

        pwsh scripts/Generate-Logos.ps1

    Assets land in native/assets; the package layout copies every PNG there, so new files need no other edit.

.DESCRIPTION
    The mark is a single <path> exported from Affinity (brand/slices/looker_v*_icon.svg). We parse its
    path data and the chain of ancestor matrix() transforms, build a GDI+ GraphicsPath in the SVG's
    viewBox space and fill it with the SVG's fill colour, 4x supersampled. Supported path commands:
    M L H V C Z (absolute and relative) — everything Affinity emits for flattened shapes.

    To adopt a new mark: export it from Affinity as a plain SVG with a single path, drop it in
    brand/slices, and point -MarkSvg at it (or update the default below).
#>
[CmdletBinding()]
param(
    [string]$OutDir = (Join-Path $PSScriptRoot '..\native\assets'),
    [string]$MarkSvg = (Join-Path $PSScriptRoot '..\brand\slices\looker_v3_icon.svg'),
    [string]$WordmarkSvg = (Join-Path $PSScriptRoot '..\brand\slices\looker_v3_wordmark.svg'),
    [string]$FullSvg = (Join-Path $PSScriptRoot '..\brand\slices\looker_v3_full.svg')
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing

$OutDir = [IO.Path]::GetFullPath($OutDir)
New-Item -ItemType Directory -Force $OutDir | Out-Null
New-Item -ItemType Directory -Force (Join-Path $OutDir 'Brand') | Out-Null

# --- Parse the mark SVG ------------------------------------------------------------------------------
$svgText = Get-Content -Raw $MarkSvg
if ($svgText -notmatch 'viewBox="([\d.\-]+)\s+([\d.\-]+)\s+([\d.\-]+)\s+([\d.\-]+)"') { throw "No viewBox in $MarkSvg" }
$vbX = [double]$Matches[1]; $vbY = [double]$Matches[2]; $vbW = [double]$Matches[3]; $vbH = [double]$Matches[4]

$pathMatches = [regex]::Matches($svgText, '<path\s+(?:[^>]*?\s)?d="([^"]+)"[^>]*>')
if ($pathMatches.Count -ne 1) { throw "Expected exactly one <path> in $MarkSvg, found $($pathMatches.Count)" }
$pathData = $pathMatches[0].Groups[1].Value
$fillColor = [System.Drawing.Color]::FromArgb(255, 255, 2, 0)
if ($pathMatches[0].Value -match 'fill:\s*rgb\((\d+),\s*(\d+),\s*(\d+)\)') {
    $fillColor = [System.Drawing.Color]::FromArgb(255, [int]$Matches[1], [int]$Matches[2], [int]$Matches[3])
}

# Compose every ancestor matrix() in document order (outermost first). The export nests one <g> per
# transform around the single path, so this equals the path's cumulative transform.
$ctm = New-Object System.Drawing.Drawing2D.Matrix
foreach ($m in [regex]::Matches($svgText, 'transform="matrix\(([^)]+)\)"')) {
    $v = $m.Groups[1].Value -split '[,\s]+' | ForEach-Object { [float]$_ }
    $local = New-Object System.Drawing.Drawing2D.Matrix($v[0], $v[1], $v[2], $v[3], $v[4], $v[5])
    $ctm.Multiply($local, [System.Drawing.Drawing2D.MatrixOrder]::Prepend)
}

function ConvertTo-GraphicsPath([string]$d) {
    $path = New-Object System.Drawing.Drawing2D.GraphicsPath
    $path.FillMode = [System.Drawing.Drawing2D.FillMode]::Alternate  # SVG default / Affinity evenodd
    $tokens = [regex]::Matches($d, '[MLHVCZmlhvcz]|-?\d*\.?\d+(?:e-?\d+)?') | ForEach-Object { $_.Value }
    $i = 0; $cmd = ''
    $cx = 0.0; $cy = 0.0; $sx = 0.0; $sy = 0.0
    function IsCmd($t) { $t -match '^[A-Za-z]$' }
    while ($i -lt $tokens.Count) {
        if (IsCmd $tokens[$i]) { $cmd = $tokens[$i]; $i++ }
        $rel = $cmd -cmatch '[a-z]'
        switch ($cmd.ToUpper()) {
            'M' {
                $x = [double]$tokens[$i]; $y = [double]$tokens[$i + 1]; $i += 2
                if ($rel) { $x += $cx; $y += $cy }
                $path.StartFigure()
                $cx = $x; $cy = $y; $sx = $x; $sy = $y
                $cmd = if ($rel) { 'l' } else { 'L' }  # implicit lineto after moveto
            }
            'L' {
                $x = [double]$tokens[$i]; $y = [double]$tokens[$i + 1]; $i += 2
                if ($rel) { $x += $cx; $y += $cy }
                $path.AddLine([float]$cx, [float]$cy, [float]$x, [float]$y)
                $cx = $x; $cy = $y
            }
            'H' {
                $x = [double]$tokens[$i]; $i++
                if ($rel) { $x += $cx }
                $path.AddLine([float]$cx, [float]$cy, [float]$x, [float]$cy)
                $cx = $x
            }
            'V' {
                $y = [double]$tokens[$i]; $i++
                if ($rel) { $y += $cy }
                $path.AddLine([float]$cx, [float]$cy, [float]$cx, [float]$y)
                $cy = $y
            }
            'C' {
                $x1 = [double]$tokens[$i]; $y1 = [double]$tokens[$i + 1]
                $x2 = [double]$tokens[$i + 2]; $y2 = [double]$tokens[$i + 3]
                $x = [double]$tokens[$i + 4]; $y = [double]$tokens[$i + 5]; $i += 6
                if ($rel) { $x1 += $cx; $y1 += $cy; $x2 += $cx; $y2 += $cy; $x += $cx; $y += $cy }
                $path.AddBezier([float]$cx, [float]$cy, [float]$x1, [float]$y1, [float]$x2, [float]$y2, [float]$x, [float]$y)
                $cx = $x; $cy = $y
            }
            'Z' {
                $path.CloseFigure()
                $cx = $sx; $cy = $sy
            }
            default { throw "Unsupported SVG path command '$cmd'" }
        }
    }
    return $path
}

$markPath = ConvertTo-GraphicsPath $pathData
$markPath.Transform($ctm)
$markBounds = $markPath.GetBounds()
Write-Host ("Mark: {0}  viewBox {1}x{2}  path bounds {3:0.#},{4:0.#} {5:0.#}x{6:0.#}  fill #{7:X2}{8:X2}{9:X2}" -f `
    (Split-Path -Leaf $MarkSvg), $vbW, $vbH, $markBounds.X, $markBounds.Y, $markBounds.Width, $markBounds.Height, $fillColor.R, $fillColor.G, $fillColor.B)

# --- Draw the mark into a square of $size px at ($x,$y) ----------------------------------------------
function Draw-Icon([System.Drawing.Graphics]$g, [double]$x, [double]$y, [double]$size) {
    $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $g.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
    $g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic

    $state = $g.Save()
    $g.TranslateTransform([float]$x, [float]$y)
    $s = $size / [Math]::Max($vbW, $vbH)
    $g.ScaleTransform([float]$s, [float]$s)
    $g.TranslateTransform([float](-$vbX), [float](-$vbY))
    $brush = New-Object System.Drawing.SolidBrush($fillColor)
    $g.FillPath($brush, $markPath)
    $brush.Dispose()
    $g.Restore($state)
}

function New-Canvas([int]$w, [int]$h) {
    $bmp = New-Object System.Drawing.Bitmap($w, $h, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.Clear([System.Drawing.Color]::Transparent)
    return @($bmp, $g)
}

# Supersample 4x then downscale: GDI+ AA alone is coarse at 16-32 px.
function Render-Icon([int]$w, [int]$h, [double]$iconSize, [string]$path) {
    $ss = 4
    $bmp, $g = New-Canvas ($w * $ss) ($h * $ss)
    Draw-Icon $g (($w - $iconSize) / 2 * $ss) (($h - $iconSize) / 2 * $ss) ($iconSize * $ss)
    $g.Dispose()

    $out, $go = New-Canvas $w $h
    $go.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
    $go.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
    $go.CompositingQuality = [System.Drawing.Drawing2D.CompositingQuality]::HighQuality
    $go.DrawImage($bmp, 0, 0, $w, $h)
    $go.Dispose(); $bmp.Dispose()
    $out.Save($path, [System.Drawing.Imaging.ImageFormat]::Png)
    $out.Dispose()
    Write-Host ("  {0,-56} {1}x{2}" -f (Split-Path -Leaf $path), $w, $h)
}

$scales = @(100, 125, 150, 200, 400)
function S([int]$base, [int]$scale) { [int][Math]::Round($base * $scale / 100.0) }

Write-Host "Tiles / logos:"
foreach ($sc in $scales) {
    # The mark is a full-bleed rounded plate, so it fills the small square tiles; the larger tiles and the
    # splash get breathing room around it.
    Render-Icon (S 44 $sc)  (S 44 $sc)  (S 44 $sc)              (Join-Path $OutDir "Square44x44Logo.scale-$sc.png")
    Render-Icon (S 150 $sc) (S 150 $sc) (S 150 $sc * 0.66)      (Join-Path $OutDir "Square150x150Logo.scale-$sc.png")
    Render-Icon (S 310 $sc) (S 150 $sc) (S 150 $sc * 0.66)      (Join-Path $OutDir "Wide310x150Logo.scale-$sc.png")
    Render-Icon (S 50 $sc)  (S 50 $sc)  (S 50 $sc)              (Join-Path $OutDir "StoreLogo.scale-$sc.png")
    Render-Icon (S 620 $sc) (S 300 $sc) (S 300 $sc * 0.5)       (Join-Path $OutDir "SplashScreen.scale-$sc.png")
    Render-Icon (S 24 $sc)  (S 24 $sc)  (S 24 $sc)              (Join-Path $OutDir "LockScreenLogo.scale-$sc.png")
}

Write-Host "Taskbar / shell targetsizes:"
$targets = @(16, 20, 24, 32, 40, 48, 64, 256)
foreach ($t in $targets) {
    # Plated (Start/taskbar composes on the accent plate), unplated (taskbar), light-unplated (light taskbar).
    # The mark carries its own plate, so all three variants are the same pixels.
    Render-Icon $t $t $t (Join-Path $OutDir "Square44x44Logo.targetsize-$t.png")
    Render-Icon $t $t $t (Join-Path $OutDir "Square44x44Logo.targetsize-${t}_altform-unplated.png")
    Render-Icon $t $t $t (Join-Path $OutDir "Square44x44Logo.targetsize-${t}_altform-lightunplated.png")
    # File-type association icon shown by Explorer on associated files.
    Render-Icon $t $t $t (Join-Path $OutDir "FileIcon.targetsize-$t.png")
}

# --- AppIcon.ico (window/taskbar icon via AppWindow.SetIcon): PNG-compressed frames ------------------
Write-Host "AppIcon.ico:"
$frames = @(16, 24, 32, 48, 64, 128, 256)
$blobs = foreach ($f in $frames) {
    $tmp = Join-Path ([IO.Path]::GetTempPath()) "looker-ico-$f.png"
    Render-Icon $f $f $f $tmp
    , [IO.File]::ReadAllBytes($tmp)
    Remove-Item $tmp
}
$ms = New-Object IO.MemoryStream
$bw = New-Object IO.BinaryWriter($ms)
$bw.Write([uint16]0); $bw.Write([uint16]1); $bw.Write([uint16]$frames.Count)
$offset = 6 + 16 * $frames.Count
for ($i = 0; $i -lt $frames.Count; $i++) {
    $f = $frames[$i]; $blob = $blobs[$i]
    $bw.Write([byte]$(if ($f -ge 256) { 0 } else { $f }))
    $bw.Write([byte]$(if ($f -ge 256) { 0 } else { $f }))
    $bw.Write([byte]0); $bw.Write([byte]0)           # palette, reserved
    $bw.Write([uint16]1); $bw.Write([uint16]32)      # planes, bpp
    $bw.Write([uint32]$blob.Length); $bw.Write([uint32]$offset)
    $offset += $blob.Length
}
foreach ($blob in $blobs) { $bw.Write($blob) }
$bw.Flush()
[IO.File]::WriteAllBytes((Join-Path $OutDir 'AppIcon.ico'), $ms.ToArray())
$bw.Dispose()
Write-Host "  AppIcon.ico ($($frames -join ','))"

# --- Cleaned brand SVGs for in-app SvgImageSource ---------------------------------------------------
# Direct2D's SVG parser wants plain SVG: drop the DOCTYPE and Affinity's serif: namespace, and turn the
# style="fill:...;fill-rule:..." declarations into presentation attributes it definitely understands.
Write-Host "Brand SVGs:"
function Clean-Svg([string]$src, [string]$dst) {
    $t = Get-Content -Raw $src
    $t = [regex]::Replace($t, '<!DOCTYPE[^>]*>\s*', '')
    $t = [regex]::Replace($t, '\s+xmlns:serif="[^"]*"', '')
    $t = [regex]::Replace($t, '\s+xml:space="preserve"', '')
    # Percent sizes confuse SvgImageSource's intrinsic-size logic; use the viewBox size in user units.
    if ($t -match 'viewBox="[\d.\-]+\s+[\d.\-]+\s+([\d.]+)\s+([\d.]+)"') {
        $t = $t -replace 'width="100%"', ('width="{0}"' -f $Matches[1]) -replace 'height="100%"', ('height="{0}"' -f $Matches[2])
    }
    $t = [regex]::Replace($t, 'style="([^"]*)"', {
        param($m)
        $attrs = foreach ($decl in ($m.Groups[1].Value -split ';')) {
            if ($decl.Trim() -eq '') { continue }
            $k, $v = $decl.Split(':', 2) | ForEach-Object { $_.Trim() }
            if ($v -match '^rgb\((\d+),\s*(\d+),\s*(\d+)\)$') { $v = '#{0:X2}{1:X2}{2:X2}' -f [int]$Matches[1], [int]$Matches[2], [int]$Matches[3] }
            '{0}="{1}"' -f $k, $v
        }
        $attrs -join ' '
    })
    [IO.File]::WriteAllText($dst, $t, [Text.UTF8Encoding]::new($false))
    Write-Host ("  {0,-56} from {1}" -f (Split-Path -Leaf $dst), (Split-Path -Leaf $src))
}
Clean-Svg $MarkSvg     (Join-Path $OutDir 'Brand\looker_mark.svg')
Clean-Svg $WordmarkSvg (Join-Path $OutDir 'Brand\looker_wordmark.svg')
Clean-Svg $FullSvg     (Join-Path $OutDir 'Brand\looker_full.svg')

$markPath.Dispose(); $ctm.Dispose()
