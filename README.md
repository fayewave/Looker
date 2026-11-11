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
- Zoom at the pointer up to 25600 %, with crisp pixels from 100 % up; drag to pan, at fit too.
- Fullscreen, slideshow, thumbnail strip; the mouse's Back and Forward buttons step through the files you've viewed.
- Info panel with EXIF, a histogram and a summary of several selected files; edit titles, captions, keywords,
  ratings, author and copyright in JPEG, TIFF, PNG and HEIC, one file or a whole selection at once.
- Filter a folder by field: `keyword:beach`, `camera:x100`, `rating:4+`, `taken:2024`, `ext:png`.
- Rotate, rename, copy, set as wallpaper, show in File Explorer.
- Colour managed, wide gamut included; HDR photos at full brightness on an HDR display.
- Animated GIF, WebP and APNG play. PDFs open as a row of pages. 3D models open in a live view.
- Four themes (Black, Dark, Paper, Light) that can follow Windows, and any accent colour.

<img src="brand/screenshots/readme-info-panel.jpg" alt="The info panel and the thumbnail strip" width="700">

## Video and audio

- Videos play, HDR and 4K60 included, with frame stepping, frame-accurate scrubbing, a timecode or frame count,
  and the media keys and taskbar progress; subtitles from the file itself (SRT, ASS/SSA with its styling and
  fonts, WebVTT, MP4 timed text, PGS, VobSub, DVB, teletext, ARIB) or from files beside it, in any encoding.
- Chapters marked on the seek bar, listed in the bar's Settings menu, and `Shift+N` / `Shift+P` to move between
  them; interlaced video (DVDs, camcorders, TV recordings) deinterlaced as it plays; Dolby and DTS sound can go
  undecoded to a receiver (Passthrough, in the bar's Settings).
- Audio files play with their cover, tags and a waveform (or spectrogram) per channel: click to seek, drag to loop
  a part, Ctrl+wheel to zoom; loudness in LUFS.
- Motion photos (Google and Samsung motion photos, iPhone Live Photos): the Motion button opens the clip with the
  full video controls, and holding the mouse on the photo plays it.
- Rip unprotected DVDs to MKV or MP4.

<img src="brand/screenshots/readme-video.jpg" alt="A 4K video playing, with the video bar" width="700">

<img src="brand/screenshots/readme-audio.jpg" alt="An audio file: cover, tags and the waveform" width="700">

## Text editor

- Text files open in a tabbed editor with a File/Edit/View menu bar, find and replace, spell check, Markdown and
  printing; big files scroll smoothly, and open tabs come back next time.
- Works with screen readers.

<img src="brand/screenshots/readme-light.jpg" alt="The Light theme, with the file explorer beside the photo" width="700">

## Formats

- **Images:** JPEG, PNG/APNG, GIF, BMP, TIFF, WebP, ICO/CUR, JPEG XR, JPEG 2000, HEIC/HEIF, AVIF, JPEG XL, SVG/SVGZ,
  PSD/PSB, TGA, DDS, PCX, PPM/PGM/PBM, XCF, QOI, OpenEXR, Radiance HDR, Valve VTF
- **Camera RAW:** CR2, CR3, CRW, NEF, NRW, ARW, SR2, SRF, DNG, ORF, RAF, RW2, PEF, SRW, 3FR, FFF, RWL, IIQ, MRW, DCR,
  KDC, ERF, MEF
- **Video:** MP4, M4V, MOV, MKV, WebM, AVI, WMV, ASF, MXF, MTS/M2TS, TS, MPG/MPEG, M2V, VOB, 3GP/3G2, FLV, F4V,
  OGV/OGM, DivX/Xvid, RealMedia (RM/RMVB), DV, WTV/DVR-MS, NSV, NUV, GXF, Y4M, IVF, raw H.264/HEVC/MPEG/AV1 streams,
  Bink, Smacker, H.266/VVC; old and pro codecs too (Cinepak, Indeo, HuffYUV, FFV1, Ut Video, MagicYUV, HAP, ZMBV,
  Camtasia, Flash Screen Video, JPEG 2000, Dirac...); game video (RoQ, VQA, PlayStation STR, SMUSH, THP, Sega FILM,
  Interplay MVE, VMD, EA, XMV, HNM, FLIC, DXA and more)
- **Audio:** MP3, MP2, M4A/M4B, AAC, FLAC, WAV, W64, AIFF, Ogg/Opus/Speex, WMA, MKA, WebM audio, WavPack, APE, CAF,
  AC-3/E-AC-3, DTS, TrueHD, MLP, Musepack, TTA, TAK, Shorten, AMR, AU, VOC, GSM, xWMA, DSD (DSF/DFF), RealAudio,
  LC3, SBC, aptX, QCELP, DSS, IAMF, QOA; tracker modules (MOD, XM, IT, S3M and more); MIDI (with GeneralUser GS);
  game audio (HCA, ADX, VAG, XVAG, MSF, BRSTM/BFSTM, AST...)
- **Documents:** PDF
- **Text:** TXT, Markdown, LOG, INI, CFG, JSON, XML, CSV/TSV, YAML, TOML, SRT/VTT subtitles, NFO, Valve VMT/VMF/VDF/QC
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
| `Shift+N`, `Shift+P` | Video: next / previous chapter |
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
