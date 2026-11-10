//! One tab strip, drawn and dragged the same way wherever tabs are: the explorer's folders and the text editor's
//! documents (Faye, 2026-11-11: the editor's tabs "exactly how the file explorer tabs look and feel"). As
//! Transient's (File Pilot's measurements): tabs 155 wide on a 32-tall strip, narrowing together to fit; the one in
//! front in the card's colour, outlined, flowing into the card below (the outline in the accent while its card has
//! the keyboard); the others a faint wash on hover, thin lines between them; a close button on each; a + after
//! them. Tabs ease into their places as they open, close and move; a dragged tab follows the pointer and the
//! others ease aside around an outlined slot.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use super::*;
use crate::motion::Ease;
use crate::shell_icons;

pub(super) const STRIP_H: f32 = 32.0;
const TAB_W: f32 = 155.0;
/// How far the pointer moves before a press on a tab becomes a drag of it.
const DRAG_START: f32 = 4.0;

/// A tab dragged along the strip: it follows the pointer, the others ease aside around an outlined slot.
pub(super) struct TabDrag {
    pub idx: usize,
    pub start_x: f32,
    pub x: f32,
    /// The pointer's offset from the tab's left edge when pressed.
    pub grab: f32,
    pub active: bool,
    /// The slot it would land in (worked out as the strip draws).
    pub insert: usize,
}

/// A strip's motion and drag, kept between frames.
pub(super) struct TabStrip {
    /// Each tab's place (by its id; u64::MAX is the +) and the tabs' width, easing.
    pub pos: HashMap<u64, Ease<1>>,
    pub w: Ease<1>,
    pub pitch: f32,
    /// Places and width ease until then (a tab opened, closed or dropped); otherwise they're set at once.
    pub anim_until: Instant,
    pub drag: Option<TabDrag>,
    /// Titles cut off, for tooltips.
    pub tips: Vec<(usize, String)>,
}

impl Default for TabStrip {
    fn default() -> TabStrip {
        TabStrip { pos: HashMap::new(), w: Ease::at([0.0]), pitch: 0.0, anim_until: Instant::now(), drag: None, tips: Vec::new() }
    }
}

impl TabStrip {
    /// Tabs opened, closed or reordered: they ease to their new places.
    pub(super) fn animate(&mut self) {
        self.anim_until = Instant::now() + Duration::from_millis(400);
    }
}

/// Which strip: its hits, and where its state lives.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum StripKind {
    Explorer,
    Text,
}

impl StripKind {
    fn tab(self, i: usize) -> Hit {
        match self {
            StripKind::Explorer => Hit::Tab(i as u8),
            StripKind::Text => Hit::TextTab(i as u16),
        }
    }
    fn close(self, i: usize) -> Hit {
        match self {
            StripKind::Explorer => Hit::TabClose(i as u8),
            StripKind::Text => Hit::TextTabClose(i as u16),
        }
    }
    fn new_tab(self) -> Hit {
        match self {
            StripKind::Explorer => Hit::TabNew,
            StripKind::Text => Hit::TextTabNew,
        }
    }
    /// The fade that lights the front tab's outline while its card has the keyboard.
    fn lit(self) -> Hit {
        match self {
            StripKind::Explorer => Hit::Tab(u8::MAX),
            StripKind::Text => Hit::TextTab(u16::MAX),
        }
    }
}

pub(super) enum TabIcon {
    /// Windows' icon, else a glyph in a tint.
    Shell(shell_icons::Kind, u16, u32),
    Glyph(u16, u32),
    Dot(u32),
}

pub(super) struct TabSpec {
    pub id: u64,
    pub title: String,
    pub icon: TabIcon,
    /// Unsaved edits: a dot where the close button is, until the pointer is on the tab.
    pub dirty: bool,
}

impl App {
    fn strip_mut(&mut self, k: StripKind) -> &mut TabStrip {
        match k {
            StripKind::Explorer => &mut self.explorer.strip,
            StripKind::Text => &mut self.text.strip,
        }
    }

    /// Draws a strip of `tabs` along the top of a card (`inner` is the card, `radius` its corners), the one at
    /// `front` in front. Returns the strip's bottom, and whether the tabs are still moving.
    pub(super) fn draw_tab_strip(&mut self, g: &Gfx, kind: StripKind, strip: D2D_RECT_F, inner: D2D_RECT_F, radius: f32, tabs: &[TabSpec], front: usize, focused: bool, closable: bool) -> (f32, bool) {
        let mut st = std::mem::take(self.strip_mut(kind));
        let out = self.draw_tab_strip_with(g, kind, &mut st, strip, inner, radius, tabs, front, focused, closable);
        *self.strip_mut(kind) = st;
        out
    }

