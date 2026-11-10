//! Colour tokens: four themes picked by hand, named as Snapper's are (Faye, 2026-11-10; they were Ember, Garnet
//! and Blush): Black (the default: pure black), Dark (on a deep shade of the accent), Paper (Snapper's warm cream)
//! and Light. Each has a "raised" variant for popups, menus, dialogs and tooltips.
//!
//! The tint is the page's only (the page behind the photo and the landing page): the popups (menus, dialogs,
//! tooltips; Faye, 2026-11-09), the cards, the Settings card, every control (buttons, chips, rows, tiles, fields,
//! switches, their hairlines) and all text and icons are neutral greys, as the old Looker's were, so nothing but
//! the accent shows colour. Paper is warm all through, as Snapper's.
//!
//! Each theme comes in any of Snapper's accents (Orchid, Lime, Sky, Crimson, White, or Custom: any colour
//! picked), Crimson, Looker's red, the default. The tokens below are Crimson's as drawn; `derive` works out the
//! others the way Snapper does, from a few colours each (`HUES`, or `custom_hue`): the accent itself, the fills
//! made from it (a text selection, a picked control) in its hue at the same luminance, and the page's tint turned
//! to its hue, so contrast holds in every one (a test checks, Custom in awkward colours too).
//!
//! The UI is drawn on one thread, so the theme and accent in use are globals that the window sets from the
//! settings (`set`, `set_accent`, `set_custom`) and every widget reads (`t`, `raised`).

use std::sync::atomic::{AtomicU32, AtomicU8, Ordering};

use windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F;

use crate::gfx::rgb;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ThemeId {
    Black,
    Dark,
    Light,
    Paper,
}

impl ThemeId {
    /// Picker order: darkest to lightest.
    pub const ALL: [ThemeId; 4] = [ThemeId::Black, ThemeId::Dark, ThemeId::Paper, ThemeId::Light];

    pub fn name(self) -> &'static str {
        match self {
            ThemeId::Black => "Black",
            ThemeId::Dark => "Dark",
            ThemeId::Light => "Light",
            ThemeId::Paper => "Paper",
        }
    }

    fn from_u8(v: u8) -> ThemeId {
        match v {
            1 => ThemeId::Dark,
            2 => ThemeId::Light,
            3 => ThemeId::Paper,
            _ => ThemeId::Black,
        }
    }

    /// A theme by name, old ones too: Ember, Garnet and Blush (until 2026-11-10), and Dark grey (`dark_grey`)
    /// from before them.
    pub fn parse(s: &str) -> Option<ThemeId> {
        let old = [("ember", ThemeId::Black), ("garnet", ThemeId::Dark), ("blush", ThemeId::Light), ("dark_grey", ThemeId::Dark)];
        ThemeId::ALL.into_iter().map(|t| (t.name(), t)).chain(old).find(|(n, _)| n.eq_ignore_ascii_case(s)).map(|(_, t)| t)
    }

    /// The theme in the accent picked (`set_accent`).
    pub fn theme(self) -> &'static Theme {
        self.in_accent(accent())
    }

    /// For popups: firmer dividers, and in the light theme a greyed field.
    pub fn raised(self) -> &'static Theme {
        &themes(accent())[1][self as usize]
    }

    /// The theme in `accent`, whichever is picked: the Appearance page's accent swatches.
    pub fn in_accent(self, accent: Accent) -> &'static Theme {
        &themes(accent)[0][self as usize]
    }
}

/// The colour of highlights in every theme: the accent buttons, selections, switches, the slider's and the seek
/// bar's fill. Picked in Settings, under Appearance, as Snapper's.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Accent {
    Orchid,
    Lime,
    Sky,
    Crimson,
    White,
    /// Any colour, picked in Settings (`set_custom`).
    Custom,
}

