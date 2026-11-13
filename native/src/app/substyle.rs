//! The Subtitle styles card: the Settings card's frame (the same blur, fade, scroll and modality; `page.rs`'s
//! `Mode::Styles`), its sidebar the styles (the one picked is the one in use, as a theme chip is) with New style
//! under them, its pane the picked style: a preview of a video frame with two lines of subtitle on it (the video
//! playing, or a night or a day scene), then the style's settings, applied the moment they change.
//! Opened from the Subtitles menu's "Customise styles…" and Settings > Video and audio.

use std::sync::OnceLock;

use super::page::{Mode, Rows, Setting, right_part};
use super::*;
use crate::motion;
use crate::subs::Run;
use crate::substyle::{self, Backdrop, Num, Paint, SubStyle};

/// The card's buttons (`Hit::StyleAct`).
pub(super) mod act {
    pub const NEW: u8 = 0;
    pub const RENAME: u8 = 1;
    pub const DUPLICATE: u8 = 2;
    pub const DELETE: u8 = 3;
}

/// The preview's backgrounds (`Hit::StyleScene`).
const SCENE_VIDEO: u8 = 0;
const SCENE_NIGHT: u8 = 1;
const SCENE_DAY: u8 = 2;

const SIDEBAR_W: f32 = 210.0;
const PANE_LEFT: f32 = 28.0;
const PANE_RIGHT: f32 = 24.0;
const STEPPER_W: f32 = 120.0;
const MAX_STYLES: usize = 100;

static FONTS: OnceLock<Vec<String>> = OnceLock::new();

/// The fonts the Font drop-down offers: `substyle::FONTS` that are installed (Inter always), and the style's own
/// if it's another.
pub(super) fn fonts(current: &str) -> Vec<String> {
    let mut v = FONTS.get().cloned().unwrap_or_else(|| vec![substyle::INTER.to_string()]);
    if !v.iter().any(|f| f.eq_ignore_ascii_case(current)) {
        v.push(current.to_string());
    }
    v
}

/// The preview's subtitle: two lines, a word in italics as subtitle files have them.
fn sample() -> Vec<Vec<Run>> {
    let run = |t: &str, italic: bool| Run { text: t.into(), italic, bold: false, underline: false, strike: false, color: None };
    vec![vec![run("Where do you think you're going?", false)], vec![run("Somewhere I've ", false), run("never", true), run(" been.", false)]]
}

/// The preview's picture of the subtitle: what it was made for (style, size, width, DPI) and the bitmap with its
/// size and margin.
pub(super) struct Preview {
    key: (u64, u32, u32, u32),
    bitmap: ID2D1Bitmap1,
    w: f32,
    h: f32,
    pad: f32,
}

impl App {
    /// "Customise styles…": the card, or the Settings card turned into it.
    pub(super) fn open_sub_styles(&mut self) {
        self.load_sub_fonts();
        self.stop_slideshow();
        self.close_menu();
        self.hide_tooltip();
        self.pressed = None;
        match &mut self.page {
            Some(p) => {
                p.open = true;
                p.shown.set([1.0], std::time::Instant::now());
                if p.mode != Mode::Styles {
                    p.mode = Mode::Styles;
                    p.switched = std::time::Instant::now();
                    p.target = 0.0;
                    p.pos = 0.0;
                    p.vel = 0.0;
                }
            }
            None => self.page = Some(page::Page::styles()),
        }
        self.invalidate();
    }

    fn load_sub_fonts(&self) {
        let Some(g) = &self.gfx else { return };
        FONTS.get_or_init(|| unsafe {
            let mut col: Option<windows::Win32::Graphics::DirectWrite::IDWriteFontCollection1> = None;
            let mut v = vec![substyle::INTER.to_string()];
            if g.text.dwrite.GetSystemFontCollection(false, &mut col, false).is_ok() {
                if let Some(col) = col {
                    v.extend(
                        substyle::FONTS[1..]
                            .iter()
                            .filter(|f| {
                                let (mut idx, mut exists) = (0u32, windows::core::BOOL(0));
                                col.FindFamilyName(&HSTRING::from(**f), &mut idx, &mut exists).is_ok() && exists.as_bool()
                            })
                            .map(|f| f.to_string()),
                    );
                }
            }
            v
        });
    }

