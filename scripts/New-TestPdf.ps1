# Writes a small hand-built multi-page PDF (no external tools): 5 pages of mixed sizes — Letter portrait,
# A4 landscape, Letter, a small square, Letter — each with a big page number, a border and a bar of text,
# so page order, sizes and centring are all visible in the viewer.
param([string]$Path = "$env:USERPROFILE\Pictures\Looker Test Photos\multipage.pdf")

$pages = @(
    @{ W = 612; H = 792 },
    @{ W = 842; H = 595 },
    @{ W = 612; H = 792 },
    @{ W = 400; H = 400 },
    @{ W = 612; H = 792 }
)

$objects = New-Object System.Collections.Generic.List[string]
# 1 catalog, 2 pages, 3 font; then per page: page obj + content obj
$kidsIds = @()
for ($i = 0; $i -lt $pages.Count; $i++) { $kidsIds += (4 + $i * 2) }
$kids = ($kidsIds | ForEach-Object { "$_ 0 R" }) -join ' '

$objects.Add("<< /Type /Catalog /Pages 2 0 R >>")
$objects.Add("<< /Type /Pages /Kids [ $kids ] /Count $($pages.Count) >>")
$objects.Add("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>")

for ($i = 0; $i -lt $pages.Count; $i++) {
    $w = $pages[$i].W; $h = $pages[$i].H
    $pageId = 4 + $i * 2; $contentId = $pageId + 1
    $n = $i + 1
    $shade = 0.15 + 0.15 * $i
    $content = @"
q 0.85 0.85 0.85 rg 20 20 $($w - 40) $($h - 40) re f Q
q $shade 0.2 0.6 rg 40 $($h - 120) $($w - 80) 60 re f Q
q 0 0 0 RG 4 w 20 20 $($w - 40) $($h - 40) re S Q
BT /F1 160 Tf $([int]($w / 2 - 50)) $([int]($h / 2 - 60)) Td ($n) Tj ET
BT /F1 24 Tf 40 40 Td (Page $n of $($pages.Count) - $w x $h pt) Tj ET
"@
    $objects.Add("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 $w $h] /Resources << /Font << /F1 3 0 R >> >> /Contents $contentId 0 R >>")
    $objects.Add("<< /Length $([System.Text.Encoding]::ASCII.GetByteCount($content)) >>`nstream`n$content`nendstream")
}

$sb = New-Object System.Text.StringBuilder
[void]$sb.Append("%PDF-1.4`n")
$offsets = @()
for ($i = 0; $i -lt $objects.Count; $i++) {
    $offsets += [System.Text.Encoding]::ASCII.GetByteCount($sb.ToString())
    [void]$sb.Append("$($i + 1) 0 obj`n$($objects[$i])`nendobj`n")
}
$xref = [System.Text.Encoding]::ASCII.GetByteCount($sb.ToString())
[void]$sb.Append("xref`n0 $($objects.Count + 1)`n0000000000 65535 f `n")
foreach ($o in $offsets) { [void]$sb.Append(("{0:D10} 00000 n `n" -f $o)) }
[void]$sb.Append("trailer`n<< /Size $($objects.Count + 1) /Root 1 0 R >>`nstartxref`n$xref`n%%EOF`n")

New-Item -ItemType Directory -Force (Split-Path $Path) | Out-Null
[System.IO.File]::WriteAllBytes($Path, [System.Text.Encoding]::ASCII.GetBytes($sb.ToString()))
Write-Host "wrote $Path ($((Get-Item $Path).Length) bytes)"