impl Accent {
    pub const ALL: [Accent; 6] = [Accent::Orchid, Accent::Lime, Accent::Sky, Accent::Crimson, Accent::White, Accent::Custom];
    /// Looker's red.
    pub const DEFAULT: Accent = Accent::Crimson;
    /// The order the Appearance page shows them in: Looker's red first (Faye, 2026-11-10). `ALL` keeps `HUES`'s.
    pub const PICKER: [Accent; 6] = [Accent::Crimson, Accent::Orchid, Accent::Lime, Accent::Sky, Accent::White, Accent::Custom];

    pub fn name(self) -> &'static str {
        match self {
            Accent::Orchid => "Orchid",
            Accent::Lime => "Lime",
            Accent::Sky => "Sky",
            Accent::Crimson => "Crimson",
            Accent::White => "White",
            Accent::Custom => "Custom",
        }
    }

    pub fn parse(s: &str) -> Option<Accent> {
        Accent::ALL.into_iter().find(|a| a.name().eq_ignore_ascii_case(s))
    }

    /// Its accent colour in `theme`, for the picker's swatches.
    pub fn colour(self, theme: ThemeId) -> u32 {
        theme.in_accent(self).accent
    }
}

/// The accent every theme is drawn in, for the whole app like the theme.
static ACCENT: AtomicU8 = AtomicU8::new(Accent::DEFAULT as u8);

pub fn set_accent(a: Accent) {
    ACCENT.store(a as u8, Ordering::Relaxed);
}

pub fn accent() -> Accent {
    Accent::ALL[(ACCENT.load(Ordering::Relaxed) as usize).min(Accent::ALL.len() - 1)]
}

/// Custom's colour until one's picked: a teal, unlike the others (Snapper's).
pub const DEFAULT_CUSTOM: u32 = 0x2EC4B6;

static CUSTOM: AtomicU32 = AtomicU32::new(DEFAULT_CUSTOM);

pub fn set_custom(c: u32) {
    CUSTOM.store(c & 0xFF_FFFF, Ordering::Relaxed);
}

pub fn custom() -> u32 {
    CUSTOM.load(Ordering::Relaxed)
}

#[derive(Clone, Copy, Debug)]
pub struct Theme {
    pub dark: bool,
    /// Faint overlays (a subtle button's hover, a link's wash) are this at a low opacity: white in the dark
    /// themes, black in the light one, so they stay grey.
    pub ink: u32,
    /// The cards' fill (the file explorer, the info card) and the Settings card's: neutral grey, like the
    /// controls, so nothing that holds content takes the tint.
    pub sidebar: u32,
    /// The page: darker, under the toolbar's buttons and behind the landing page.
    pub content: u32,
    /// Every other row of the explorer's list: an opaque shade off the card's `sidebar`, as Transient's.
    pub stripe: u32,
    pub divider: u32,
    /// A switch's or slider's empty track.
    pub track: u32,
    pub text: u32,
    /// Secondary text: hints, labels beside values.
    pub label: u32,
    pub placeholder: u32,
    pub icon: u32,
    /// Looker's red.
    pub accent: u32,
    pub on_accent: u32,
    /// A text selection.
    pub accent_fill: u32,
    pub hover_fill: u32,
    /// A picked row or toggle, under its accent outline.
    pub row_select: u32,
    #[allow(dead_code)]
    pub warn: u32,
    /// A brighter red for fills that read as "signal" (a slider's fill, focus rings).
    pub signal: u32,
    pub close_hover: u32,
    pub popup: u32,
    pub field: u32,
    /// An idle button's or chip's fill (opaque, so it's the same grey on any surface).
    pub button: u32,
    /// The checkerboard's two squares behind the photo (neutral greys a hair apart).
    pub checker: [u32; 2],
    /// The explorer's browse sidebar: a step further from the page than the cards, darker in the dark themes and
    /// lighter in the light ones, so it stands apart from the tabbed list beside it (Faye, 2026-11-10).
    pub pane: u32,
    /// The title bar and status row when Appearance makes them solid: neutral (no page tint), Black's black.
    pub bar: u32,
    /// A button that's on (a panel's toggle, the picked theme chip), the file the explorer is on and the
    /// Settings section open: `button` tinted with the accent (under an accent outline, on the buttons and
    /// rows). The one control fill that's red.
    pub selected: u32,
}