    /// The styles card is up (taking input or not).
    pub(super) fn styles_shown(&self) -> bool {
        self.page.as_ref().is_some_and(|p| p.mode == Mode::Styles)
    }

    /// Anything shown changed: saved, and the subtitles drawn again.
    fn style_changed(&mut self) {
        settings::save(&self.settings);
        self.invalidate();
    }

    pub(super) fn style_row(&mut self, i: usize) {
        if i < self.settings.subtitle_styles.len() && i != self.settings.subtitle_style {
            self.settings.subtitle_style = i;
            if let Some(p) = &mut self.page {
                p.switched = std::time::Instant::now();
            }
            self.style_changed();
        }
    }

    pub(super) fn style_act(&mut self, a: u8) {
        let s = &mut self.settings;
        match a {
            act::NEW if s.subtitle_styles.len() < MAX_STYLES => {
                let name = substyle::unique_name(&s.subtitle_styles, "New style");
                s.subtitle_styles.push(SubStyle::named(&name));
                s.subtitle_style = s.subtitle_styles.len() - 1;
                self.style_changed();
                self.rename_style();
            }
            act::DUPLICATE if s.subtitle_styles.len() < MAX_STYLES => {
                let mut copy = s.sub_style().clone();
                copy.name = substyle::unique_name(&s.subtitle_styles, &format!("{} copy", copy.name));
                let at = s.subtitle_style + 1;
                s.subtitle_styles.insert(at, copy);
                s.subtitle_style = at;
                self.style_changed();
            }
            act::RENAME => self.rename_style(),
            act::DELETE if s.subtitle_styles.len() > 1 => {
                let (name, at) = (s.sub_style().name.clone(), s.subtitle_style);
                let buttons = vec![ui::DialogButton::new("Delete", actions::Choice::Confirm), ui::DialogButton::new("Cancel", actions::Choice::Cancel)];
                let body = "The subtitles go back to the style above it in the list.".to_string();
                self.open_dialog_ui(actions::DialogKind::DeleteStyle(at), ui::Dialog::new(format!("Delete \u{201C}{name}\u{201D}?"), body, buttons, 1, false));
            }
            _ => {}
        }
        if let Some(p) = &mut self.page {
            p.switched = std::time::Instant::now();
        }
    }

    fn rename_style(&mut self) {
        let i = self.settings.subtitle_style;
        let mut edit = crate::textedit::TextEdit::new(&self.settings.sub_style().name);
        edit.select_all();
        let buttons = vec![ui::DialogButton::new("Rename", actions::Choice::Confirm), ui::DialogButton::new("Cancel", actions::Choice::Cancel)];
        let d = ui::Dialog::new("Rename style".into(), String::new(), buttons, 0, true).with_field(ui::TextField::new(edit));
        self.open_dialog_ui(actions::DialogKind::RenameStyle(i), d);
    }

    /// The rename dialog's name, for style `i`.
    pub(super) fn style_renamed(&mut self, i: usize, name: &str) {
        let name = name.trim().replace(['\t', '\n', '\r'], " ");
        if name.is_empty() || i >= self.settings.subtitle_styles.len() {
            return;
        }
        let others: Vec<SubStyle> = self.settings.subtitle_styles.iter().enumerate().filter(|(k, _)| *k != i).map(|(_, s)| s.clone()).collect();
        self.settings.subtitle_styles[i].name = substyle::unique_name(&others, &name);
        self.style_changed();
    }

    pub(super) fn style_deleted(&mut self, i: usize) {
        let s = &mut self.settings;
        if s.subtitle_styles.len() > 1 && i < s.subtitle_styles.len() {
            s.subtitle_styles.remove(i);
            s.subtitle_style = i.saturating_sub(1).min(s.subtitle_styles.len() - 1);
            self.style_changed();
        }
    }

