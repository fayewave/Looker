// Looker's file-type icons: one solid red icon per kind of file, with a tag naming the extension at 48 px and up.
// Drawn at 256 units; at 32 px and under each kind has a simpler form. The design is
// brand/mockups/file-icons-kinds.html (picked 2026-11-10). scripts/New-FileIcons.ps1 renders every extension at
// every target size through render.html and writes the manifest's file type associations.

const RED = "#F52524", DARK = "#B81716", DEEP = "#8C100F", LIGHT = "#FF6B5E", INK = "#fff";
const FONT = "'Segoe UI Variable Display', 'Segoe UI', system-ui, sans-serif";
const MARK = "M7348.97,461.776L7348.97,1039.201C7348.97,1080.513 7315.431,1114.053 7274.119,1114.053L6696.694,1114.053C6655.382,1114.053 6621.842,1080.513 6621.842,1039.201L6621.842,461.776C6621.842,420.464 6655.382,386.925 6696.694,386.925L7274.119,386.925C7315.431,386.925 7348.97,420.464 7348.97,461.776ZM6883.284,877.256C6865.955,842.158 6855.645,798.189 6855.645,750.489C6855.645,702.789 6865.955,658.819 6883.273,623.717C6887.908,614.577 6886.264,603.507 6879.173,596.109C6872.082,588.712 6861.091,586.601 6851.764,590.845C6775.266,625.075 6707.829,683.821 6707.829,750.489C6707.829,817.157 6775.266,875.902 6851.759,910.143C6861.09,914.389 6872.087,912.277 6879.181,904.876C6886.276,897.475 6887.921,886.399 6883.284,877.256ZM7087.529,623.722C7104.857,658.819 7115.168,702.789 7115.168,750.489C7115.168,798.189 7104.857,842.158 7087.539,877.261C7082.904,886.4 7084.549,897.47 7091.64,904.868C7098.731,912.266 7109.722,914.377 7119.049,910.132C7195.547,875.902 7262.983,817.157 7262.983,750.489C7262.983,683.821 7195.547,625.075 7119.054,590.835C7109.722,586.588 7098.726,588.7 7091.631,596.101C7084.537,603.503 7082.892,614.578 7087.529,623.722Z";

// Every extension (must equal format::EXTENSIONS; New-FileIcons.ps1 checks), its kind and its Explorer type name.
const KIND_OF = {
  photo: "jpg jpeg jpe jfif png bmp dib tif tiff webp jxr wdp hdp heic heif hif avif jxl jp2 j2k jpf jpx j2c tga icb vda vst dds pcx pnm ppm pgm pbm qoi vtf",
  anim: "gif apng heics avifs",
  icon: "ico cur",
  vector: "svg",
  doc: "pdf txt vmt md markdown log ini cfg vdf qc",
  layers: "psd psb xcf",
  raw: "cr2 cr3 crw nef nrw arw sr2 srf dng orf raf rw2 pef srw 3fr fff rwl iiq mrw dcr kdc erf mef",
  model: "obj stl ply 3mf gltf glb vmf",
  material: "mtl",
  video: "mp4 m4v mov mkv webm avi wmv mxf mts m2ts mpg mpeg 3gp",
  audio: "mp3 m4a m4b aac flac wav aif aiff ogg oga opus wma mka wv ape caf",
};
const TYPE_NAME = {
  photo: e => `${e} image`, anim: e => `${e} animation`, icon: e => e === "CUR" ? "Cursor" : "Icon",
  vector: () => "SVG image", doc: e => ({ PDF: "PDF document", TXT: "Text document", VMT: "VMT Source material", MD: "Markdown document", MARKDOWN: "Markdown document", LOG: "Log file", INI: "Configuration settings", CFG: "Configuration file", VDF: "Valve data file", QC: "QC model script" })[e] || `${e} file`, layers: e => `${e} layered image`, raw: e => `${e} camera RAW`,
  model: e => e === "VMF" ? "VMF Hammer map" : `${e} 3D model`,
  material: () => "MTL material library", video: e => `${e} video`,
  audio: e => `${e} audio`,
};

// The real width of `t` at `size` (with the 2 % letter spacing), measured in Segoe UI.
const measureCtx = document.createElement("canvas").getContext("2d");
function textWidth(t, size, weight = 800) {
  measureCtx.font = `${weight} ${size}px ${FONT}`;
  return measureCtx.measureText(t).width + t.length * size * 0.02;
}

function fitText(t, x, y, maxW, size, fill, weight = 800) {
  const w = textWidth(t, size, weight);
  const s = w > maxW ? size * maxW / w : size;
  // Extensions are all capitals: centre the cap height (0.7 em in Segoe UI) on y, not the em box.
  return `<text x="${x}" y="${(y + s * 0.35).toFixed(2)}" font-family="${FONT}" font-weight="${weight}" font-size="${s.toFixed(2)}" fill="${fill}" text-anchor="middle" letter-spacing="${(s * 0.02).toFixed(2)}">${t}</text>`;
}