// Black, after Transient's Black (Faye, 2026-11-09: its contrast, Looker's red): the page pure black, the
// cards a step above, hairlines, tracks and fields firm enough to read, text near white.
pub const BLACK: Theme = Theme {
    dark: true,
    ink: 0xFFFFFF,
    sidebar: 0x0A0A0A,
    content: 0x000000,
    stripe: 0x151515,
    divider: 0x262626,
    track: 0x2E2E2E,
    text: 0xF2F2F2,
    label: 0xB0B0B0,
    placeholder: 0x878787,
    icon: 0xA8A8A8,
    accent: 0xF52524,
    on_accent: 0xFFFFFF,
    accent_fill: 0x4A1E1E,
    hover_fill: 0x1E1E1E,
    row_select: 0x262626,
    warn: 0xF0A040,
    signal: 0xFF6B6A,
    close_hover: 0xC42B1C,
    popup: 0x1D1D1D,
    field: 0x131313,
    button: 0x131313,
    selected: 0x401616,
    checker: [0x070707, 0x0A0A0A],
    bar: 0x000000,
    pane: 0x000000,
};
pub const BLACK_RAISED: Theme = Theme { divider: 0x363636, track: 0x3A3A3A, field: 0x161616, button: 0x262626, hover_fill: 0x2C2C2C, row_select: 0x323232, ..BLACK };

// Dark, on deep wine.
pub const DARK: Theme = Theme {
    dark: true,
    ink: 0xFFFFFF,
    sidebar: 0x222222,
    content: 0x1C1313,
    stripe: 0x2A2A2A,
    divider: 0x3A3A3A,
    track: 0x444444,
    text: 0xEAEAEA,
    label: 0xB0B0B0,
    placeholder: 0x959595,
    icon: 0xA5A5A5,
    accent: 0xF52524,
    on_accent: 0xFFFFFF,
    accent_fill: 0x6A3030,
    hover_fill: 0x404040,
    row_select: 0x4A4A4A,
    warn: 0xF0A040,
    signal: 0xFF6B6A,
    close_hover: 0xC42B1C,
    popup: 0x2C2C2C,
    field: 0x262626,
    button: 0x2E2E2E,
    selected: 0x602C2C,
    checker: [0x0F0F0F, 0x131313],
    bar: 0x222222,
    pane: 0x181818,
};
pub const DARK_RAISED: Theme = Theme { divider: 0x4E4E4E, track: 0x555555, field: 0x2B2B2B, button: 0x3A3A3A, hover_fill: 0x4A4A4A, row_select: 0x555555, ..DARK };

// Light, on a pale blush: a deeper red for the accent, so white text reads on it.
pub const LIGHT: Theme = Theme {
    dark: false,
    ink: 0x000000,
    sidebar: 0xF3F3F3,
    content: 0xFBF4F4,
    stripe: 0xEAEAEA,
    divider: 0xD9D9D9,
    track: 0xD4D4D4,
    text: 0x1F1F1F,
    label: 0x555555,
    placeholder: 0x686868,
    icon: 0x5F5F5F,
    accent: 0xD21E1E,
    on_accent: 0xFFFFFF,
    accent_fill: 0xF2C9C9,
    hover_fill: 0xE4E4E4,
    row_select: 0xDCDCDC,
    warn: 0xB35C00,
    signal: 0xD21E1E,
    close_hover: 0xC42B1C,
    popup: 0xFFFFFF,
    field: 0xFFFFFF,
    button: 0xEEEEEE,
    selected: 0xF2CACA,
    checker: [0xECECEC, 0xF1F1F1],
    bar: 0xF3F3F3,
    pane: 0xFBFBFB,
};
pub const LIGHT_RAISED: Theme = Theme { divider: 0xE2E2E2, track: 0xD8D8D8, field: 0xF4F4F4, ..LIGHT };