    /// A stepper's − (0) or + (1) for `Num::ALL[n]`.
    pub(super) fn style_step(&mut self, n: u8, k: u8) {
        let Some(num) = Num::ALL.get(n as usize).copied() else { return };
        let step = num.range().2;
        let st = self.settings.sub_style_mut();
        st.set(num, st.get(num) + if k == 0 { -step } else { step });
        self.style_changed();
    }

    /// A press on a stepper's number: it drags from here.
    pub(super) fn style_drag_start(&mut self, n: u8, x: f32) {
        let Some(num) = Num::ALL.get(n as usize).copied() else { return };
        let v = self.settings.sub_style().get(num);
        if let Some(p) = &mut self.page {
            p.style_drag = Some((num, x, v));
        }
    }

    /// The number dragged to `x`: a step for every 6 DIPs from where the press began (the text size's way).
    pub(super) fn style_drag_to(&mut self, x: f32) {
        let Some((num, x0, v0)) = self.page.as_ref().and_then(|p| p.style_drag) else { return };
        let step = num.range().2;
        let st = self.settings.sub_style_mut();
        let before = st.get(num);
        st.set(num, v0 + ((x - x0) / 6.0).round() * step);
        if st.get(num) != before {
            self.invalidate();
        }
    }

    /// Released: kept.
    pub(super) fn style_drag_end(&mut self) {
        if self.page.as_mut().and_then(|p| p.style_drag.take()).is_some() {
            settings::save(&self.settings);
        }
    }

    pub(super) fn style_scene(&mut self, i: u8) {
        if let Some(p) = &mut self.page {
            p.scene = Some(i);
            self.invalidate();
        }
    }

    /// A drop-down's choices on the card (`page::choices` asks): its labels and which is picked.
    pub(super) fn style_choices(s: &Settings, which: Setting) -> (Vec<String>, usize) {
        let st = s.sub_style();
        match which {
            Setting::SubFont => {
                let v = fonts(&st.font);
                let i = v.iter().position(|f| f.eq_ignore_ascii_case(&st.font)).unwrap_or(0);
                (v, i)
            }
            Setting::SubWeight => (substyle::WEIGHTS.iter().map(|w| w.1.to_string()).collect(), substyle::WEIGHTS.iter().position(|w| w.0 == st.weight).unwrap_or(1)),
            Setting::SubBackdrop => (Backdrop::ALL.iter().map(|b| b.label().to_string()).collect(), Backdrop::ALL.iter().position(|b| *b == st.backdrop).unwrap_or(0)),
            _ => (Vec::new(), 0),
        }
    }

    /// A drop-down choice on the card.
    pub(super) fn style_choose(&mut self, which: Setting, i: usize) {
        let st = self.settings.sub_style_mut();
        match which {
            Setting::SubFont => {
                if let Some(f) = fonts(&st.font).get(i) {
                    st.font = f.clone();
                }
            }
            Setting::SubWeight => st.weight = substyle::WEIGHTS[i.min(substyle::WEIGHTS.len() - 1)].0,
            Setting::SubBackdrop => st.backdrop = Backdrop::ALL[i.min(Backdrop::ALL.len() - 1)],
            _ => return,
        }
        self.style_changed();
    }

    pub(super) fn style_toggle(&mut self, which: Setting) {
        let st = self.settings.sub_style_mut();
        match which {
            Setting::SubItalic => st.italic = !st.italic,
            Setting::SubFileColours => st.file_colours = !st.file_colours,
            _ => return,
        }
        self.style_changed();
    }

