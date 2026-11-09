//! Colour tokens: three themes picked by hand, built the way Snapper's are (the same tokens, the same
//! lightness steps) but in Looker's red: Ember (the default: nearly black, a faint warm tint), Garnet (dark,
//! on deep wine) and Blush (light). Each has a "raised" variant for popups, menus, dialogs and tooltips.
//!
//! The tint is the page's and the popups' only (the page behind the photo and the landing page, menus,
//! dialogs, tooltips): the cards, the Settings card, every control (buttons, chips, rows, tiles, fields,
//! switches, their hairlines) and all text and icons are neutral greys, as the old Looker's were, so nothing but
//! the accent reads red.
//!
//! The UI is drawn on one thread, so the theme in use is a global that the window sets from the settings
//! (`set`) and every widget reads (`t`, `raised`).

use std::sync::atomic::{AtomicU8, Ordering};

use windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F;

use crate::gfx::rgb;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ThemeId {
    Ember,
    Garnet,
    Blush,
}

impl ThemeId {
    /// Picker order: darkest to lightest.
    pub const ALL: [ThemeId; 3] = [ThemeId::Ember, ThemeId::Garnet, ThemeId::Blush];

    pub fn name(self) -> &'static str {
        match self {
            ThemeId::Ember => "Ember",
            ThemeId::Garnet => "Garnet",
            ThemeId::Blush => "Blush",
        }
    }

    /// A theme by name. The two themes before these read as the nearest: Black as Ember, Dark grey (written
    /// `dark_grey`) as Garnet.
    pub fn parse(s: &str) -> Option<ThemeId> {
        let gone = [("black", ThemeId::Ember), ("dark_grey", ThemeId::Garnet)];
        ThemeId::ALL.into_iter().map(|t| (t.name(), t)).chain(gone).find(|(n, _)| n.eq_ignore_ascii_case(s)).map(|(_, t)| t)
    }

    pub fn theme(self) -> &'static Theme {
        match self {
            ThemeId::Ember => &EMBER,
            ThemeId::Garnet => &GARNET,
            ThemeId::Blush => &BLUSH,
        }
    }

    /// For popups: firmer dividers, and in the light theme a greyed field.
    pub fn raised(self) -> &'static Theme {
        match self {
            ThemeId::Ember => &EMBER_RAISED,
            ThemeId::Garnet => &GARNET_RAISED,
            ThemeId::Blush => &BLUSH_RAISED,
        }
    }
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
    /// Every other row of a list, a shade off `content` (kept with Snapper's tokens; Looker has no striped list
    /// yet).
    #[allow(dead_code)]
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
}

// Nearly black, warmed a touch; its greys darker than Garnet's, close to the old Looker's black theme.
pub const EMBER: Theme = Theme {
    dark: true,
    ink: 0xFFFFFF,
    sidebar: 0x0A0A0A,
    content: 0x060303,
    stripe: 0x171010,
    divider: 0x242424,
    track: 0x2C2C2C,
    text: 0xEEEEEE,
    label: 0xACACAC,
    placeholder: 0x878787,
    icon: 0xA4A4A4,
    accent: 0xF52524,
    on_accent: 0xFFFFFF,
    accent_fill: 0x5A2626,
    hover_fill: 0x242424,
    row_select: 0x2C2C2C,
    warn: 0xF0A040,
    signal: 0xFF6B6A,
    close_hover: 0xC42B1C,
    popup: 0x2A1C1C,
    field: 0x0F0F0F,
    button: 0x161616,
};
pub const EMBER_RAISED: Theme = Theme { divider: 0x333333, track: 0x383838, field: 0x121212, button: 0x1F1F1F, hover_fill: 0x2C2C2C, row_select: 0x363636, ..EMBER };

// Dark, on deep wine.
pub const GARNET: Theme = Theme {
    dark: true,
    ink: 0xFFFFFF,
    sidebar: 0x222222,
    content: 0x1C1313,
    stripe: 0x221818,
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
    popup: 0x322323,
    field: 0x262626,
    button: 0x2E2E2E,
};
pub const GARNET_RAISED: Theme = Theme { divider: 0x4E4E4E, track: 0x555555, field: 0x2B2B2B, button: 0x3A3A3A, hover_fill: 0x4A4A4A, row_select: 0x555555, ..GARNET };

// Light, on a pale blush: a deeper red for the accent, so white text reads on it.
pub const BLUSH: Theme = Theme {
    dark: false,
    ink: 0x000000,
    sidebar: 0xF3F3F3,
    content: 0xFBF4F4,
    stripe: 0xF6EAEA,
    divider: 0xD9D9D9,
    track: 0xD4D4D4,
    text: 0x202020,
    label: 0x595959,
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
    popup: 0xFFFAFA,
    field: 0xFFFFFF,
    button: 0xEEEEEE,
};
pub const BLUSH_RAISED: Theme = Theme { divider: 0xE2E2E2, track: 0xD8D8D8, field: 0xF4F4F4, ..BLUSH };

static CURRENT: AtomicU8 = AtomicU8::new(0);

/// Makes `id` the theme every widget draws in.
pub fn set(id: ThemeId) {
    CURRENT.store(id as u8, Ordering::Relaxed);
}

pub fn id() -> ThemeId {
    ThemeId::ALL[CURRENT.load(Ordering::Relaxed) as usize % ThemeId::ALL.len()]
}

/// The theme in use.
pub fn t() -> &'static Theme {
    id().theme()
}

/// The theme in use, raised (popups).
pub fn raised() -> &'static Theme {
    id().raised()
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

    #[test]
    fn text_meets_aa() {
        for id in ThemeId::ALL {
            for t in [id.theme(), id.raised()] {
                for bg in [t.content, t.sidebar, t.popup, t.field] {
                    assert!(contrast(t.text, bg) >= 7.0, "{id:?} text on {bg:06X}");
                    assert!(contrast(t.label, bg) >= 4.5, "{id:?} label on {bg:06X}");
                    assert!(contrast(t.placeholder, bg) >= 4.5, "{id:?} placeholder on {bg:06X}");
                }
            }
        }
    }

    #[test]
    fn old_names_still_read() {
        assert_eq!(ThemeId::parse("black"), Some(ThemeId::Ember));
        assert_eq!(ThemeId::parse("dark_grey"), Some(ThemeId::Garnet));
        for id in ThemeId::ALL {
            assert_eq!(ThemeId::parse(id.name()), Some(id));
        }
    }
}