// Paper, after Snapper's (Faye, 2026-11-10): light on warm off-white, its text and washes a warm ink. Unlike
// the others its cards, popups and controls are warm too (Snapper's Paper is cream all through; neutral greys
// on it read cold), and so is its checkerboard. Snapper's Paper tokens, with Looker's light red for the accent.
pub const PAPER: Theme = Theme {
    dark: false,
    ink: 0x2B2620,
    sidebar: 0xE8E2D5,
    content: 0xF1ECE1,
    stripe: 0xEAE4D7,
    divider: 0xD3CBBA,
    track: 0xCCC3B1,
    text: 0x2B2620,
    label: 0x5A5247,
    placeholder: 0x686054,
    icon: 0x60584D,
    accent: 0xD21E1E,
    on_accent: 0xFFFFFF,
    accent_fill: 0xEDC6BC,
    hover_fill: 0xDED6C6,
    row_select: 0xD6CDBC,
    warn: 0xB35C00,
    signal: 0xD21E1E,
    close_hover: 0xC42B1C,
    popup: 0xF8F4EB,
    field: 0xF8F4EB,
    button: 0xE3DCCE,
    selected: 0xEFCDC3,
    checker: [0xE7E1D5, 0xECE6DB],
    bar: 0xE8E2D5,
    pane: 0xF3EEE4,
};
pub const PAPER_RAISED: Theme = Theme { divider: 0xDDD6C7, track: 0xD6CEBD, field: 0xECE6DA, ..PAPER };

// --- Accents (Snapper's, worked out Looker's way) -------------------------------------------------------------

/// An accent's own colours (Snapper's `HUES`): everything in a theme that follows the accent comes from these.
struct Hue {
    /// In the dark themes: the accent, the stronger ink for washes (unused here: Looker's washes stay neutral),
    /// the brighter signal, and the text that goes on the accent.
    dark: [u32; 4],
    /// In Light: the accent (deep), the ink, and the text that goes on the accent.
    light: [u32; 3],
    /// Paper's accent, a step deeper to read on the cream.
    paper: u32,
    /// The accent and signal as picked, not softened (`STRENGTH`).
    exact: bool,
}

/// The accents with colours of their own, in `Accent` order (Custom is worked out). Snapper's, Crimson its take
/// on Looker's red (Looker's own tokens are used for Crimson as they are: `derive`).
const HUES: [Hue; 5] = [
    Hue { dark: [0xDFA2E3, 0xE27AEA, 0xEDB6F0, 0x2A1530], light: [0xA8519E, 0xB0409F, 0xFFFFFF], paper: 0x9C4592, exact: false },
    Hue { dark: [0xC2EC3A, 0xB4E33A, 0xD4F466, 0x1A2400], light: [0x4E7A00, 0x5A8C00, 0xFFFFFF], paper: 0x4A7400, exact: true },
    Hue { dark: [0x9EC4F2, 0x6EA6F4, 0xBAD6F7, 0x0F213A], light: [0x2F68B0, 0x2C6DCC, 0xFFFFFF], paper: 0x2B60A4, exact: false },
    Hue { dark: [0xE81E22, 0xF52524, 0xFF6B6A, 0xFFFFFF], light: [0xD21E1E, 0xE0201F, 0xFFFFFF], paper: 0xD21E1E, exact: true },
    Hue { dark: [0xEEEEEE, 0xFFFFFF, 0xFFFFFF, 0x141414], light: [0x2B2B2B, 0x000000, 0xFFFFFF], paper: 0x2B2620, exact: true },
];