function whiteMark(x, y, s) {
  const k = s / 727.128;
  return `<g transform="translate(${x} ${y}) scale(${k}) translate(-6621.842 -386.925)"><path d="${MARK}" fill="${INK}" fill-rule="evenodd"/></g>`;
}

function scene(x, y, w, h, small) {
  const X = f => (x + f * w).toFixed(1), Y = f => (y + f * h).toFixed(1);
  const sun = small ? "" : `<circle cx="${X(0.72)}" cy="${Y(0.28)}" r="${(Math.min(w, h) * 0.11).toFixed(1)}" fill="${INK}"/>`;
  return sun + `<path d="M${X(0.1)} ${Y(0.86)} L${X(0.4)} ${Y(small ? 0.36 : 0.44)} L${X(0.6)} ${Y(0.68)} L${X(0.72)} ${Y(0.54)} L${X(0.9)} ${Y(0.86)} Z" fill="${INK}" stroke="${INK}" stroke-width="${small ? 10 : 8}" stroke-linejoin="round"/>`;
}

const DRAW = {
  photo: (ext, small) => `<rect x="12" y="32" width="232" height="192" rx="26" fill="${RED}"/>` + scene(12, 32, 232, 192, small),
  raw: (ext, small) => `<path d="M82 70 L98 40 Q103 30 115 30 H141 Q153 30 158 40 L174 70 Z" fill="${RED}" stroke="${RED}" stroke-width="8" stroke-linejoin="round"/>
    <rect x="10" y="66" width="236" height="164" rx="30" fill="${RED}"/>
    <circle cx="128" cy="150" r="${small ? 60 : 58}" fill="${INK}"/><circle cx="128" cy="150" r="${small ? 34 : 38}" fill="${RED}"/>
    ${small ? "" : `<circle cx="114" cy="136" r="10" fill="${INK}"/><rect x="196" y="88" width="30" height="16" rx="6" fill="${INK}"/>`}`,
  video: (ext, small) => {
    const holes = small ? [62, 118, 174] : [58, 96, 134, 172];
    const hs = small ? 26 : 18;
    let s = `<rect x="8" y="36" width="240" height="184" rx="26" fill="${RED}"/>`;
    for (const y of holes) s += `<rect x="${small ? 18 : 22}" y="${y + (small ? 0 : 4)}" width="${hs}" height="${hs}" rx="5" fill="${INK}"/><rect x="${small ? 212 : 216}" y="${y + (small ? 0 : 4)}" width="${hs}" height="${hs}" rx="5" fill="${INK}"/>`;
    const t = small ? "M95 84 L171 128 L95 172 Z" : "M101 94 L165 128 L101 162 Z";
    return s + `<path d="${t}" fill="${INK}" stroke="${INK}" stroke-width="14" stroke-linejoin="round"/>`;
  },
  // A waveform: mirrored bars about the middle, as the audio view draws it, symmetric and centred in the square
  // both ways (Faye, 2026-11-11: it was lopsided and sat high).
  audio: (ext, small) => {
    // The same five bars at every size, the large ones a touch slimmer (Faye, 2026-11-11: the large one had
    // nine, flatter).
    const hs = [0.4, 0.75, 1, 0.75, 0.4];
    const bw = small ? 26 : 22, pitch = small ? 38 : 36;
    const x0 = 128 - ((hs.length - 1) * pitch + bw) / 2, top = 128, amp = small ? 82 : 78;
    let s = `<rect x="12" y="12" width="232" height="232" rx="30" fill="${RED}"/>`;
    hs.forEach((h, i) => {
      const half = Math.max(bw / 2, h * amp);
      s += `<rect x="${(x0 + i * pitch).toFixed(1)}" y="${(top - half).toFixed(1)}" width="${bw}" height="${(half * 2).toFixed(1)}" rx="${bw / 2}" fill="${INK}"/>`;
    });
    return s;
  },
  anim: (ext, small) => `<rect x="${small ? 56 : 52}" y="16" width="${small ? 188 : 192}" height="160" rx="24" fill="${DARK}"/>
    ${small ? "" : `<rect x="32" y="36" width="192" height="160" rx="24" fill="${RED}" opacity=".75"/>`}
    <rect x="12" y="${small ? 72 : 60}" width="${small ? 196 : 192}" height="${small ? 172 : 176}" rx="24" fill="${RED}"/>` + scene(12, small ? 72 : 60, small ? 196 : 192, small ? 172 : 176, small),
  vector: (ext, small) => {
    let s = `<rect x="14" y="14" width="228" height="228" rx="30" fill="${RED}"/>`;
    if (!small) s += `<path d="M58 186 L92 84 M198 70 L164 172" stroke="${INK}" stroke-width="6" opacity=".7"/><circle cx="92" cy="84" r="11" fill="${INK}"/><circle cx="164" cy="172" r="11" fill="${INK}"/>`;
    s += `<path d="M58 186 C92 84 164 172 198 70" fill="none" stroke="${INK}" stroke-width="${small ? 22 : 14}" stroke-linecap="round"/>`;
    const a = small ? 46 : 32;
    return s + `<rect x="${58 - a / 2}" y="${186 - a / 2}" width="${a}" height="${a}" rx="5" fill="${INK}"/><rect x="${198 - a / 2}" y="${70 - a / 2}" width="${a}" height="${a}" rx="5" fill="${INK}"/>`;
  },
  layers: (ext, small) => {
    const d = y => `M128 ${y - 60} L240 ${y} L128 ${y + 60} L16 ${y} Z`;
    const gap = small ? 52 : 44;
    return `<path d="${d(128 + gap)}" fill="${DEEP}" stroke="${DEEP}" stroke-width="10" stroke-linejoin="round"/>
      <path d="${d(128)}" fill="${DARK}" stroke="${DARK}" stroke-width="10" stroke-linejoin="round"/>
      <path d="${d(128 - gap)}" fill="${RED}" stroke="${RED}" stroke-width="10" stroke-linejoin="round"/>`;
  },
  doc: (ext, small) => {
    const s = `<path d="M50 10 H166 L218 62 V232 Q218 246 204 246 H64 Q50 246 50 232 V24 Q50 10 64 10 Z" fill="${RED}"/>
      <path d="M166 10 V50 Q166 62 178 62 H218 Z" fill="${DARK}"/>`;
    if (small) return s + `<rect x="80" y="104" width="108" height="20" rx="8" fill="${INK}"/><rect x="80" y="146" width="108" height="20" rx="8" fill="${INK}"/><rect x="80" y="188" width="72" height="20" rx="8" fill="${INK}"/>`;
    // Lines of text down the page; the extension is the black tag at the bottom right, as on every other kind
    // (Faye, 2026-11-11: it was the extension in the middle over the Looker mark).
    // Three even lines, the last shorter, as the small icon has (Faye, 2026-11-11: five uneven ones looked random).
    const line = (y, w) => `<rect x="80" y="${y}" width="${w}" height="16" rx="8" fill="${INK}"/>`;
    return s + line(82, 108) + line(116, 108) + line(150, 72);
  },
  model: () => `<g stroke-linejoin="round" stroke-width="10">
    <path d="M128 20 L228 74 L128 128 L28 74 Z" fill="${LIGHT}" stroke="${LIGHT}"/>
    <path d="M28 74 L128 128 L128 238 L28 184 Z" fill="${RED}" stroke="${RED}"/>
    <path d="M228 74 L128 128 L128 238 L228 184 Z" fill="${DARK}" stroke="${DARK}"/></g>`,
  material: (ext, small) => `<defs><radialGradient id="sph" cx="38%" cy="32%" r="72%">
    <stop offset="0" stop-color="#FF9A8F"/><stop offset=".35" stop-color="${RED}"/><stop offset="1" stop-color="${DEEP}"/></radialGradient></defs>
    ${small ? "" : `<ellipse cx="128" cy="230" rx="84" ry="12" fill="${DEEP}" opacity=".35"/>`}
    <circle cx="128" cy="${small ? 128 : 120}" r="${small ? 116 : 104}" fill="url(#sph)"/>
    ${small ? "" : `<ellipse cx="92" cy="82" rx="22" ry="14" fill="#fff" opacity=".55" transform="rotate(-30 92 82)"/>`}`,
  icon: (ext, small) => {
    let s = `<rect x="12" y="12" width="232" height="232" rx="26" fill="${RED}"/>`;
    const n = small ? 4 : 6, c = 176 / n;
    for (let r = 0; r < n; r++) for (let q = 0; q < n; q++) if ((r + q) % 2 === 0) s += `<rect x="${40 + q * c}" y="${40 + r * c}" width="${c}" height="${c}" fill="${INK}"/>`;
    return s;
  },
};

// The extension tag, bottom right.
function tag(ext) {
  // As wide as the text plus even padding (the text shrinks only if it would pass 242).
  const w = Math.min(242, textWidth(ext, 50) + 44);
  return `<rect x="${250 - w}" y="178" width="${w}" height="72" rx="16" fill="#141414" stroke="#ffffff" stroke-width="5"/>` + fitText(ext, 250 - w / 2, 214, w - 44, 50, "#fff");
}

function kindOf(ext) {
  for (const [k, list] of Object.entries(KIND_OF)) if (list.split(" ").includes(ext)) return k;
  return null;
}

// The icon for `ext` (lower case) at `size` px, as an SVG document.
function iconSvg(ext, size) {
  const kind = kindOf(ext), E = ext.toUpperCase();
  const small = size <= 32;
  let body = DRAW[kind](E, small);
  if (size >= 48) body += tag(E);
  return `<svg xmlns="http://www.w3.org/2000/svg" width="${size}" height="${size}" viewBox="0 0 256 256">${body}</svg>`;
}