    fn draw_tab_strip_with(&mut self, g: &Gfx, kind: StripKind, st: &mut TabStrip, strip: D2D_RECT_F, inner: D2D_RECT_F, radius: f32, tabs: &[TabSpec], front: usize, focused: bool, closable: bool) -> (f32, bool) {
        let t = theme::t();
        let strip = D2D_RECT_F { bottom: strip.top + STRIP_H, ..strip };
        let n = tabs.len();
        let now = Instant::now();
        let lit = self.fades.get(kind.lit(), focused);
        // The strip behind the tabs is the browse pane's colour, inactive tabs on it; the tab in front keeps the
        // card's, joining what's below (Faye, 2026-11-10). Inside the card's rounded corners.
        g.push_clip(strip);
        g.fill_round(inner, radius, rgb(t.pane));
        g.pop_clip();
        g.hline(strip.left, strip.bottom - g.px(), strip.right - strip.left, rgb(t.divider));
        if lit > 0.0 {
            g.hline(strip.left, strip.bottom - g.px(), strip.right - strip.left, gfx::rgba(t.accent, lit));
        }
        let plus_w = 34.0;
        st.tips.clear();
        let fit = ((strip.right - strip.left - 12.0 - plus_w) / (TAB_W * n.max(1) as f32)).min(1.0);
        let tw_target = (TAB_W * fit).max(40.0);
        let x0 = strip.left + 6.0;
        // The tabs' width eases with their places when one opens or closes (squeezed into a narrow strip, it
        // changes: it used to snap there, Faye 2026-11-10); a window resize sets it at once.
        let tab_anim = now < st.anim_until;
        if tab_anim {
            st.w.set([tw_target], now);
        } else {
            st.w = Ease::at([tw_target]);
        }
        let tw = st.w.value(now)[0];
        st.pitch = tw_target;
        // Draw order: the tabs in their slots (a dragged one leaves its slot to the ghost), then the dragged one.
        let drag = st.drag.as_ref().filter(|d| d.active && d.idx < n).map(|d| (d.idx, d.x - d.grab));
        let mut order: Vec<usize> = (0..n).collect();
        let mut ghost = None;
        let mut drag_x = 0.0;
        if let Some((di, left)) = drag {
            drag_x = left.clamp(x0, (strip.right - tw - 6.0 - plus_w).max(x0));
            let slot = (((drag_x - x0) / tw).round().max(0.0) as usize).min(n - 1);
            order.retain(|&k| k != di);
            order.insert(slot, di);
            ghost = Some(slot);
            if let Some(d) = st.drag.as_mut() {
                d.insert = slot;
            }
        }
        let animate = drag.is_some() || now < st.anim_until;
        let mut moving = drag.is_some() || st.w.moving(now);
        let mut xs: Vec<(usize, f32)> = Vec::new();
        for (pos, &k) in order.iter().enumerate() {
            let id = tabs[k].id;
            let target = (pos as f32) * tw_target;
            if Some(k) == drag.map(|d| d.0) {
                // It follows the pointer exactly; dropped, it eases into its slot from there.
                st.pos.insert(id, Ease::at([drag_x - x0]));
                continue;
            }
            let e = st.pos.entry(id).or_insert_with(|| Ease::at([target]));
            if animate {
                e.set([target], now);
            } else {
                *e = Ease::at([target]);
            }
            moving |= e.moving(now);
            xs.push((k, x0 + e.value(now)[0]));
        }
        let lane = D2D_RECT_F { right: strip.right - plus_w, ..strip };
        g.push_clip(lane);
        if let (Some(slot), Some(_)) = (ghost, drag) {
            let gb = rect(x0 + slot as f32 * tw + 3.5, strip.top + 4.0, tw - 7.0, STRIP_H - 4.0);
            g.fill_tab(gb, 7.0, 0.0, gfx::rgba(t.accent, 0.10));
            g.stroke_tab(D2D_RECT_F { bottom: gb.bottom - 1.0, ..gb }, 7.0, 0.0, rgb(t.accent));
        }
        if let Some((di, _)) = drag {
            xs.push((di, drag_x));
        }
        let pos_of = |k: usize| order.iter().position(|&o| o == k).unwrap_or(0);
        let dragging = st.drag.as_ref().is_some_and(|d| d.active);
        for &(k, x) in &xs {
            let tab = &tabs[k];
            let is_dragged = drag.is_some_and(|d| d.0 == k);
            let body = rect(x + 3.5, strip.top + 4.0, tw - 7.0, STRIP_H - 4.0);
            let on = k == front;
            let id = kind.tab(k);
            if !is_dragged {
                self.hits.add(id, body);
            }
            let cr = rect(body.right - 25.0, body.top + (body.bottom - body.top - 16.0) / 2.0, 16.0, 16.0);
            let close_id = kind.close(k);
            if closable {
                self.hits.add(close_id, cr);
            }
            let close_hot = self.hover == Some(close_id);
            let hot = !dragging && (self.hover == Some(id) || close_hot);
            if on {
                g.fill_tab(body, 7.0, 4.0, rgb(t.sidebar));
                g.stroke_tab(body, 7.0, 4.0, rgb(t.divider));
                if lit > 0.0 {
                    g.stroke_tab(body, 7.0, 4.0, gfx::rgba(t.accent, lit));
                }
            } else if hot || is_dragged {
                if is_dragged {
                    ui::shadow(g, body, 6.0);
                    g.fill_tab(body, 7.0, 0.0, rgb(theme::raised().popup));
                }
                g.fill_tab(D2D_RECT_F { bottom: body.bottom - 1.0, ..body }, 7.0, 0.0, gfx::rgba(t.ink, 0.05));
            }
            // A line between tabs that aren't in front (none beside the one in front, a hovered one or the slot).
            let p = pos_of(k);
            let next = order.get(p + 1).copied();
            if !is_dragged && !on && !hot && next != Some(front) && ghost != Some(p + 1) && ghost != Some(p) {
                g.fill(rect(x + tw, strip.top + 10.0, g.px(), STRIP_H - 11.0), rgb(t.divider));
            }
            // The icon, the title (cut short with an ellipsis before the close button), the close button.
            let ir = rect(body.left + 12.0, body.top, 16.0, body.bottom - body.top);
            match &tab.icon {
                TabIcon::Shell(k, glyph, tint) => {
                    if !super::explorer_view::shell_icon(g, k, ir, 1.0, None) {
                        g.text(&[*glyph], &g.fonts.body_icons, ir, rgb(*tint), Align::Center);
                    }
                }
                TabIcon::Glyph(glyph, tint) => g.text(&[*glyph], &g.fonts.small_icons, ir, rgb(*tint), Align::Center),
                TabIcon::Dot(c) => g.fill_round(rect(ir.left + 4.0, ir.top + (ir.bottom - ir.top - 8.0) / 2.0, 8.0, 8.0), 4.0, rgb(*c)),
            }
            let tx = body.left + 34.0;
            let text_end = if closable { cr.left - 4.0 } else { body.right - 10.0 };
            g.text(&wide(&tab.title), &g.fonts.body, rect(tx, body.top, (text_end - tx).max(0.0), body.bottom - body.top), rgb(t.text), Align::Left);
            // A title cut off: its full text for a tooltip.
            if self.hover == Some(id) && g.measure(&wide(&tab.title), &g.fonts.body) > (text_end - tx).max(0.0) + 0.5 {
                st.tips.push((k, tab.title.clone()));
            }
            if closable {
                if tab.dirty && !hot {
                    // Unsaved edits: a dot until the pointer is on the tab, then the close button.
                    g.fill_round(rect(cr.left + 4.5, cr.top + 4.5, 7.0, 7.0), 3.5, rgb(t.label));
                } else {
                    if close_hot {
                        g.fill_round(cr, 4.0, rgb(t.track));
                    }
                    g.text(&[0xE8BB], &g.fonts.caption_icons, cr, rgb(if close_hot { t.text } else { t.label }), Align::Center);
                }
            }
        }
        g.pop_clip();
        // The +, after the last tab (easing along as tabs come and go).
        let plus_target = n as f32 * tw_target;
        let e = st.pos.entry(u64::MAX).or_insert_with(|| Ease::at([plus_target]));
        if animate {
            e.set([plus_target], now);
        } else {
            *e = Ease::at([plus_target]);
        }
        moving |= e.moving(now);
        let px = (x0 + e.value(now)[0]).min(strip.right - plus_w);
        self.nav_button(g, rect(px + 4.0, strip.top + 5.0, 26.0, STRIP_H - 9.0), kind.new_tab(), 0xE710, true, false);
        (strip.bottom, moving)
    }