/// How colourful the accent is against the colours picked above (Snapper's: "a bit less strong overall").
const STRENGTH: f32 = 0.8;

/// A colour with less chroma than this counts as none (White, a grey picked for Custom): the page's tint goes
/// neutral rather than taking a hue it hasn't got.
const NEUTRAL: f32 = 0.03;

/// `base` (one of the Crimson-drawn themes above, plain or raised) in `h`: the accent, its signal and the text on
/// it from `h`; the fills made from the accent (a text selection, a picked control) in its hue at the base's
/// luminance and colourfulness (never more than the accent has); and Dark's and Light's page tint turned to its
/// hue at the same luminance. Everything neutral stays so; Black's page is black, Paper's cream is its own.
fn derive(base: &Theme, id: ThemeId, a: Accent, h: &Hue) -> Theme {
    if a == Accent::Crimson {
        return *base; // the themes as drawn
    }
    let mut t = *base;
    let neutral = chroma(h.dark[0]) < NEUTRAL;
    let solid = |c: u32| if h.exact { c } else { soften(c) };
    if t.dark {
        (t.accent, t.signal, t.on_accent) = (solid(h.dark[0]), solid(h.dark[2]), h.dark[3]);
    } else {
        t.accent = solid(if id == ThemeId::Paper { h.paper } else { h.light[0] });
        t.signal = t.accent;
        t.on_accent = h.light[2];
    }
    let hue = if t.dark { settle(oklch_hue(t.accent)) } else { oklch_hue(t.accent) };
    let most = chroma(t.accent);
    t.accent_fill = tone(luminance(base.accent_fill), chroma(base.accent_fill).min(most), hue);
    t.selected = tone(luminance(base.selected), chroma(base.selected).min(most), hue);
    if matches!(id, ThemeId::Dark | ThemeId::Light) {
        t.content = retint(base.content, oklch_hue(h.dark[0]), if neutral { 0.0 } else { 1.0 });
    }
    t
}

/// A hue (OKLCH radians) for a dark fill: the ones that go muddy dark move off it a little (Snapper's).
fn settle(hue: f32) -> f32 {
    let d = hue.to_degrees().rem_euclid(360.0);
    let to = if (95.0..=135.0).contains(&d) {
        d + (135.0 - d) * 0.5 + 6.0
    } else if (40.0..95.0).contains(&d) {
        d - (d - 40.0) * 0.35 - 6.0
    } else if (10.0..40.0).contains(&d) {
        d - 4.0
    } else {
        d
    };
    to.to_radians()
}

/// `c` at `STRENGTH` of its colourfulness, as light as it was.
fn soften(c: u32) -> u32 {
    retint(c, oklch_hue(c), STRENGTH)
}

/// The colours for Custom, from the colour picked (Snapper's): as it is wherever it can be, lightened in the dark
/// themes until it clears 3:1 on their pages, darkened in the light ones likewise, and kept out of the band where
/// neither white nor near-black text on it reads at 4.5:1.
fn custom_hue(c: u32) -> Hue {
    let dark = if luminance(c) < 0.14 { relight(c, 0.14) } else { c };
    let (dark, dark_on) = readable(dark, true);
    let light = if luminance(c) > 0.21 { relight(c, 0.21) } else { c };
    let (light, light_on) = readable(light, false);
    let signal = relight(dark, ((luminance(dark) + 0.05) * 1.3 - 0.05).min(0.9));
    Hue { dark: [dark, dark, signal, dark_on], light: [light, light, light_on], paper: light, exact: true }
}

/// `c` with the text that reads on it: white if it's dark enough, near-black if it's light enough, and in between
/// moved (up if `up`, else down) until one of them does.
fn readable(c: u32, up: bool) -> (u32, u32) {
    let y = luminance(c);
    if y <= 0.18 {
        (c, 0xFFFFFF)
    } else if y >= 0.212 {
        (c, 0x141414)
    } else if up {
        (relight(c, 0.214), 0x141414)
    } else {
        (relight(c, 0.176), 0xFFFFFF)
    }
}

