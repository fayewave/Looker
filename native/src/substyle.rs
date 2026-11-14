//! Subtitle styles: how plain-text subtitles look (font, colour, outline, shadow, a box behind them, size, where
//! they sit). Made and picked in the Subtitle styles card (`app/substyle.rs`), picked again from the Subtitles
//! menu; saved in the settings as `subtitle_style=` lines (`key=value` pairs split by tabs), the one in use as
//! `subtitle_style_active=`. ASS subtitles keep their file's own look and picture subtitles can't be restyled.

/// What sits behind the text.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Backdrop {
    None,
    /// A box behind each line, as TV captions have.
    Lines,
    /// One box behind all the lines.
    Block,
}

impl Backdrop {
    pub const ALL: [Backdrop; 3] = [Backdrop::None, Backdrop::Lines, Backdrop::Block];

    pub fn label(self) -> &'static str {
        match self {
            Backdrop::None => "None",
            Backdrop::Lines => "Behind each line",
            Backdrop::Block => "One box behind all",
        }
    }

    fn key(self) -> &'static str {
        match self {
            Backdrop::None => "none",
            Backdrop::Lines => "lines",
            Backdrop::Block => "block",
        }
    }
}

#[derive(Clone, PartialEq, Debug)]
pub struct SubStyle {
    pub name: String,
    /// The font family; `INTER` is Looker's own.
    pub font: String,
    /// 400, 500, 600 or 700 (`WEIGHTS`).
    pub weight: u16,
    pub italic: bool,
    /// Percent of the normal size (which follows the video's height).
    pub size: f32,
    pub colour: u32,
    /// Colours the subtitle file gives its words (SubRip's `<font color>`) are kept.
    pub file_colours: bool,
    /// The outline's width, percent of the text size (0: none).
    pub outline: f32,
    pub outline_colour: u32,
    /// The shadow's opacity, percent (0: none).
    pub shadow: f32,
    pub shadow_colour: u32,
    /// How far down the shadow falls and how soft it is, percent of the text size.
    pub shadow_distance: f32,
    pub shadow_blur: f32,
    pub backdrop: Backdrop,
    pub backdrop_colour: u32,
    /// Percent.
    pub backdrop_opacity: f32,
    /// The box's corners, percent of the line height.
    pub backdrop_round: f32,
    /// The gap below the lowest line, percent of the video's height.
    pub margin: f32,
}

pub const INTER: &str = "Inter";

/// The fonts offered, Inter first; only those installed show (`app/substyle.rs`).
pub const FONTS: [&str; 18] = [
    INTER,
    "Segoe UI",
    "Arial",
    "Verdana",
    "Tahoma",
    "Trebuchet MS",
    "Calibri",
    "Candara",
    "Corbel",
    "Bahnschrift",
    "Georgia",
    "Times New Roman",
    "Cambria",
    "Palatino Linotype",
    "Consolas",
    "Cascadia Code",
    "Comic Sans MS",
    "Impact",
];

pub const WEIGHTS: [(u16, &str); 4] = [(400, "Regular"), (500, "Medium"), (600, "Semibold"), (700, "Bold")];

/// A number a stepper sets: its place in `NUMBERS`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Num {
    Size,
    Outline,
    Shadow,
    ShadowDistance,
    ShadowBlur,
    BackdropOpacity,
    BackdropRound,
    Margin,
}

/// A colour a swatch sets.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Paint {
    Text,
    Outline,
    Shadow,
    Backdrop,
}

impl Paint {
    pub const ALL: [Paint; 4] = [Paint::Text, Paint::Outline, Paint::Shadow, Paint::Backdrop];

    pub fn title(self) -> &'static str {
        match self {
            Paint::Text => "Text colour",
            Paint::Outline => "Outline colour",
            Paint::Shadow => "Shadow colour",
            Paint::Backdrop => "Background colour",
        }
    }
}

