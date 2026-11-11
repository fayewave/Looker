# Renders Looker's file-type icons (scripts/file-icons/icons.js: one red icon per kind of file, tagged with the
# extension) for every extension at every target size into native/assets/FileIcons, and rewrites the manifest's
# file type associations between the "file types" markers: one association per extension, named after it, with its
# own icon and Explorer type name. Run it after changing icons.js or format::EXTENSIONS.
#
# Renaming or splitting an association changes the ProgID Windows keys the user's default-app choice to: every
# extension whose association changes has to be set to Looker again in Settings > Default apps.

$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
$native = Join-Path $root 'native'
$outDir = Join-Path $native 'assets\FileIcons'

# format::EXTENSIONS, in its order.
$formatRs = Get-Content (Join-Path $native 'src\format.rs') -Raw
$block = [regex]::Match($formatRs, 'pub const EXTENSIONS: &\[&str\] = &\[(.*?)\];', 'Singleline').Groups[1].Value
$block = ($block -split "`n" | ForEach-Object { ($_ -replace '//.*$', '') }) -join "`n"
$exts = [regex]::Matches($block, '"([a-z0-9-]+)"') | ForEach-Object { $_.Groups[1].Value }
if ($exts.Count -lt 50) { throw "Couldn't read format::EXTENSIONS ($($exts.Count) found)." }

# Render through headless Chrome (Edge's headless mode printed nothing here, 2026-11-10).
$browser = @("$env:ProgramFiles\Google\Chrome\Application\chrome.exe", "${env:ProgramFiles(x86)}\Google\Chrome\Application\chrome.exe",
    "$env:LOCALAPPDATA\Google\Chrome\Application\chrome.exe", "${env:ProgramFiles(x86)}\Microsoft\Edge\Application\msedge.exe") |
    Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $browser) { throw 'Chrome not found.' }
$page = 'file:///' + ((Join-Path $PSScriptRoot 'file-icons\render.html') -replace '\\', '/' -replace ' ', '%20')
$tmp = Join-Path ([IO.Path]::GetTempPath()) 'looker-file-icons'
New-Item -ItemType Directory $tmp -Force | Out-Null
$dumpFile = Join-Path $tmp 'dom.html'
Start-Process $browser -Wait -NoNewWindow -RedirectStandardOutput $dumpFile -RedirectStandardError (Join-Path $tmp 'err.txt') -ArgumentList @(
    '--headless=new', '--no-first-run', "--user-data-dir=`"$tmp\profile`"", '--allow-file-access-from-files',
    '--virtual-time-budget=60000', '--dump-dom', $page)
$dom = Get-Content $dumpFile -Raw
$m = [regex]::Match($dom, '<pre id="out">(.*?)</pre>', 'Singleline')
if (-not $m.Success -or $m.Groups[1].Value -notmatch '^\{') { throw "Rendering failed: $($m.Groups[1].Value.Substring(0, [Math]::Min(200, $m.Groups[1].Value.Length)))" }
$data = [Net.WebUtility]::HtmlDecode($m.Groups[1].Value) | ConvertFrom-Json

$rendered = @($data.icons.PSObject.Properties.Name)
$missing = $exts | Where-Object { $_ -notin $rendered }
$extra = $rendered | Where-Object { $_ -notin $exts }
if ($missing -or $extra) { throw "icons.js KIND_OF doesn't match format::EXTENSIONS. Missing: $missing. Extra: $extra." }

# Empty the folder rather than delete it: Dropbox holds the folder itself open.
New-Item -ItemType Directory $outDir -Force | Out-Null
Get-ChildItem $outDir -File | Remove-Item -Force
foreach ($e in $exts) {
    foreach ($p in $data.icons.$e.PSObject.Properties) {
        $bytes = [Convert]::FromBase64String(($p.Value -replace '^data:image/png;base64,', ''))
        [IO.File]::WriteAllBytes((Join-Path $outDir "$e.targetsize-$($p.Name).png"), $bytes)
    }
}

# The manifest's associations.
$sb = [Text.StringBuilder]::new()
foreach ($e in $exts) {
    $name = [Security.SecurityElement]::Escape($data.names.$e)
    [void]$sb.Append(@"
        <uap:Extension Category="windows.fileTypeAssociation">
          <uap:FileTypeAssociation Name="$e">
            <uap:DisplayName>$name</uap:DisplayName>
            <uap:Logo>Assets\FileIcons\$e.png</uap:Logo>
            <uap:InfoTip>Open with Looker</uap:InfoTip>
            <uap:SupportedFileTypes><uap:FileType>.$e</uap:FileType></uap:SupportedFileTypes>
          </uap:FileTypeAssociation>
        </uap:Extension>

"@)
}
$manifestPath = Join-Path $native 'Package.appxmanifest'
$manifest = Get-Content $manifestPath -Raw
$begin = '<!-- file types: begin -->'; $end = '<!-- file types: end -->'
$i = $manifest.IndexOf($begin); $j = $manifest.IndexOf($end)
if ($i -lt 0 -or $j -lt $i) { throw "Manifest has no '$begin' ... '$end' markers." }
$manifest = $manifest.Substring(0, $i + $begin.Length) + "`r`n" + ($sb.ToString() -replace "`r?`n", "`r`n") + '        ' + $manifest.Substring($j)
Set-Content $manifestPath $manifest -NoNewline
# A gallery of every icon, self-contained (the PNGs embedded) so it opens anywhere.
$gallerySizes = '16', '24', '32', '48', '64', '256'
$png = [ordered]@{}
foreach ($e in $exts) {
    $png[$e] = [ordered]@{}
    foreach ($s in $gallerySizes) { $png[$e][$s] = $data.icons.$e.$s }
}
$gallery = (Get-Content (Join-Path $PSScriptRoot 'file-icons\gallery.html') -Raw).
    Replace('/*ICONS_JS*/', (Get-Content (Join-Path $PSScriptRoot 'file-icons\icons.js') -Raw)).
    Replace('/*PNG_DATA*/null', ($png | ConvertTo-Json -Compress -Depth 4))
Set-Content (Join-Path $root 'brand\mockups\file-icons-all.html') $gallery -NoNewline

Write-Host "$($exts.Count) extensions, $((Get-ChildItem $outDir).Count) icons in $outDir; manifest updated."