/// `c` lightened or darkened to luminance `y`, its hue and as much of its colour as fits.
fn relight(c: u32, y: f32) -> u32 {
    tone(y, chroma(c), oklch_hue(c))
}

/// Every theme, plain and raised (in `ThemeId` order), in accent `a`: the fixed ones worked out once.
fn themes(a: Accent) -> &'static [[Theme; 4]; 2] {
    if a == Accent::Custom {
        return custom_themes(custom());
    }
    static TABLE: std::sync::OnceLock<[[[Theme; 4]; 2]; 5]> = std::sync::OnceLock::new();
    &TABLE.get_or_init(|| std::array::from_fn(|i| build(Accent::ALL[i], &HUES[i])))[a as usize]
}

/// The themes for a Custom colour, worked out the first time it's asked for and kept (only the colours actually
/// picked, or dragged through in the picker).
fn custom_themes(c: u32) -> &'static [[Theme; 4]; 2] {
    static CACHE: std::sync::Mutex<Vec<(u32, &'static [[Theme; 4]; 2])>> = std::sync::Mutex::new(Vec::new());
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, t)) = cache.iter().find(|(k, _)| *k == c) {
        return t;
    }
    let t: &'static [[Theme; 4]; 2] = Box::leak(Box::new(build(Accent::Custom, &custom_hue(c))));
    cache.push((c, t));
    t
}

/// The four themes, plain and raised, in `ThemeId` order, in `h`.
fn build(a: Accent, h: &Hue) -> [[Theme; 4]; 2] {
    let bases = [[BLACK, DARK, LIGHT, PAPER], [BLACK_RAISED, DARK_RAISED, LIGHT_RAISED, PAPER_RAISED]];
    bases.map(|row| {
        let mut i = 0u8;
        row.map(|base| {
            let id = ThemeId::from_u8(i);
            i += 1;
            derive(&base, id, a, h)
        })
    })
}

// --- Colour arithmetic (Snapper's) ----------------------------------------------------------------------------

fn channels(c: u32) -> [f32; 3] {
    [(c >> 16) & 0xFF, (c >> 8) & 0xFF, c & 0xFF].map(|v| v as f32 / 255.0)
}

fn to_linear(v: f32) -> f32 {
    if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
}

fn from_linear(v: f32) -> u32 {
    let v = v.clamp(0.0, 1.0);
    let s = if v <= 0.0031308 { 12.92 * v } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 };
    (s * 255.0).round() as u32
}