    /// Tooltips for the card's buttons.
    pub(super) fn style_tooltip(h: Hit) -> Option<&'static str> {
        Some(match h {
            Hit::StyleAct(act::RENAME) => "Rename",
            Hit::StyleAct(act::DUPLICATE) => "Duplicate",
            Hit::StyleAct(act::DELETE) => "Delete",
            Hit::StyleAct(act::NEW) => "A new style, starting from Looker's",
            Hit::StyleColour(i) => Paint::ALL.get(i as usize)?.title(),
            Hit::StyleStep(_, 0) => "Less",
            Hit::StyleStep(_, 1) => "More",
            Hit::StyleStep(_, _) => "Drag left or right",
            _ => return None,
        })
    }

    /// Before the frame begins: the preview's subtitle picture, for where the last frame put the preview.
    pub(super) fn prepare_style_preview(&mut self, g: &Gfx) {
        let Some(p) = self.page.as_ref().filter(|p| p.mode == Mode::Styles) else { return };
        let Some(r) = p.preview_rect else { return };
        let style = self.settings.sub_style().clone();
        let (w, h) = (r.right - r.left, r.bottom - r.top);
        let size = super::subtitles::text_size(w, h, style.size.round() as u32);
        let max_w = (w * 0.9).max(40.0);
        let key = (style.key(), (size * 4.0).round() as u32, max_w.round() as u32, g.dpi.round() as u32);
        if p.preview.as_ref().is_some_and(|pv| pv.key == key) {
            return;
        }
        let made = super::subtitles::text_sprite(g, &sample(), 2, size, max_w, &style);
        if let Some(p) = &mut self.page {
            p.preview = made.map(|(bitmap, w, h, pad)| Preview { key, bitmap, w, h, pad });
        }
    }

    // --- Drawing ------------------------------------------------------------------------------------

    pub(super) fn draw_styles_sidebar(&mut self, g: &Gfx, side: D2D_RECT_F, open: bool) {
        let t = theme::raised();
        g.fill(rect(side.right, side.top + 1.0, g.px(), side.bottom - side.top - 2.0), rgb(t.divider));
        g.text(&wide("Subtitle styles"), &g.fonts.page_title, rect(side.left + 22.0, side.top + 18.0, SIDEBAR_W - 34.0, 28.0), rgb(t.text), Align::Left);
        let item_h = crate::gfx::tsr(34.0);
        let mut y = side.top + 64.0;
        let current = self.settings.subtitle_style;
        let n = self.settings.subtitle_styles.len();
        g.push_clip(rect(side.left, y, SIDEBAR_W, side.bottom - y - 8.0));
        for i in 0..=n {
            let r = rect(side.left + 12.0, y, SIDEBAR_W - 24.0, item_h);
            let new = i == n;
            let id = if new { Hit::StyleAct(act::NEW) } else { Hit::StyleRow(i as u8) };
            let enabled = open && (!new || n < MAX_STYLES);
            if enabled {
                self.hits.add(id, r);
            }
            if !new {
                self.a11y_mark(id, super::a11y::Mark::Selected(i == current));
            }
            let on = if new { 0.0 } else { self.fades.get(Hit::StyleRowOn(i as u8), i == current) };
            let hov = self.state(id, enabled).hover;
            if hov > 0.0 && on < 1.0 {
                g.fill_round(r, 5.0, gfx::rgba(t.button, hov * (1.0 - on)));
            }
            if on > 0.0 {
                g.fill_round(r, 5.0, gfx::rgba(t.selected, on));
            }
            let fg = theme::mix(t.label, t.text, on.max(hov));
            if new {
                g.text(&[0xE710], &g.fonts.small_icons, rect(r.left + 6.0, r.top, 24.0, item_h), fg, Align::Center);
                g.text(&wide("New style"), &g.fonts.body, rect(r.left + 38.0, r.top, r.right - r.left - 42.0, item_h), fg, Align::Left);
            } else {
                // A swatch of the style: its text colour on its box (or a dark square).
                let st = &self.settings.subtitle_styles[i];
                let sw = rect(r.left + 9.0, r.top + (item_h - 18.0) / 2.0, 18.0, 18.0);
                let bg = if st.backdrop != Backdrop::None { st.backdrop_colour } else { 0x2A2A2A };
                g.fill_round(sw, 4.0, rgb(bg));
                ui::stroke(g, sw, 4.0, gfx::rgba(t.ink, 0.2));
                g.text(&wide("A"), &g.fonts.body, sw, rgb(st.colour), Align::Center);
                g.text(&wide(&st.name), &g.fonts.body, rect(r.left + 38.0, r.top, r.right - r.left - 42.0, item_h), fg, Align::Left);
            }
            y += item_h;
        }
        g.pop_clip();
    }

    /// The picked style: its name and buttons, the preview, its settings. True while something moves.
    pub(super) fn draw_styles_content(&mut self, g: &Gfx, pane: D2D_RECT_F, open: bool) -> bool {
        let Some(p) = &self.page else { return false };
        let t = theme::raised();
        let e = motion::progress(p.switched);
        let off = (p.pos / g.px()).round() * g.px();
        let (picked_scene, last_rect) = (p.scene, p.preview_rect);
        let pv = p.preview.as_ref().map(|pv| (pv.bitmap.clone(), pv.w, pv.h, pv.pad));
        let x = pane.left + PANE_LEFT;
        let w = pane.right - pane.left - PANE_LEFT - PANE_RIGHT;
        let style = self.settings.sub_style().clone();
        let count = self.settings.subtitle_styles.len();

        // The head: the name, Rename, Duplicate, Delete, and Close.
        let head = rect(x, pane.top + 18.0, w, 28.0);
        let mut bx = head.right;
        let buttons = [(Hit::PageClose, 0xE8BB, true), (Hit::StyleAct(act::DELETE), 0xE74D, count > 1), (Hit::StyleAct(act::DUPLICATE), 0xE8C8, count < MAX_STYLES), (Hit::StyleAct(act::RENAME), 0xE8AC, true)];
        for (k, (id, glyph, enabled)) in buttons.into_iter().enumerate() {
            bx -= 30.0;
            if k == 1 {
                bx -= 10.0; // a gap between the style's buttons and Close
            }
            let b = rect(bx, head.top, 30.0, 28.0);
            if open && enabled {
                self.hits.add(id, b);
            }
            let st = self.state(id, open && enabled);
            let fg = ui::button_frame(g, b, ui::Kind::Subtle, &st);
            let fg = if enabled { fg } else { rgb(t.placeholder) };
            g.text(&[glyph], &g.fonts.small_icons, b, fg, Align::Center);
        }
        g.text(&wide(&style.name), &g.fonts.page_title, rect(head.left, head.top, (bx - head.left - 8.0).max(0.0), head.bottom - head.top), rgb(t.text), Align::Left);

        // The preview: 16:9 across the pane, no taller than 42 % of the card.
        let mut pw = w;
        let mut ph = (pw * 9.0 / 16.0).min((pane.bottom - pane.top) * 0.42);
        pw = pw.min(ph * 16.0 / 9.0);
        ph = ph.min(pw * 9.0 / 16.0);
        let preview = rect(g.snap(x + (w - pw) / 2.0), g.snap(pane.top + 58.0), pw.round(), ph.round());
        let video = self.video_bitmap();
        let scene = match picked_scene {
            Some(SCENE_VIDEO) if video.is_none() => SCENE_NIGHT,
            Some(s) => s,
            None if video.is_some() => SCENE_VIDEO,
            None => SCENE_NIGHT,
        };
        let had_rect = last_rect == Some(preview);
        g.with_round_clip(preview, 8.0, || {
            match (scene, &video) {
                (SCENE_VIDEO, Some((bmp, sdr))) => {
                    g.fill(preview, rgb(0x000000));
                    // The frame fitted, as the viewer shows it.
                    let size = unsafe { bmp.GetSize() };
                    let (iw, ih) = (size.width.max(1.0), size.height.max(1.0));
                    let k = (pw / iw).min(ph / ih);
                    let (dw, dh) = (iw * k, ih * k);
                    g.draw_photo(bmp, rect(preview.left + (pw - dw) / 2.0, preview.top + (ph - dh) / 2.0, dw, dh), 1.0, *sdr);
                }
                (SCENE_DAY, _) => draw_day(g, preview),
                _ => draw_night(g, preview),
            }
            if let Some((bmp, sw, sh, pad)) = &pv {
                let text_h = sh - 2.0 * pad;
                let lx = (preview.left + preview.right) / 2.0 - (pw * 0.9).max(40.0) / 2.0;
                let ly = preview.bottom - ph * style.margin / 100.0 - text_h;
                g.draw_bitmap(bmp, rect(g.snap(lx - pad), g.snap(ly - pad), *sw, *sh), 1.0);
            }
        });
        ui::stroke(g, preview, 8.0, rgb(t.divider));
        // The first frame only learns where the preview goes: its picture comes on the next.
        let mut moving = !had_rect;
        if let Some(p) = &mut self.page {
            p.preview_rect = Some(preview);
        }

        // The scenes under it.
        let mut sx = preview.left;
        let sy = preview.bottom + 10.0;
        let scenes: Vec<(u8, &str)> = [(SCENE_VIDEO, "This video"), (SCENE_NIGHT, "Night"), (SCENE_DAY, "Day")].into_iter().filter(|(s, _)| *s != SCENE_VIDEO || video.is_some()).collect();
        for (s, label) in scenes {
            let bw = (g.measure(&wide(label), &g.fonts.body) + 24.0).ceil();
            let b = rect(sx, sy, bw, 26.0);
            let id = Hit::StyleScene(s);
            if open {
                self.hits.add(id, b);
            }
            self.a11y_mark(id, super::a11y::Mark::Selected(s == scene));
            let st = self.state(id, open);
            let fg = ui::button_frame(g, b, ui::Kind::Toggle(s == scene), &st);
            g.text(&wide(label), &g.fonts.body, b, fg, Align::Center);
            sx += bw + 6.0;
        }

        // The settings, scrolling under the preview.
        let view = D2D_RECT_F { left: pane.left, top: sy + 26.0 + 10.0, right: pane.right, bottom: pane.bottom - 12.0 };
        g.push_clip(view);
        g.push_fade(e, 1.0, 0.0, 0.0);
        let mut rows = Rows { x, w, y: view.top - off + (1.0 - e) * 10.0 };
        let hits_at = self.hits.len();
        self.style_rows(g, &mut rows, &style, open);
        self.hits.clip_since(hits_at, view);
        g.pop_fade();
        g.pop_clip();
        let used = rows.y + off - view.top + 16.0 - (1.0 - e) * 10.0;
        let Some(p) = &mut self.page else { return moving };
        p.content_h = used;
        p.view_h = view.bottom - view.top;
        if used > p.view_h + 0.5 {
            let max = used - p.view_h;
            let strip = rect(view.right - 10.0, view.top, 10.0, p.view_h);
            let thumb_h = (p.view_h * p.view_h / used).max(28.0);
            let ty = view.top + p.pos.clamp(0.0, max) / max * (p.view_h - thumb_h);
            let held = p.thumb_drag.is_some();
            if open {
                self.hits.add(Hit::PageThumb, strip);
            }
            let lit = self.fades.hover(Hit::PageThumb, held || (open && self.hover == Some(Hit::PageThumb)));
            ui::scroll_thumb(g, rect(view.right - 2.0 - ui::SCROLL_W, ty, ui::SCROLL_W, thumb_h), lit);
        }
        moving |= e < 1.0;
        moving
    }

    fn style_rows(&mut self, g: &Gfx, rows: &mut Rows, st: &SubStyle, open: bool) {
        rows.heading(g, "Text");
        let r = rows.row(g, "Font", "");
        self.combo(g, right_part(r, 200.0, 30.0), Setting::SubFont, open);
        let r = rows.row(g, "Weight", "");
        self.combo(g, right_part(r, 200.0, 30.0), Setting::SubWeight, open);
        let r = rows.switch_row(g, "Italic", "");
        self.switch(g, r, Setting::SubItalic, st.italic, open);
        let r = rows.row_room(g, "Size", "Follows the video's height. The Subtitles menu changes it too", STEPPER_W + 24.0, 0.0);
        self.style_stepper(g, r, Num::Size, st, open);
        let r = rows.row_room(g, "Colour", "", 144.0, 0.0);
        self.style_swatch(g, r, Paint::Text, st, open);
        let r = rows.switch_row(g, "Keep the subtitle file's colours", "Words a subtitle file colours itself keep their colour");
        self.switch(g, r, Setting::SubFileColours, st.file_colours, open);

        rows.heading(g, "Outline");
        let r = rows.row_room(g, "Width", "A share of the text size; 0 for none", STEPPER_W + 24.0, 0.0);
        self.style_stepper(g, r, Num::Outline, st, open);
        if st.outline > 0.0 {
            let r = rows.row_room(g, "Colour", "", 144.0, 0.0);
            self.style_swatch(g, r, Paint::Outline, st, open);
        }

        rows.heading(g, "Shadow");
        let r = rows.row_room(g, "Strength", "0 for none", STEPPER_W + 24.0, 0.0);
        self.style_stepper(g, r, Num::Shadow, st, open);
        if st.shadow > 0.0 {
            let r = rows.row_room(g, "Distance", "How far below the text it falls", STEPPER_W + 24.0, 0.0);
            self.style_stepper(g, r, Num::ShadowDistance, st, open);
            let r = rows.row_room(g, "Softness", "", STEPPER_W + 24.0, 0.0);
            self.style_stepper(g, r, Num::ShadowBlur, st, open);
            let r = rows.row_room(g, "Colour", "", 144.0, 0.0);
            self.style_swatch(g, r, Paint::Shadow, st, open);
        }

        rows.heading(g, "Background");
        let r = rows.row(g, "Box", "A box behind the text, as TV captions have");
        self.combo(g, right_part(r, 200.0, 30.0), Setting::SubBackdrop, open);
        if st.backdrop != Backdrop::None {
            let r = rows.row_room(g, "Colour", "", 144.0, 0.0);
            self.style_swatch(g, r, Paint::Backdrop, st, open);
            let r = rows.row_room(g, "Opacity", "", STEPPER_W + 24.0, 0.0);
            self.style_stepper(g, r, Num::BackdropOpacity, st, open);
            let r = rows.row_room(g, "Rounded corners", "", STEPPER_W + 24.0, 0.0);
            self.style_stepper(g, r, Num::BackdropRound, st, open);
        }

        rows.heading(g, "Position");
        let r = rows.row_room(g, "Distance from the bottom", "A share of the video's height. Subtitles still move up out of the way of the controls", STEPPER_W + 24.0, 0.0);
        self.style_stepper(g, r, Num::Margin, st, open);
        rows.note(g, "Styles change subtitles that are plain text (SubRip, WebVTT, MP4 and MKV text). ASS subtitles keep the look their file gives them, apart from the size, and subtitles that are pictures (Blu-ray, DVD) can't be restyled.");
    }

    /// A stepper at the right of row `r` for `num`: − value +, the value draggable.
    fn style_stepper(&mut self, g: &Gfx, r: D2D_RECT_F, num: Num, st: &SubStyle, open: bool) {
        let n = Num::ALL.iter().position(|x| *x == num).unwrap_or(0) as u8;
        let b = right_part(r, STEPPER_W, 30.0);
        let b = rect(g.snap(b.left), g.snap(b.top), STEPPER_W, 30.0);
        let (lo, hi, _) = num.range();
        let v = st.get(num);
        let can = [v > lo + 1e-3, v < hi - 1e-3];
        let (minus, plus) = ui::stepper_buttons(b);
        if open {
            self.hits.add(Hit::StyleStep(n, 2), D2D_RECT_F { left: minus.right, right: plus.left, ..b });
        }
        let mut hot = [0.0; 2];
        for (k, rr) in [(0usize, minus), (1, plus)] {
            let id = Hit::StyleStep(n, k as u8);
            if open && can[k] {
                self.hits.add(id, rr);
            }
            hot[k] = self.state(id, open && can[k]).hover;
        }
        ui::stepper(g, b, &num.text(v), can, hot);
    }

    /// A colour button at the right of row `r`: the colour and its hex code; it opens the colour picker.
    fn style_swatch(&mut self, g: &Gfx, r: D2D_RECT_F, paint: Paint, st: &SubStyle, open: bool) {
        let t = theme::raised();
        let i = Paint::ALL.iter().position(|x| *x == paint).unwrap_or(0) as u8;
        let b = right_part(r, 120.0, 30.0);
        let b = rect(g.snap(b.left), g.snap(b.top), 120.0, 30.0);
        let id = Hit::StyleColour(i);
        if open {
            self.hits.add(id, b);
        }
        let state = self.state(id, open);
        let fg = ui::button_frame(g, b, ui::Kind::Standard, &state);
        let c = st.paint(paint);
        let sw = rect(b.left + 9.0, b.top + 7.0, 16.0, 16.0);
        g.fill_round(sw, 4.0, rgb(c));
        ui::stroke(g, sw, 4.0, gfx::rgba(t.ink, 0.3));
        g.text_tabular(&wide(&format!("#{c:06X}")), &g.fonts.body, rect(sw.right + 8.0, b.top, b.right - sw.right - 14.0, 30.0), fg, Align::Left);
    }
}

