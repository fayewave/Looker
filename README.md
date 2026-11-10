<p align="left">
  <img src="brand/slices/looker_v3_full.svg" alt="Looker" width="400">
</p>

A fast, minimal photo and file viewer for Windows 11, with a proper file explorer built in.

<a href="https://apps.microsoft.com/detail/9NV130N4C2GZ?mode=direct">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://get.microsoft.com/images/en-us%20light.svg">
    <img src="https://get.microsoft.com/images/en-us%20dark.svg" alt="Get it from Microsoft" width="200">
  </picture>
</a>

Or from the [latest release](https://github.com/fayewave/Looker/releases/latest): open `Looker.appinstaller` and
it keeps itself up to date.

---

<img src="brand/screenshots/readme-fit.jpg" alt="A photo in Looker" width="700">

## File explorer

- Beside the photo, or across the window with the photo as a preview (`Ctrl+B`).
- Windows' own folder and drive icons; type a letter to jump to a name.
- Tabs, and a sidebar of Bookmarks, Collections, History and Places.
- Sortable, resizable columns: date taken, camera, lens, dimensions and more.
- Thumbnails at any row height, subfolders listed flat, a saved view per folder.
- Move, copy, rename, delete and new folder, with undo. Drag onto folders and tabs, or out to other apps.
- Favourites and colour tags.

<img src="brand/screenshots/readme-explorer.jpg" alt="Browse mode: tabs, the sidebar and the file list" width="700">

## Viewer

- Opens straight into the file; the next files in the folder are already loaded.
- Zoom at the pointer up to 3200 %, fullscreen, slideshow, thumbnail strip.
- Info panel with EXIF and a histogram.
- Rotate, rename, copy, set as wallpaper, show in File Explorer.
- Colour managed, wide gamut included; HDR photos at full brightness on an HDR display.
- Animated GIF, WebP and APNG play. PDFs open as a row of pages. 3D models open in a live view.
- Videos play, HDR and 4K60 included, with frame stepping, frame-accurate scrubbing, a timecode or frame count,
  and the media keys and taskbar progress; subtitles from the file itself (SRT, ASS/SSA with its styling and
  fonts, WebVTT, MP4 timed text, PGS, VobSub, DVB) or from files beside it, in any encoding.
- Four themes (Black, Dark, Paper, Light) that can follow Windows, and any accent colour.

<img src="brand/screenshots/readme-info-panel.jpg" alt="The info panel and the thumbnail strip" width="700">

## Formats

- **Images:** JPEG, PNG/APNG, GIF, BMP, TIFF, WebP, ICO/CUR, JPEG XR, JPEG 2000, HEIC/HEIF, AVIF, JPEG XL, SVG,
  PSD/PSB, TGA, DDS, PCX, PPM/PGM/PBM, XCF, QOI
- **Camera RAW:** CR2, CR3, CRW, NEF, NRW, ARW, SR2, SRF, DNG, ORF, RAF, RW2, PEF, SRW, 3FR, FFF, RWL, IIQ, MRW, DCR,
  KDC, ERF, MEF
- **Video:** MP4, M4V, MOV, MKV, WebM, AVI, WMV, MXF, MTS/M2TS, MPG/MPEG, 3GP
- **Documents:** PDF
- **3D:** OBJ (with MTL), STL, PLY, 3MF, glTF/GLB; MTL material libraries

## Keys

| Key | Action |
|---|---|
| `←` `→`, `Home` `End` | Previous / next, first / last |
| Wheel, `Ctrl` `+` `-` | Zoom at the pointer |
| `Ctrl+0`, `Ctrl+1`, double-click | Fit, 100 %, toggle |
| `Ctrl+E`, `Ctrl+B`, `Ctrl+Shift+T`, `Ctrl+I` | File explorer, browse mode, thumbnail strip, info panel |
| A letter (in the file explorer) | Jump to the next name starting with it |
| `Ctrl+T`, `Ctrl+W`, `Ctrl+Tab` | New tab, close tab, next tab |
| `Ctrl+F`, `Ctrl+Z` | Filter the folder, undo |
| `F11`, `F5`, `Space` | Fullscreen, slideshow, pause |
| `,` `.`, `J` `K` `L`, `M` | Video: previous / next frame, slower / pause / faster, mute |
| `V`, `G` `H` | Video: next subtitle track, subtitles 50 ms earlier / later |
| `Delete`, `F2` | Delete, rename |
| `Ctrl+R`, `Ctrl+S` | Rotate, save rotation |
| `Ctrl+C`, `Ctrl+Shift+C`, `Ctrl+Shift+E` | Copy image, copy path, show in File Explorer |
| `Ctrl+,` | Settings |

## Build

Windows 11, Rust (stable, MSVC), the Windows SDK and Developer Mode.

```powershell
cd native; cargo build --release; cargo test --release; cd ..
pwsh scripts/Publish-Native.ps1                # install the build as the Looker package
pwsh scripts/Build-NativePackage.ps1 -Store    # Microsoft Store upload
```

Native Rust: Direct2D and DirectWrite, images through WIC, video through FFmpeg, 3D through Direct3D 11.
