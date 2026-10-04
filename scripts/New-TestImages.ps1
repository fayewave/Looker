<#
.SYNOPSIS
    Generates a folder of sample images, one per format Looker supports, for manual testing.

.DESCRIPTION
    Uses Magick.NET from the NuGet cache (Magick.NET-Q8-x64 + Magick.NET.Core, the versions the retired C# app
    shipped; `dotnet restore` of the csharp-final tag puts them there), or from -BinDir. Every image is a plasma fractal labelled with
    its format so it's obvious in the viewer which file is open. Also emits edge cases: a 300 dpi JPEG
    (the DPI-pixelation gotcha), 16-bit PNG, PNG with alpha, CMYK JPEG, EXIF-rotated JPEG, a large
    6000x4000 JPEG (progressive decode), animated GIF/WebP/APNG/AVIF, and an SVG. Sept 2026 additions:
    JPEG XR (via WPF's inbox encoder), JPEG 2000, CUR, PCX, PPM/PGM/PBM, QOI, PSB, a hand-built XCF and a
    HIF copy of any HEIC already in the folder. Camera RAW cannot be synthesised and Magick.NET cannot
    encode HEIC - copy real .heic/.cr2/.nef/.dng files in by hand (the 60-74 RAW samples in the test
    folder are CC0 files from raw.pixls.us, one per extension Looker added in Sept 2026).

.EXAMPLE
    pwsh scripts/New-TestImages.ps1 -OutDir F:\tmp\looker-test-photos
#>
[CmdletBinding()]
param(
    [string]$OutDir = (Join-Path $env:USERPROFILE 'Pictures\Looker Test Photos'),
    [string]$BinDir,
    [string]$MagickVersion = '14.14.0'
)

$ErrorActionPreference = 'Stop'
if (-not $BinDir) {
    # Managed and native DLLs side by side, so the managed one finds Magick.Native next to itself.
    $nuget = Join-Path $env:USERPROFILE '.nuget\packages'
    $BinDir = Join-Path ([IO.Path]::GetTempPath()) "looker-magick-$MagickVersion"
    New-Item -ItemType Directory -Force $BinDir | Out-Null
    foreach ($src in "magick.net.core\$MagickVersion\lib\net8.0\Magick.NET.Core.dll",
                     "magick.net-q8-x64\$MagickVersion\lib\net8.0\Magick.NET-Q8-x64.dll",
                     "magick.net-q8-x64\$MagickVersion\runtimes\win-x64\native\Magick.Native-Q8-x64.dll") {
        $path = Join-Path $nuget $src
        if (-not (Test-Path $path)) { throw "Magick.NET $MagickVersion is not in the NuGet cache ($path). Pass -BinDir." }
        Copy-Item $path $BinDir -Force
    }
}
$BinDir = (Resolve-Path $BinDir).Path
foreach ($dll in 'Magick.NET.Core.dll', 'Magick.NET-Q8-x64.dll') {
    Add-Type -Path (Join-Path $BinDir $dll)
}
New-Item -ItemType Directory -Force $OutDir | Out-Null

function New-Base([int]$w, [int]$h, [string]$label, [int]$seed) {
    [ImageMagick.MagickNET]::SetRandomSeed($seed)
    $img = [ImageMagick.MagickImage]::new("plasma:fractal", [uint32]$w, [uint32]$h)
    $img.Blur(0, 2)
    $img.Modulate([ImageMagick.Percentage]::new(100), [ImageMagick.Percentage]::new(140), [ImageMagick.Percentage]::new(100))
    # Fine detail so resolution/pixelation problems are visible: a thin grid + concentric rings.
    $d = [ImageMagick.Drawing.Drawables]::new()
    $null = $d.StrokeColor([ImageMagick.MagickColor]::new('#00000060')).StrokeWidth(1).FillColor([ImageMagick.MagickColors]::Transparent)
    for ($x = 0; $x -lt $w; $x += 100) { $null = $d.Line($x, 0, $x, $h) }
    for ($y = 0; $y -lt $h; $y += 100) { $null = $d.Line(0, $y, $w, $y) }
    $cx = $w * 0.75; $cy = $h * 0.65
    for ($r = 20; $r -lt [Math]::Min($w, $h) * 0.3; $r += 12) { $null = $d.Circle($cx, $cy, $cx + $r, $cy) }
    $img.Draw($d)
    # Label
    $t = [ImageMagick.Drawing.Drawables]::new()
    $fontSize = [Math]::Max(24, [int]($w / 18))
    $null = $t.Font('Segoe UI').FontPointSize($fontSize).FillColor([ImageMagick.MagickColors]::White).StrokeColor([ImageMagick.MagickColors]::Black).StrokeWidth(2).Gravity([ImageMagick.Gravity]::Northwest)
    $null = $t.Text(30, 30, $label)
    $null = $t.FontPointSize([int]($fontSize * 0.5)).StrokeWidth(1).Text(30, 30 + $fontSize * 1.5, "$w x $h")
    $img.Draw($t)
    return $img
}

function Save([ImageMagick.MagickImage]$img, [string]$name) {
    $path = Join-Path $OutDir $name
    try {
        $img.Write($path)
        Write-Host ("  ok   {0,-34} {1,8:N0} KB" -f $name, ((Get-Item $path).Length / 1KB))
    } catch {
        Write-Host ("  FAIL {0,-34} {1}" -f $name, $_.Exception.Message.Split("`n")[0])
        Remove-Item $path -ErrorAction SilentlyContinue
    }
}

Write-Host "Writing to $OutDir"
$seed = 1

# --- One per format, ordinary size -----------------------------------------------------------
# (No HEIC: Magick.NET ships without an HEVC encoder. Drop a real iPhone .heic into the folder.)
$formats = @(
    @{ n = '01 JPEG.jpg';       f = 'Jpeg' },
    @{ n = '02 PNG.png';        f = 'Png' },
    @{ n = '03 BMP.bmp';        f = 'Bmp' },
    @{ n = '04 GIF.gif';        f = 'Gif' },
    @{ n = '05 TIFF.tif';       f = 'Tiff' },
    @{ n = '06 WebP.webp';      f = 'WebP' },
    @{ n = '08 AVIF.avif';      f = 'Avif' },
    @{ n = '09 JPEG XL.jxl';    f = 'Jxl' },
    @{ n = '10 PSD.psd';        f = 'Psd' },
    @{ n = '11 TGA.tga';        f = 'Tga' },
    @{ n = '12 DDS.dds';        f = 'Dds' }
)
foreach ($e in $formats) {
    $label = ($e.n -replace '^\d+ ', '') -replace '\.\w+$', ''
    $img = New-Base 1920 1280 $label ($seed++)
    $img.Format = [ImageMagick.MagickFormat]::($e.f)
    $img.Quality = 90
    Save $img $e.n
    $img.Dispose()
}

# ICO (max 256 px)
$img = New-Base 256 256 'ICO' ($seed++)
$img.Format = [ImageMagick.MagickFormat]::Ico
Save $img '13 ICO.ico'
$img.Dispose()

# --- Edge cases ------------------------------------------------------------------------------
Write-Host "Edge cases"

# 300 dpi JPEG: must render identically to the 96 dpi twin (WicDecoder DPI normalisation).
$img = New-Base 1920 1280 'JPEG 300 dpi' ($seed++)
$img.Format = [ImageMagick.MagickFormat]::Jpeg; $img.Quality = 90
$img.Density = [ImageMagick.Density]::new(300, 300, [ImageMagick.DensityUnit]::PixelsPerInch)
Save $img '20 JPEG 300dpi.jpg'
$img.Density = [ImageMagick.Density]::new(96, 96, [ImageMagick.DensityUnit]::PixelsPerInch)
Save $img '21 JPEG 96dpi twin.jpg'
$img.Dispose()

# Large: exercises progressive decode / resolution upgrade on zoom.
$img = New-Base 6000 4000 'JPEG 6000x4000' ($seed++)
$img.Format = [ImageMagick.MagickFormat]::Jpeg; $img.Quality = 85
Save $img '22 JPEG large 24MP.jpg'
$img.Dispose()

# Progressive JPEG
$img = New-Base 1920 1280 'JPEG progressive' ($seed++)
$img.Format = [ImageMagick.MagickFormat]::Jpeg; $img.Quality = 90; $img.Settings.Interlace = [ImageMagick.Interlace]::Jpeg
Save $img '23 JPEG progressive.jpg'
$img.Dispose()

# CMYK JPEG
$img = New-Base 1920 1280 'JPEG CMYK' ($seed++)
$img.ColorSpace = [ImageMagick.ColorSpace]::CMYK
$img.Format = [ImageMagick.MagickFormat]::Jpeg; $img.Quality = 90
Save $img '24 JPEG CMYK.jpg'
$img.Dispose()

# EXIF orientation 6 (rotate 90 CW on display): stored rotated so honouring the tag restores upright.
$img = New-Base 1920 1280 'JPEG EXIF rotate (should read upright)' ($seed++)
$img.Rotate(-90)
$img.Orientation = [ImageMagick.OrientationType]::RightTop
$img.Format = [ImageMagick.MagickFormat]::Jpeg; $img.Quality = 90
Save $img '25 JPEG EXIF orientation 6.jpg'
$img.Dispose()

# PNG with alpha: transparent holes so the checkerboard/backdrop shows through.
$img = New-Base 1600 1200 'PNG alpha' ($seed++)
$img.Alpha([ImageMagick.AlphaOption]::Set)
$mask = [ImageMagick.MagickImage]::new([ImageMagick.MagickColors]::White, 1600, 1200)
$md = [ImageMagick.Drawing.Drawables]::new()
$null = $md.FillColor([ImageMagick.MagickColors]::Black).Circle(400, 300, 400, 620).Rectangle(1000, 700, 1500, 1100)
$mask.Draw($md)
$img.Composite($mask, [ImageMagick.CompositeOperator]::CopyAlpha)
$mask.Dispose()
$img.Format = [ImageMagick.MagickFormat]::Png
Save $img '26 PNG alpha.png'
$img.Dispose()

# 16-bit PNG
$img = New-Base 1920 1280 'PNG 16-bit' ($seed++)
$img.Depth = 16
$img.Format = [ImageMagick.MagickFormat]::Png48
Save $img '27 PNG 16-bit.png'
$img.Dispose()

# Tiny image (upscale path)
$img = New-Base 64 48 'tiny' ($seed++)
$img.Format = [ImageMagick.MagickFormat]::Png
Save $img '28 PNG tiny 64x48.png'
$img.Dispose()

# Extreme aspect ratio (panorama)
$img = New-Base 8000 800 'Panorama 10:1' ($seed++)
$img.Format = [ImageMagick.MagickFormat]::Jpeg; $img.Quality = 85
Save $img '29 JPEG panorama 8000x800.jpg'
$img.Dispose()

# Animated GIF + WebP: 24 frames, orbiting dot.
foreach ($fmt in @(
        @{ f = 'Gif';  n = '30 GIF animated.gif';    l = 'Animated GIF' },
        @{ f = 'WebP'; n = '31 WebP animated.webp';  l = 'Animated WebP' },
        @{ f = 'APng'; n = '37 APNG animated.apng';  l = 'Animated PNG' },
        @{ f = 'Avif'; n = '39 AVIF animated.avifs'; l = 'Animated AVIF' })) {
    $col = [ImageMagick.MagickImageCollection]::new()
    $base = New-Base 800 600 $fmt.l ($seed)
    for ($i = 0; $i -lt 24; $i++) {
        $frame = $base.Clone()
        $ang = $i / 24.0 * 2 * [Math]::PI
        $px = 400 + 150 * [Math]::Cos($ang); $py = 380 + 150 * [Math]::Sin($ang)
        $fd = [ImageMagick.Drawing.Drawables]::new()
        $null = $fd.FillColor([ImageMagick.MagickColors]::White).StrokeColor([ImageMagick.MagickColors]::Black).StrokeWidth(2)
        $null = $fd.Circle($px, $py, $px + 30, $py)
        $frame.Draw($fd)
        $frame.AnimationDelay = 5   # centiseconds -> 50 ms/frame
        $frame.AnimationIterations = 0
        $frame.Format = [ImageMagick.MagickFormat]::($fmt.f)
        $col.Add($frame)
    }
    $base.Dispose()
    $path = Join-Path $OutDir $fmt.n
    try {
        if ($fmt.f -eq 'Gif') { $col.Optimize() }
        $col.Write($path)
        Write-Host ("  ok   {0,-34} {1,8:N0} KB" -f $fmt.n, ((Get-Item $path).Length / 1KB))
    } catch {
        Write-Host ("  FAIL {0,-34} {1}" -f $fmt.n, $_.Exception.Message.Split("`n")[0])
    }
    $col.Dispose()
    $seed++
}

# SVG: hand-written vector so it's a genuine vector file, not a raster wrapped in <image>.
$svg = @'
<svg xmlns="http://www.w3.org/2000/svg" width="1200" height="800" viewBox="0 0 1200 800">
  <defs>
    <linearGradient id="g" x1="0" y1="0" x2="1" y2="1">
      <stop offset="0" stop-color="#1e3a8a"/><stop offset="1" stop-color="#f97316"/>
    </linearGradient>
  </defs>
  <rect width="1200" height="800" fill="url(#g)"/>
  <g stroke="#ffffff" stroke-opacity="0.35" stroke-width="1">
    <path d="M0 100H1200M0 200H1200M0 300H1200M0 400H1200M0 500H1200M0 600H1200M0 700H1200M100 0V800M200 0V800M300 0V800M400 0V800M500 0V800M600 0V800M700 0V800M800 0V800M900 0V800M1000 0V800M1100 0V800"/>
  </g>
  <circle cx="850" cy="480" r="220" fill="#fde68a" stroke="#111" stroke-width="6"/>
  <circle cx="850" cy="480" r="120" fill="none" stroke="#111" stroke-width="2"/>
  <circle cx="850" cy="480" r="40" fill="#111"/>
  <path d="M120 640 Q300 380 480 640 T840 640" fill="none" stroke="#fff" stroke-width="10" stroke-linecap="round"/>
  <text x="40" y="110" font-family="Segoe UI, Arial, sans-serif" font-size="88" font-weight="700" fill="#fff" stroke="#000" stroke-width="2">SVG</text>
  <text x="40" y="160" font-family="Segoe UI, Arial, sans-serif" font-size="36" fill="#fff">1200 x 800 (vector - zoom should stay crisp)</text>
</svg>
'@
Set-Content -Path (Join-Path $OutDir '32 SVG.svg') -Value $svg -Encoding UTF8
Write-Host ("  ok   {0,-34}" -f '32 SVG.svg')

# --- Formats added Sept 2026 ------------------------------------------------------------------
Write-Host "More formats"

# Plain Magick-encodable ones.
$more = @(
    @{ n = '15 JPEG 2000.jp2';           f = 'Jp2' },
    @{ n = '16 JPEG 2000 codestream.j2k'; f = 'J2k' },
    @{ n = '18 PCX.pcx';                 f = 'Pcx' },
    @{ n = '19 PPM.ppm';                 f = 'Ppm' },
    @{ n = '33 PGM.pgm';                 f = 'Pgm' },
    @{ n = '34 PBM.pbm';                 f = 'Pbm' },
    @{ n = '35 QOI.qoi';                 f = 'Qoi' },
    @{ n = '36 PSB.psb';                 f = 'Psb' }
)
foreach ($e in $more) {
    $label = ($e.n -replace '^\d+ ', '') -replace '\.\w+$', ''
    $img = New-Base 1920 1280 $label ($seed++)
    $img.Format = [ImageMagick.MagickFormat]::($e.f)
    $img.Quality = 90
    Save $img $e.n
    $img.Dispose()
}

# CUR: a Windows cursor (same container as ICO, type 2; max 256 px).
$img = New-Base 128 128 'CUR' ($seed++)
$img.Format = [ImageMagick.MagickFormat]::Cur
Save $img '17 CUR.cur'
$img.Dispose()

# JPEG XR: Magick.NET has no JXR coder, so encode through WPF's inbox WmpBitmapEncoder (the same WIC codec
# Looker decodes with). PNG in memory -> WPF frame -> JXR file.
try {
    Add-Type -AssemblyName PresentationCore
    $img = New-Base 1920 1280 'JPEG XR' ($seed++)
    $pngBytes = $img.ToByteArray([ImageMagick.MagickFormat]::Png)
    $img.Dispose()
    $pngStream = [System.IO.MemoryStream]::new($pngBytes)
    $decoder = [System.Windows.Media.Imaging.PngBitmapDecoder]::new($pngStream,
        [System.Windows.Media.Imaging.BitmapCreateOptions]::PreservePixelFormat,
        [System.Windows.Media.Imaging.BitmapCacheOption]::OnLoad)
    $encoder = [System.Windows.Media.Imaging.WmpBitmapEncoder]::new()
    $encoder.ImageQualityLevel = 0.9
    $encoder.Frames.Add([System.Windows.Media.Imaging.BitmapFrame]::Create($decoder.Frames[0]))
    $jxrPath = Join-Path $OutDir '14 JPEG XR.jxr'
    $fs = [System.IO.File]::Create($jxrPath)
    $encoder.Save($fs)
    $fs.Dispose(); $pngStream.Dispose()
    Write-Host ("  ok   {0,-34} {1,8:N0} KB" -f '14 JPEG XR.jxr', ((Get-Item $jxrPath).Length / 1KB))
} catch {
    Write-Host ("  FAIL {0,-34} {1}" -f '14 JPEG XR.jxr', $_.Exception.Message.Split("`n")[0])
}

# XCF: Magick reads GIMP files but cannot write them, so build a minimal v0 file by hand: one RGBA layer in
# RLE-compressed 64x64 tiles, exactly the shape GIMP writes. Two ImageMagick reader quirks forced that shape
# (coders/xcf.c): its RGB tile loaders walk 4 bytes per pixel, so an RGB-without-alpha layer dies with "not enough
# pixel data", and its *uncompressed* loader maps pixel alpha 255 to TransparentAlpha (a bug nobody hits because
# GIMP always RLE-compresses) - the whole image came out invisible. The RLE loader reads alpha correctly. RLE here
# is the trivial encoding: per tile, per byte-plane (R, G, B, A), one "verbatim" run (opcode 128 + big-endian
# length + the raw plane bytes). Dimensions deliberately not multiples of 64 so edge tiles get exercised.
function Write-Xcf([ImageMagick.MagickImage]$img, [string]$path) {
    $w = [int]$img.Width; $h = [int]$img.Height
    $img.Alpha([ImageMagick.AlphaOption]::Set)
    $rgba = $img.GetPixels().ToByteArray('RGBA')
    $ms = [System.IO.MemoryStream]::new()
    $u32 = {
        param([long]$v)
        $b = [BitConverter]::GetBytes([uint32]$v); [Array]::Reverse($b); $ms.Write($b, 0, 4)
    }
    $patch = {
        param([long]$pos, [long]$v)
        $save = $ms.Position; $ms.Position = $pos; & $u32 $v; $ms.Position = $save
    }
    $magic = [System.Text.Encoding]::ASCII.GetBytes('gimp xcf file')
    $ms.Write($magic, 0, $magic.Length); $ms.WriteByte(0)
    & $u32 $w; & $u32 $h; & $u32 0                     # base type RGB
    & $u32 17; & $u32 1; $ms.WriteByte(1)              # PROP_COMPRESSION = RLE
    & $u32 0; & $u32 0                                 # PROP_END
    $layerOffsetPos = $ms.Position
    & $u32 0; & $u32 0                                 # layer offset (patched), terminator
    & $u32 0                                           # no channels
    $layerOffset = $ms.Position
    & $u32 $w; & $u32 $h; & $u32 1                     # layer: RGBA
    $name = [System.Text.Encoding]::ASCII.GetBytes('Background')
    & $u32 ($name.Length + 1); $ms.Write($name, 0, $name.Length); $ms.WriteByte(0)
    & $u32 6; & $u32 4; & $u32 255                     # PROP_OPACITY
    & $u32 8; & $u32 4; & $u32 1                       # PROP_VISIBLE
    & $u32 15; & $u32 8; & $u32 0; & $u32 0            # PROP_OFFSETS
    & $u32 0; & $u32 0                                 # PROP_END
    $hierarchyOffsetPos = $ms.Position
    & $u32 0; & $u32 0                                 # hierarchy offset (patched), no mask
    $hierarchyOffset = $ms.Position
    & $u32 $w; & $u32 $h; & $u32 4                     # bpp
    $levelOffsetPos = $ms.Position
    & $u32 0; & $u32 0                                 # level offset (patched), terminator
    $levelOffset = $ms.Position
    & $u32 $w; & $u32 $h
    $tilesX = [int][Math]::Ceiling($w / 64.0); $tilesY = [int][Math]::Ceiling($h / 64.0)
    $tileOffsetPos = $ms.Position
    for ($i = 0; $i -lt $tilesX * $tilesY; $i++) { & $u32 0 }
    & $u32 0
    $tileOffsets = [System.Collections.Generic.List[long]]::new()
    for ($ty = 0; $ty -lt $tilesY; $ty++) {
        for ($tx = 0; $tx -lt $tilesX; $tx++) {
            $tileOffsets.Add($ms.Position)
            $tw = [Math]::Min(64, $w - $tx * 64); $th = [Math]::Min(64, $h - $ty * 64)
            $plane = New-Object byte[] ($tw * $th)
            for ($c = 0; $c -lt 4; $c++) {
                $k = 0
                for ($y = 0; $y -lt $th; $y++) {
                    $start = (($ty * 64 + $y) * $w + $tx * 64) * 4 + $c
                    for ($x = 0; $x -lt $tw; $x++) { $plane[$k++] = $rgba[$start + $x * 4] }
                }
                $ms.WriteByte(128)                                        # long verbatim run
                $ms.WriteByte([byte](($plane.Length -shr 8) -band 0xFF))  # big-endian 16-bit length
                $ms.WriteByte([byte]($plane.Length -band 0xFF))
                $ms.Write($plane, 0, $plane.Length)
            }
        }
    }
    $ms.Write((New-Object byte[] 24576), 0, 24576)     # slack: the reader sizes the last tile's read at 64*64*4*1.5
    & $patch $layerOffsetPos $layerOffset
    & $patch $hierarchyOffsetPos $hierarchyOffset
    & $patch $levelOffsetPos $levelOffset
    for ($i = 0; $i -lt $tileOffsets.Count; $i++) { & $patch ($tileOffsetPos + 4 * $i) $tileOffsets[$i] }
    [System.IO.File]::WriteAllBytes($path, $ms.ToArray())
    $ms.Dispose()
}
try {
    $img = New-Base 1500 1000 'XCF' ($seed++)
    $xcfPath = Join-Path $OutDir '38 XCF.xcf'
    Write-Xcf $img $xcfPath
    $img.Dispose()
    $check = [ImageMagick.MagickImage]::new($xcfPath)   # prove Magick (= Looker's decoder) reads it back
    $alphaMean = [int]$check.Statistics().GetChannel([ImageMagick.PixelChannel]::Alpha).Mean
    $note = "reads back as $($check.Width)x$($check.Height), alpha mean $alphaMean (255 = opaque)"
    $check.Dispose()
    Write-Host ("  ok   {0,-34} {1,8:N0} KB  {2}" -f '38 XCF.xcf', ((Get-Item $xcfPath).Length / 1KB), $note)
} catch {
    Write-Host ("  FAIL {0,-34} {1}" -f '38 XCF.xcf', $_.Exception.Message.Split("`n")[0])
}

# HIF is just HEIF with Fujifilm/Sony's extension: copy a real HEIC if one is in the folder.
$heic = Get-ChildItem $OutDir -Filter '*.heic' | Select-Object -First 1
if ($heic) {
    Copy-Item $heic.FullName (Join-Path $OutDir '50 HIF (copy of a HEIC).hif') -Force
    Write-Host ("  ok   {0,-34} copied from {1}" -f '50 HIF (copy of a HEIC).hif', $heic.Name)
} else {
    Write-Host "  skip 50 HIF: no .heic in the folder to copy"
}

Write-Host ""
Write-Host "Done. $((Get-ChildItem $OutDir -File).Count) files in $OutDir"