/// A night: a deep blue sky with a moon and stars over dark hills (a dark scene, as most films are).
fn draw_night(g: &Gfx, r: D2D_RECT_F) {
    let (w, h) = (r.right - r.left, r.bottom - r.top);
    g.fill_vgradient(r, &[(0.0, rgb(0x0B1230)), (0.65, rgb(0x2B3A6B)), (1.0, rgb(0x5B4E7A))]);
    for (i, (fx, fy)) in [(0.08, 0.12), (0.2, 0.3), (0.33, 0.08), (0.47, 0.22), (0.58, 0.1), (0.66, 0.34), (0.9, 0.18), (0.14, 0.45), (0.4, 0.4), (0.84, 0.42)].into_iter().enumerate() {
        let s = if i % 3 == 0 { 2.2 } else { 1.4 };
        g.fill_round(rect(r.left + fx * w, r.top + fy * h, s, s), s / 2.0, gfx::rgba(0xFFFFFF, 0.8));
    }
    let m = h * 0.13;
    g.fill_round(rect(r.left + w * 0.76, r.top + h * 0.12, m, m), m / 2.0, rgb(0xF2EEDB));
    hills(g, r, 0.62, 0.07, 1.3, rgb(0x1A2244));
    hills(g, r, 0.74, 0.06, 2.1, rgb(0x0C1124));
}