impl Num {
    pub const ALL: [Num; 8] = [Num::Size, Num::Outline, Num::Shadow, Num::ShadowDistance, Num::ShadowBlur, Num::BackdropOpacity, Num::BackdropRound, Num::Margin];

    /// Lowest, highest, one step.
    pub fn range(self) -> (f32, f32, f32) {
        match self {
            Num::Size => (25.0, 400.0, 5.0),
            Num::Outline => (0.0, 20.0, 0.5),
            Num::Shadow => (0.0, 100.0, 5.0),
            Num::ShadowDistance => (0.0, 30.0, 1.0),
            Num::ShadowBlur => (0.0, 30.0, 1.0),
            Num::BackdropOpacity => (0.0, 100.0, 5.0),
            Num::BackdropRound => (0.0, 50.0, 5.0),
            Num::Margin => (0.0, 40.0, 1.0),
        }
    }

    /// The value as the stepper shows it.
    pub fn text(self, v: f32) -> String {
        match self {
            Num::Outline if v.fract() != 0.0 => format!("{v:.1}"),
            Num::Shadow | Num::BackdropOpacity | Num::Size => format!("{v:.0}%"),
            _ => format!("{v:.0}"),
        }
    }
}

impl SubStyle {
    pub fn get(&self, n: Num) -> f32 {
        match n {
            Num::Size => self.size,
            Num::Outline => self.outline,
            Num::Shadow => self.shadow,
            Num::ShadowDistance => self.shadow_distance,
            Num::ShadowBlur => self.shadow_blur,
            Num::BackdropOpacity => self.backdrop_opacity,
            Num::BackdropRound => self.backdrop_round,
            Num::Margin => self.margin,
        }
    }

    /// Sets a number, snapped to its steps and kept in its range.
    pub fn set(&mut self, n: Num, v: f32) {
        let (lo, hi, step) = n.range();
        let v = ((v.clamp(lo, hi) / step).round() * step).clamp(lo, hi);
        match n {
            Num::Size => self.size = v,
            Num::Outline => self.outline = v,
            Num::Shadow => self.shadow = v,
            Num::ShadowDistance => self.shadow_distance = v,
            Num::ShadowBlur => self.shadow_blur = v,
            Num::BackdropOpacity => self.backdrop_opacity = v,
            Num::BackdropRound => self.backdrop_round = v,
            Num::Margin => self.margin = v,
        }
    }

    pub fn paint(&self, p: Paint) -> u32 {
        match p {
            Paint::Text => self.colour,
            Paint::Outline => self.outline_colour,
            Paint::Shadow => self.shadow_colour,
            Paint::Backdrop => self.backdrop_colour,
        }
    }

    pub fn set_paint(&mut self, p: Paint, c: u32) {
        let c = c & 0xFF_FFFF;
        match p {
            Paint::Text => self.colour = c,
            Paint::Outline => self.outline_colour = c,
            Paint::Shadow => self.shadow_colour = c,
            Paint::Backdrop => self.backdrop_colour = c,
        }
    }

    /// Changes whenever anything that shows changes (the name doesn't): subtitle pictures are kept by it.
    pub fn key(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        let mut s = self.clone();
        s.name.clear();
        s.to_line().hash(&mut h);
        h.finish()
    }

    /// The settings line's value.
    pub fn to_line(&self) -> String {
        let clean = |s: &str| s.replace(['\t', '\n', '\r'], " ");
        let b = |v: bool| if v { "1" } else { "0" };
        [
            format!("name={}", clean(&self.name)),
            format!("font={}", clean(&self.font)),
            format!("weight={}", self.weight),
            format!("italic={}", b(self.italic)),
            format!("size={}", self.size),
            format!("colour={:06X}", self.colour),
            format!("file_colours={}", b(self.file_colours)),
            format!("outline={}", self.outline),
            format!("outline_colour={:06X}", self.outline_colour),
            format!("shadow={}", self.shadow),
            format!("shadow_colour={:06X}", self.shadow_colour),
            format!("shadow_distance={}", self.shadow_distance),
            format!("shadow_blur={}", self.shadow_blur),
            format!("backdrop={}", self.backdrop.key()),
            format!("backdrop_colour={:06X}", self.backdrop_colour),
            format!("backdrop_opacity={}", self.backdrop_opacity),
            format!("backdrop_round={}", self.backdrop_round),
            format!("margin={}", self.margin),
        ]
        .join("\t")
    }