/// WCAG relative luminance.
fn luminance(c: u32) -> f32 {
    let [r, g, b] = channels(c).map(to_linear);
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

fn to_oklab(c: u32) -> [f32; 3] {
    let [r, g, b] = channels(c).map(to_linear);
    let l = (0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b).cbrt();
    let m = (0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b).cbrt();
    let s = (0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b).cbrt();
    [
        0.2104542553 * l + 0.7936177850 * m - 0.0040720468 * s,
        1.9779984951 * l - 2.4285922050 * m + 0.4505937099 * s,
        0.0259040371 * l + 0.7827717662 * m - 0.8086757660 * s,
    ]
}

/// OKLab to linear sRGB, unclamped (outside 0..1 means sRGB can't show it).
fn oklab_linear([l, a, b]: [f32; 3]) -> [f32; 3] {
    let lc = (l + 0.3963377774 * a + 0.2158037573 * b).powi(3);
    let mc = (l - 0.1055613458 * a - 0.0638541728 * b).powi(3);
    let sc = (l - 0.0894841775 * a - 1.2914855480 * b).powi(3);
    [
        4.0767416621 * lc - 3.3077115913 * mc + 0.2309699292 * sc,
        -1.2684380046 * lc + 2.6097574011 * mc - 0.3413193965 * sc,
        -0.0041960863 * lc - 0.7034186147 * mc + 1.7076147010 * sc,
    ]
}

fn from_oklab(lab: [f32; 3]) -> u32 {
    let [r, g, b] = oklab_linear(lab).map(from_linear);
    (r << 16) | (g << 8) | b
}

/// A colour's hue in OKLCH, in radians.
fn oklch_hue(c: u32) -> f32 {
    let [_, a, b] = to_oklab(c);
    b.atan2(a)
}

/// A colour's chroma in OKLCH.
fn chroma(c: u32) -> f32 {
    let [_, a, b] = to_oklab(c);
    a.hypot(b)
}

/// `c` turned to hue `deg` (OKLCH radians), `k` as colourful as it was, then lightened or darkened until its
/// luminance is what it was.
fn retint(c: u32, deg: f32, k: f32) -> u32 {
    let [_, a, b] = to_oklab(c);
    let chroma = a.hypot(b) * k;
    let (na, nb) = (chroma * deg.cos(), chroma * deg.sin());
    let target = luminance(c);
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    for _ in 0..40 {
        let mid = (lo + hi) / 2.0;
        if luminance(from_oklab([mid, na, nb])) < target {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    from_oklab([(lo + hi) / 2.0, na, nb])
}

/// The colour of luminance `y`, hue `hue` (OKLCH radians) and chroma `c`, less if sRGB can't show that much at that
/// lightness (rather than clipping, which shifts the hue).
fn tone(y: f32, c: f32, hue: f32) -> u32 {
    let at = |l: f32| {
        let mut k = c;
        while k > 0.0 && oklab_linear([l, k * hue.cos(), k * hue.sin()]).iter().any(|v| !(-0.0005..=1.0005).contains(v)) {
            k -= 0.002;
        }
        let k = k.max(0.0);
        from_oklab([l, k * hue.cos(), k * hue.sin()])
    };
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    for _ in 0..32 {
        let mid = (lo + hi) / 2.0;
        if luminance(at(mid)) < y {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    at((lo + hi) / 2.0)
}

static CURRENT: AtomicU8 = AtomicU8::new(0);

/// Makes `id` the theme every widget draws in.
pub fn set(id: ThemeId) {
    CURRENT.store(id as u8, Ordering::Relaxed);
}

pub fn id() -> ThemeId {
    ThemeId::from_u8(CURRENT.load(Ordering::Relaxed))
}

/// Whether Windows' app mode (Settings > Personalization > Colors) is light. Dark if it can't be read.
pub fn windows_light() -> bool {
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
    let mut v = 0u32;
    let mut size = std::mem::size_of::<u32>() as u32;
    let key = windows::core::w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize");
    let ok = unsafe { RegGetValueW(HKEY_CURRENT_USER, key, windows::core::w!("AppsUseLightTheme"), RRF_RT_REG_DWORD, None, Some(&mut v as *mut u32 as _), Some(&mut size)) }.is_ok();
    ok && v != 0
}

/// The theme in use.
pub fn t() -> &'static Theme {
    id().theme()
}

/// The theme in use, raised (popups).
pub fn raised() -> &'static Theme {
    id().raised()
}

/// `c` with no tint: the grey of its luminance (the home screen's page: Faye, 2026-11-10, untinted by the accent
/// in Dark, Paper and Light).
pub fn plain(c: u32) -> u32 {
    retint(c, 0.0, 0.0)
}

/// `a` to `b` by `k` (0..1), opaque.
pub fn mix(a: u32, b: u32, k: f32) -> D2D1_COLOR_F {
    let (a, b) = (rgb(a), rgb(b));
    let k = k.clamp(0.0, 1.0);
    D2D1_COLOR_F { r: a.r + (b.r - a.r) * k, g: a.g + (b.g - a.g) * k, b: a.b + (b.b - a.b) * k, a: 1.0 }
}


#[cfg(test)]
mod tests {
    use super::*;

    fn lum(c: u32) -> f64 {
        let ch = |v: u32| {
            let c = v as f64 / 255.0;
            if c <= 0.03928 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
        };
        0.2126 * ch((c >> 16) & 255) + 0.7152 * ch((c >> 8) & 255) + 0.0722 * ch(c & 255)
    }

    fn contrast(a: u32, b: u32) -> f64 {
        let (x, y) = (lum(a), lum(b));
        (x.max(y) + 0.05) / (x.min(y) + 0.05)
    }

    /// Some colours that could be picked for Custom, awkward ones included (Snapper's).
    const CUSTOMS: [u32; 8] = [0x000000, 0xFFFFFF, 0x808080, 0xFFFF00, 0x000080, 0x00FF00, 0xFF00FF, 0x7A6040];

    /// Every theme in every accent (Custom in all sorts of colours), plain and raised, keeps its contrast.
    #[test]
    fn every_accent_keeps_contrast() {
        let mut all: Vec<(String, &[[Theme; 4]; 2])> = Accent::ALL[..5].iter().map(|a| (a.name().to_string(), themes(*a))).collect();
        all.extend(CUSTOMS.iter().map(|c| (format!("Custom {c:06X}"), custom_themes(*c))));
        for (name, set) in all {
            for (raised, row) in set.iter().enumerate() {
                for id in ThemeId::ALL {
                    let t = &row[id as usize];
                    let at = format!("{} in {name}{}", id.name(), if raised == 1 { ", raised" } else { "" });
                    for bg in [t.content, t.sidebar, t.popup, t.field] {
                        assert!(contrast(t.text, bg) >= 7.0, "{at}: text on {bg:06X}");
                        assert!(contrast(t.label, bg) >= 4.5, "{at}: label on {bg:06X}");
                        assert!(contrast(t.placeholder, bg) >= 4.5, "{at}: placeholder on {bg:06X}");
                    }
                    for (bg, what) in [(t.accent_fill, "selection"), (t.selected, "picked")] {
                        assert!(contrast(t.text, bg) >= 4.45, "{at}: text on the {what} {:.2}", contrast(t.text, bg));
                    }
                    // Crimson is Looker's red as Faye picked it (white on F52524 is 4.06): held to that, the rest to 4.5.
                    let on_accent_min = if name == "Crimson" { 4.0 } else { 4.45 };
                    assert!(contrast(t.on_accent, t.accent) >= on_accent_min, "{at}: text on the accent {:.2}", contrast(t.on_accent, t.accent));
                    assert!(contrast(t.accent, t.content) >= 3.0, "{at}: accent on the page {:.2}", contrast(t.accent, t.content));
                }
            }
        }
    }

    /// Crimson is the themes as drawn: the default looks exactly as Looker did.
    #[test]
    fn crimson_is_as_drawn() {
        assert_eq!(themes(Accent::Crimson)[0][ThemeId::Dark as usize].content, DARK.content);
        assert_eq!(themes(Accent::Crimson)[0][ThemeId::Black as usize].accent, 0xF52524);
        assert_eq!(Accent::DEFAULT, Accent::Crimson);
    }

    #[test]
    fn old_names_still_read() {
        assert_eq!(ThemeId::parse("ember"), Some(ThemeId::Black));
        assert_eq!(ThemeId::parse("garnet"), Some(ThemeId::Dark));
        assert_eq!(ThemeId::parse("blush"), Some(ThemeId::Light));
        assert_eq!(ThemeId::parse("dark_grey"), Some(ThemeId::Dark));
        for id in ThemeId::ALL {
            assert_eq!(ThemeId::parse(id.name()), Some(id));
        }
    }

    #[test]
    fn stored_index_is_the_theme() {
        for id in ThemeId::ALL {
            set(id);
            assert_eq!(super::id(), id);
        }
        set(ThemeId::Black);
    }
}