/// A day: a pale sky and green hills, a hard place for white text to read.
fn draw_day(g: &Gfx, r: D2D_RECT_F) {
    let (w, h) = (r.right - r.left, r.bottom - r.top);
    g.fill_vgradient(r, &[(0.0, rgb(0x7DB8F0)), (0.7, rgb(0xD8ECFA)), (1.0, rgb(0xF4F8FB))]);
    for (fx, fy, k) in [(0.12, 0.16, 1.0), (0.55, 0.1, 1.3), (0.8, 0.26, 0.8)] {
        let cw = w * 0.16 * k;
        let ch = h * 0.07 * k;
        let (cx, cy) = (r.left + fx * w, r.top + fy * h);
        g.fill_round(rect(cx, cy, cw, ch), ch / 2.0, gfx::rgba(0xFFFFFF, 0.92));
        g.fill_round(rect(cx + cw * 0.22, cy - ch * 0.45, cw * 0.45, ch * 1.1), ch * 0.55, gfx::rgba(0xFFFFFF, 0.92));
    }
    hills(g, r, 0.6, 0.08, 1.1, rgb(0x9CCB86));
    hills(g, r, 0.72, 0.07, 1.9, rgb(0x6FAF5E));
    g.fill(rect(r.left, r.top + h * 0.9, w, h * 0.1 + 1.0), rgb(0xE9DFC4));
}

/// A line of hills across `r`: their tops near `at` (a share of the height) rising and falling by `amp`, `waves`
/// times across.
fn hills(g: &Gfx, r: D2D_RECT_F, at: f32, amp: f32, waves: f32, c: windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F) {
    let (w, h) = (r.right - r.left, r.bottom - r.top);
    let mut pts = vec![(r.left, r.bottom + 1.0)];
    for i in 0..=48 {
        let f = i as f32 / 48.0;
        let y = at + amp * (f * waves * std::f32::consts::TAU + at * 9.0).sin() * (0.6 + 0.4 * (f * 7.3).cos());
        pts.push((r.left + f * w, r.top + y * h));
    }
    pts.push((r.right, r.bottom + 1.0));
    g.fill_polygon(&pts, c);
}