    /// Reads a settings line's value: what's missing or unreadable keeps Looker's default.
    pub fn from_line(line: &str) -> Option<SubStyle> {
        let mut s = SubStyle::looker();
        let mut named = false;
        for part in line.split('\t') {
            let Some((k, v)) = part.split_once('=') else { continue };
            let v = v.trim();
            let num = |d: f32| v.parse::<f32>().ok().filter(|x| x.is_finite()).unwrap_or(d);
            let col = |d: u32| u32::from_str_radix(v.trim_start_matches('#'), 16).ok().filter(|c| *c <= 0xFF_FFFF).unwrap_or(d);
            match k.trim() {
                "name" if !v.is_empty() => {
                    s.name = v.to_string();
                    named = true;
                }
                "font" if !v.is_empty() => s.font = v.to_string(),
                "weight" => s.weight = WEIGHTS.iter().map(|w| w.0).find(|w| v.parse() == Ok(*w)).unwrap_or(s.weight),
                "italic" => s.italic = v == "1",
                "size" => s.size = num(s.size),
                "colour" => s.colour = col(s.colour),
                "file_colours" => s.file_colours = v == "1",
                "outline" => s.outline = num(s.outline),
                "outline_colour" => s.outline_colour = col(s.outline_colour),
                "shadow" => s.shadow = num(s.shadow),
                "shadow_colour" => s.shadow_colour = col(s.shadow_colour),
                "shadow_distance" => s.shadow_distance = num(s.shadow_distance),
                "shadow_blur" => s.shadow_blur = num(s.shadow_blur),
                "backdrop" => s.backdrop = Backdrop::ALL.into_iter().find(|b| b.key() == v).unwrap_or(s.backdrop),
                "backdrop_colour" => s.backdrop_colour = col(s.backdrop_colour),
                "backdrop_opacity" => s.backdrop_opacity = num(s.backdrop_opacity),
                "backdrop_round" => s.backdrop_round = num(s.backdrop_round),
                "margin" => s.margin = num(s.margin),
                _ => {}
            }
        }
        for n in Num::ALL {
            s.set(n, s.get(n));
        }
        named.then_some(s)
    }

    /// Looker's own look, as it always was: white Inter Medium on a dark outline with a soft shadow.
    pub fn looker() -> SubStyle {
        SubStyle {
            name: "Looker".into(),
            font: INTER.into(),
            weight: 500,
            italic: false,
            size: 80.0,
            colour: 0xFFFFFF,
            file_colours: true,
            outline: 7.5,
            outline_colour: 0x000000,
            shadow: 55.0,
            shadow_colour: 0x000000,
            shadow_distance: 7.0,
            shadow_blur: 9.0,
            backdrop: Backdrop::None,
            backdrop_colour: 0x000000,
            backdrop_opacity: 75.0,
            backdrop_round: 15.0,
            margin: 5.0,
        }
    }

    /// A new style's start: Looker's, named.
    pub fn named(name: &str) -> SubStyle {
        SubStyle { name: name.into(), ..SubStyle::looker() }
    }
}