    /// A press on a tab: moving on drags it.
    pub(super) fn strip_press(&mut self, kind: StripKind, i: usize, x: f32) {
        let left = self.hits.rect_of(kind.tab(i)).map_or(x, |r| r.left);
        self.strip_mut(kind).drag = Some(TabDrag { idx: i, start_x: x, x, grab: x - left, active: false, insert: i });
    }

    pub(super) fn strip_drag_move(&mut self, kind: StripKind, x: f32) {
        let Some(d) = self.strip_mut(kind).drag.as_mut() else { return };
        d.x = x;
        if !d.active && (x - d.start_x).abs() > DRAG_START {
            d.active = true;
        }
        if d.active {
            self.invalidate();
        }
    }

    /// Let go: a dragged tab's move (from, to), or None when it wasn't a drag.
    pub(super) fn strip_release(&mut self, kind: StripKind) -> Option<(usize, usize)> {
        let st = self.strip_mut(kind);
        let d = st.drag.take()?;
        if !d.active {
            return None;
        }
        st.animate();
        self.invalidate();
        Some((d.idx, d.insert))
    }

    pub(super) fn strip_tip(&self, kind: StripKind, i: usize) -> Option<String> {
        let st = match kind {
            StripKind::Explorer => &self.explorer.strip,
            StripKind::Text => &self.text.strip,
        };
        st.tips.iter().find(|(k, _)| *k == i).map(|(_, t)| t.clone())
    }
}