/// The styles a first run starts with.
pub fn presets() -> Vec<SubStyle> {
    let base = SubStyle::looker();
    vec![
        base.clone(),
        SubStyle { name: "TV captions".into(), font: "Segoe UI".into(), weight: 600, outline: 0.0, shadow: 0.0, backdrop: Backdrop::Lines, backdrop_opacity: 75.0, backdrop_round: 15.0, ..base.clone() },
        SubStyle { name: "Yellow".into(), weight: 600, colour: 0xFFE14D, outline: 9.0, shadow: 60.0, ..base.clone() },
        SubStyle { name: "Soft shadow".into(), weight: 600, outline: 0.0, shadow: 90.0, shadow_distance: 3.0, shadow_blur: 14.0, ..base.clone() },
        SubStyle { name: "Large print".into(), weight: 700, size: 140.0, outline: 0.0, shadow: 0.0, backdrop: Backdrop::Block, backdrop_opacity: 85.0, backdrop_round: 20.0, ..base },
    ]
}

/// Settings from before the presets were 80 %: the presets (by name) still at 100 % go to their new size.
pub fn migrate_sizes(styles: &mut [SubStyle]) {
    let presets = presets();
    for s in styles.iter_mut().filter(|s| s.size == 100.0) {
        if let Some(p) = presets.iter().find(|p| p.name == s.name) {
            s.size = p.size;
        }
    }
}

/// `base`, or "base 2", "base 3"... whichever no style has yet.
pub fn unique_name(styles: &[SubStyle], base: &str) -> String {
    let taken = |n: &str| styles.iter().any(|s| s.name.eq_ignore_ascii_case(n));
    if !taken(base) {
        return base.to_string();
    }
    (2..).map(|i| format!("{base} {i}")).find(|n| !taken(n)).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_round_trip() {
        for s in presets() {
            assert_eq!(SubStyle::from_line(&s.to_line()), Some(s));
        }
        let odd = SubStyle { name: "Tab\there".into(), ..SubStyle::looker() };
        assert_eq!(SubStyle::from_line(&odd.to_line()).unwrap().name, "Tab here");
        // Unnamed: not a style; junk values keep the defaults and numbers stay in range.
        assert_eq!(SubStyle::from_line("font=Arial"), None);
        let s = SubStyle::from_line("name=X\tsize=9999\tcolour=zz\tweight=123\toutline=0.26").unwrap();
        assert_eq!((s.size, s.colour, s.weight, s.outline), (400.0, 0xFFFFFF, 500, 0.5));
    }

    #[test]
    fn old_preset_sizes_move_to_80() {
        let mut v = presets();
        for s in v.iter_mut() {
            s.size = 100.0;
        }
        v[4].size = 140.0;
        v.push(SubStyle { size: 100.0, ..SubStyle::named("Mine") });
        migrate_sizes(&mut v);
        let sizes: Vec<f32> = v.iter().map(|s| s.size).collect();
        assert_eq!(sizes, vec![80.0, 80.0, 80.0, 80.0, 140.0, 100.0]);
        // Old settings: styles without the version marker are migrated; saved ones aren't again.
        let old = "subtitle_style=name=Looker	size=100
";
        assert_eq!(crate::settings::parse(old).subtitle_styles[0].size, 80.0);
        let new = format!("subtitle_styles_v=2
{old}");
        assert_eq!(crate::settings::parse(&new).subtitle_styles[0].size, 100.0);
        assert_eq!(crate::settings::parse("subtitle_size=125").subtitle_styles[0].size, 125.0);
        assert_eq!(crate::settings::parse("subtitle_size=100").subtitle_styles[0].size, 80.0);
    }

    #[test]
    fn names_and_keys() {
        let p = presets();
        assert_eq!(unique_name(&p, "Looker"), "Looker 2");
        assert_eq!(unique_name(&p, "Mine"), "Mine");
        let mut a = p[0].clone();
        let k = a.key();
        a.name = "Renamed".into();
        assert_eq!(a.key(), k, "the name doesn't change the picture");
        a.set(Num::Outline, 3.0);
        assert_ne!(a.key(), k);
    }
}
