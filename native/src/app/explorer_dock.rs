//! The browse pane: a dock across the top of the explorer card, above the file tabs (Faye, 2026-11-13; it was a
//! column at the left that only came once the card was wide enough, which nobody would find). Its sections
//! (Bookmarks, Collections, Places, Network, History) are tabs in a header row; the open one's places flow down
//! columns beneath. Its bottom edge resizes it, giving on the panes' rubber band past either end and springing
//! back; the chevron folds it to its header. The rows are the sidebar's (`sidebar_items`, `Hit::SideRow` by the
//! same index), so clicks, menus, tooltips and drops onto folders work as they did.

use std::time::Instant;

use super::explorer::{SideItem, SideTarget, bounce, rubber};
use super::explorer_view::{place_icon, row_h, shell_icon};
use super::*;
use crate::explorer::{Place, SideSection};
use crate::motion::Ease;
use crate::shell_icons;
use windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F;

/// The header row with the section tabs.
pub(super) const DOCK_HEAD: f32 = 34.0;
/// The places area's height range (its height is the user's, `Settings::dock_height`); the most also leaves the
/// list `LIST_ROOM` under it.
pub(super) const DOCK_MIN: f32 = 84.0;
pub(super) use crate::settings::DOCK_HEIGHT;
const LIST_ROOM: f32 = 220.0;
/// A column of places is never narrower than this; the room left over widens them all.
const COL_MIN: f32 = 180.0;
const COL_GAP: f32 = 4.0;
const PAD: f32 = 6.0;
/// A section's tab with its icon alone.
const ICON_TAB: f32 = 32.0;

/// A section in the header: where its items are in `sidebar_items`, its name and glyph.
struct Section {
    sec: SideSection,
    first: usize,
    end: usize,
    label: String,
    glyph: u16,
}

impl App {
    /// The dock's places area at full height now: the user's height (never more than the card leaves), stretched or
    /// bouncing past its limits, easing back to its default after a double-click on its edge.
    fn dock_body_full(&self) -> f32 {
        let e = &self.explorer;
        let max = e.dock_max.max(DOCK_MIN);
        let stretch = if e.dock_stretch != 0.0 { e.dock_stretch } else { e.dock_spring.map_or(0.0, |(from, at)| bounce(from, at)) };
        let h = self.settings.dock_height.clamp(DOCK_MIN, max) + stretch;
        match e.dock_settle {
            Some(s) if s.moving() => s.value(h),
            _ => h,
        }
    }

    /// Still bouncing or settling (the frame after it lands too).
    fn dock_settling(&mut self) -> bool {
        let e = &mut self.explorer;
        let springing = e.dock_spring.is_some_and(|(_, at)| crate::motion::enabled() && at.elapsed().as_secs_f32() < super::explorer::SPRING_SECS);
        let sprung = !springing && e.dock_spring.take().is_some();
        let settling = e.dock_settle.is_some_and(|s| s.moving());
        let settled = !settling && e.dock_settle.take().is_some();
        springing || sprung || settling || settled
    }

    /// The dock's sections this frame, in the user's order (History only once there is some).
    fn dock_sections(items: &[SideItem]) -> Vec<Section> {
        let mut out: Vec<Section> = Vec::new();
        for (i, it) in items.iter().enumerate() {
            if let SideTarget::Header(sec) = it.target {
                if let Some(last) = out.last_mut() {
                    last.end = i;
                }
                out.push(Section { sec, first: i + 1, end: items.len(), label: it.label.clone(), glyph: it.glyph });
            }
        }
        out
    }

    /// The section showing: the one picked, else the first.
    fn dock_active(&self, secs: &[Section]) -> Option<usize> {
        secs.iter().position(|s| Some(s.sec) == self.settings.dock_tab).or(if secs.is_empty() { None } else { Some(0) })
    }

    /// Draws the dock across the top of `inner` (the card inside its border). Returns where it ends (the tab strip
    /// starts there), its edge's grip rect (the caller adds its hit last, over the strip's), and whether it moves.
    pub(super) fn draw_dock(&mut self, g: &Gfx, inner: D2D_RECT_F) -> (f32, Option<D2D_RECT_F>, bool) {
        let t = theme::t();
        let now = Instant::now();
        let w = inner.right - inner.left;
        let items = self.sidebar_items();
        let secs = Self::dock_sections(&items);
        let active = self.dock_active(&secs);
        let mut moving = self.dock_settling();

        // Folding and opening ease on the app's curve.
        let open = self.fades.get(Hit::SidebarColumn, self.settings.dock_open);
        moving |= open > 0.0 && open < 1.0;
        self.explorer.dock_max = (inner.bottom - inner.top - DOCK_HEAD - LIST_ROOM).max(DOCK_MIN);
        let body_full = self.dock_body_full().max(0.0);
        let body_h = g.snap(body_full * open);
        let bottom = inner.top + DOCK_HEAD + body_h;
        let dock = D2D_RECT_F { bottom, ..inner };

        // Its fill is the pane's, inside the card's top corners (the tab strip under it is the same colour).
        g.push_clip(dock);
        g.fill_round(inner, 9.0, rgb(t.pane));
        g.pop_clip();

        // --- The header: a tab per section, the chevron at the right.
        let toggle = rect(inner.right - PAD - 26.0, inner.top + 5.0, 26.0, 24.0);
        let lane = (inner.left + PAD, toggle.left - 6.0);
        let hover_tint = theme::mix(t.sidebar, t.accent, 0.2);
        // Each tab's width: its icon and name; when they don't all fit, the others are their icons alone (the open
        // one keeps its name while that fits), named in their tooltips.
        let full: Vec<f32> = secs.iter().map(|s| 16.0 + 6.0 + g.measure(&wide(&s.label), &g.fonts.body) + 22.0).collect();
        let room = lane.1 - lane.0 - 2.0 * secs.len() as f32;
        let named: Vec<bool> = if full.iter().sum::<f32>() <= room {
            vec![true; secs.len()]
        } else {
            let keep = active.filter(|&a| full[a] + ICON_TAB * (secs.len() - 1) as f32 <= room);
            (0..secs.len()).map(|k| Some(k) == keep).collect()
        };
        let widths: Vec<f32> = (0..secs.len()).map(|k| if named[k] { full[k] } else { ICON_TAB }).collect();
        // A tab dragged by the pointer leaves its place and the others close up around where it would land.
        let drag = self.explorer.section_drag.as_ref().filter(|d| d.active).map(|d| (d.sec, d.at - d.grab));
        let dragged = drag.and_then(|(sec, _)| secs.iter().position(|s| s.sec == sec));
        let mut order: Vec<usize> = (0..secs.len()).collect();
        if let (Some(di), Some((_, left))) = (dragged, drag) {
            order.retain(|&k| k != di);
            let mid = left + widths[di] / 2.0 - lane.0;
            let mut x = 0.0;
            let mut insert = 0;
            for &k in &order {
                if x + widths[k] / 2.0 < mid {
                    insert += 1;
                }
                x += widths[k] + 2.0;
            }
            order.insert(insert, di);
            if let Some(d) = self.explorer.section_drag.as_mut() {
                d.insert = insert;
            }
        }
        let animate = drag.is_some() || now < self.explorer.section_anim_until;
        let mut xs: Vec<f32> = vec![0.0; secs.len()];
        let mut x = 0.0;
        for &k in &order {
            let e = self.explorer.section_pos.entry(secs[k].sec).or_insert_with(|| Ease::at([x]));
            if Some(k) == dragged {
                let left = drag.map_or(0.0, |d| d.1).clamp(lane.0, (lane.1 - widths[k]).max(lane.0));
                *e = Ease::at([left - lane.0]);
            } else if animate {
                e.set([x], now);
            } else {
                *e = Ease::at([x]);
            }
            moving |= e.moving(now);
            xs[k] = lane.0 + e.value(now)[0];
            x += widths[k] + 2.0;
        }
        moving |= drag.is_some();
        g.push_clip(D2D_RECT_F { left: lane.0 - 2.0, right: lane.1 + 2.0, top: inner.top, bottom: inner.top + DOCK_HEAD });
        // The open tab's box glides from one tab to the next.
        if let Some(a) = active {
            let target = [xs[a], widths[a]];
            let e = self.explorer.dock_pill.get_or_insert_with(|| Ease::at(target));
            if Some(a) == dragged {
                *e = Ease::at(target);
            } else {
                e.set(target, now);
            }
            moving |= e.moving(now);
            let [px, pw] = e.value(now);
            let pr = rect(px, inner.top + 5.0, pw, 24.0);
            g.fill_round(pr, 5.0, rgb(t.selected));
            ui::stroke(g, pr, 5.0, rgb(t.accent));
        }
        for &k in order.iter().filter(|&&k| Some(k) != dragged).chain(dragged.iter()) {
            let s = &secs[k];
            let r = rect(xs[k], inner.top + 5.0, widths[k], 24.0);
            let id = Hit::SideHeader(s.sec);
            self.hits.add(id, r);
            let on = Some(k) == active;
            let hovered = self.hover == Some(id) && drag.is_none();
            let hover = self.fades.hover(id, hovered && !on);
            if Some(k) == dragged {
                let raised = theme::raised();
                ui::shadow(g, r, 5.0);
                g.fill_round(r, 5.0, rgb(if on { t.selected } else { raised.popup }));
                ui::stroke(g, r, 5.0, rgb(if on { t.accent } else { raised.divider }));
            } else if hover > 0.0 {
                g.fill_round(r, 5.0, D2D1_COLOR_F { a: hover, ..hover_tint });
                ui::stroke(g, r, 5.0, gfx::rgba(t.accent, hover));
            }
            let c = if on || hovered { t.text } else { t.label };
            if !named[k] {
                g.text(&[s.glyph], &g.fonts.small_icons, r, rgb(c), Align::Center);
            } else {
                g.text(&[s.glyph], &g.fonts.small_icons, rect(r.left + 10.0, r.top, 16.0, 24.0), rgb(c), Align::Center);
                g.text(&wide(&s.label), &g.fonts.body, rect(r.left + 32.0, r.top, r.right - r.left - 36.0, 24.0), rgb(c), Align::Left);
            }
        }
        g.pop_clip();
        // The chevron: up folds it to the header, down opens it.
        self.nav_button(g, toggle, Hit::ExplorerSidebar, if self.settings.dock_open { 0xE70E } else { 0xE70D }, true, false);

        // --- The places: the open section's, down columns as wide as fit, scrolling when they're taller.
        let body = rect(inner.left, inner.top + DOCK_HEAD, w, body_h);
        if body_h > 1.0 {
            if let Some(a) = active {
                moving |= self.draw_dock_places(g, body, body_full, &items, &secs[a]);
            }
        }

        // --- Its edge: a hairline, the grip line on hover and while held.
        g.hline(inner.left, g.snap(bottom), w, rgb(t.divider));
        let grip = (open > 0.5).then(|| rect(inner.left, bottom - 5.0, w, 10.0));
        if grip.is_some() {
            let dragging = self.explorer.dock_drag.is_some();
            let k = self.fades.hover(Hit::SidebarGrip, self.hover == Some(Hit::SidebarGrip) || dragging);
            ui::grip_line(g, rect(inner.left + 10.0, bottom - 1.0, w - 20.0, 2.0), k, dragging);
        }
        (bottom + 1.0, grip, moving)
    }

    /// The open section's places in `body` (cut off at its bottom while it folds; `full` is its unfolded height).
    fn draw_dock_places(&mut self, g: &Gfx, body: D2D_RECT_F, full: f32, items: &[SideItem], sec: &Section) -> bool {
        let t = theme::t();
        let now = Instant::now();
        let rh = row_h();
        let w = body.right - body.left - 2.0 * PAD;
        let cols = (((w + COL_GAP) / (COL_MIN + COL_GAP)).floor() as usize).max(1);
        let col_w = (w - COL_GAP * (cols - 1) as f32) / cols as f32;
        let range: Vec<usize> = (sec.first..sec.end).collect();
        let n = range.len();
        let rows = n.div_ceil(cols).max(1);
        self.hits.add(Hit::Sidebar, body);

        // Scrolling, eased as the list's.
        let content = rows as f32 * rh + 2.0 * PAD;
        self.explorer.side_max = (content - full).max(0.0);
        let (scroll, scrolling) = self.explorer.side_scroll_now();
        let scroll = scroll.clamp(0.0, self.explorer.side_max);
        if !scrolling {
            self.explorer.side_scroll = scroll;
        }
        let origin = (body.left + PAD, body.top + PAD - scroll);
        let slot_at = |k: usize| (origin.0 + (k / rows) as f32 * (col_w + COL_GAP), origin.1 + (k % rows) as f32 * rh);

        // Entries: a top-level place and the open tree under it. A bookmark or collection being dragged follows the
        // pointer; the others make room where it would land.
        let mut groups: Vec<(usize, usize)> = Vec::new(); // range of `range` indices
        for (k, &i) in range.iter().enumerate() {
            if items[i].depth <= 1 || groups.is_empty() {
                groups.push((k, k + 1));
            } else if let Some(gr) = groups.last_mut() {
                gr.1 = k + 1;
            }
        }
        let row_drag = self.explorer.side_row_drag.as_ref().filter(|d| d.active && d.sec == sec.sec).map(|d| (d.from, d.x - d.grab_x, d.y - d.grab));
        // Each item's slot in the flow (by `range` index), and the dragged entry's slot.
        let mut slot_of: Vec<usize> = (0..n).collect();
        let mut ghost = None;
        if let Some((from, px, py)) = row_drag.filter(|d| d.0 < groups.len()) {
            let others: Vec<usize> = (0..groups.len()).filter(|&gk| gk != from).collect();
            // The flow slot under the dragged row's middle.
            let (cx, cy) = (px + col_w / 2.0 - origin.0, py + rh / 2.0 - origin.1);
            let col = ((cx / (col_w + COL_GAP)).floor().max(0.0) as usize).min(cols - 1);
            let row = ((cy / rh).floor().max(0.0) as usize).min(rows - 1);
            let at = col * rows + row;
            let mut insert = 0;
            let mut k = 0;
            for &gk in &others {
                let size = groups[gk].1 - groups[gk].0;
                if k + size / 2 < at {
                    insert += 1;
                }
                k += size;
            }
            if let Some(d) = self.explorer.side_row_drag.as_mut() {
                d.insert = insert;
            }
            let mut k = 0;
            for (pos, &gk) in others.iter().enumerate() {
                if pos == insert {
                    ghost = Some(k);
                    k += groups[from].1 - groups[from].0;
                }
                for j in groups[gk].0..groups[gk].1 {
                    slot_of[j] = k;
                    k += 1;
                }
            }
            if ghost.is_none() {
                ghost = Some(k);
            }
        }
        let animate = row_drag.is_some() || now < self.explorer.row_anim_until;
        let mut moving = scrolling || row_drag.is_some();
        let place = self.explorer.place.clone();
        let target = self.explorer.row_drag.as_ref().filter(|d| d.active).and_then(|d| d.target.clone());
        let hover_tint = theme::mix(t.sidebar, t.accent, 0.2);

        g.push_clip(body);
        if let (Some(gk), Some((from, _, _))) = (ghost, row_drag) {
            let (sx, sy) = slot_at(gk);
            let sr = rect(sx, sy, col_w, rh * (groups[from].1 - groups[from].0) as f32);
            g.fill_round(sr, 4.0, gfx::rgba(t.accent, 0.10));
            ui::stroke(g, sr, 4.0, rgb(t.accent));
        }
        let dragged_items = row_drag.map(|(from, _, _)| groups.get(from).copied().unwrap_or((0, 0)));
        for (k, &i) in range.iter().enumerate() {
            let it = &items[i];
            let key = format!("{:?}|{}|{}", sec.sec, it.node.clone().unwrap_or_else(|| it.label.clone()), it.depth);
            let lifted = dragged_items.is_some_and(|(a, b)| k >= a && k < b);
            let (x, y) = if let (true, Some((_, px, py))) = (lifted, row_drag) {
                (px, py + (k - dragged_items.unwrap().0) as f32 * rh)
            } else {
                slot_at(slot_of[k])
            };
            let pos = [x - origin.0, y - origin.1];
            let e = self.explorer.dock_pos.entry(key).or_insert_with(|| Ease::at(pos));
            if lifted || !animate {
                *e = Ease::at(pos);
            } else {
                e.set(pos, now);
            }
            moving |= e.moving(now);
            if lifted {
                continue; // drawn last, over the rest
            }
            let [ex, ey] = e.value(now);
            // A hint ("Pin folders with...") runs across the columns.
            let span = if matches!(it.target, SideTarget::Hint) { w } else { col_w };
            let row = rect(origin.0 + ex, origin.1 + ey, span, rh);
            if row.bottom <= body.top || row.top >= body.bottom {
                continue;
            }
            self.dock_row(g, row, it, i, place.as_ref(), target.is_none(), hover_tint, true);
        }
        g.pop_clip();
        // The dragged entry, lifted where the pointer holds it.
        if let (Some((a, _)), Some((_, px, py))) = (dragged_items, row_drag) {
            let raised = theme::raised();
            let r = rect(px, py, col_w, rh);
            ui::shadow(g, r, 4.0);
            g.fill_round(r, 4.0, rgb(raised.popup));
            ui::stroke(g, r, 4.0, rgb(raised.divider));
            let i = range[a];
            self.dock_row(g, r, &items[i], i, None, false, hover_tint, false);
        }
        moving
    }

    /// One place in the dock (as the sidebar drew them): its hover wash, the one showing filled, a tree chevron,
    /// Windows' icon or the item's glyph, its name.
    #[allow(clippy::too_many_arguments)]
    fn dock_row(&mut self, g: &Gfx, row: D2D_RECT_F, it: &SideItem, i: usize, place: Option<&Place>, can_hover: bool, hover_tint: D2D1_COLOR_F, hit: bool) {
        let t = theme::t();
        let rh = row.bottom - row.top;
        match &it.target {
            SideTarget::Hint => {
                g.text(&wide(&it.label), &g.fonts.body, rect(row.left + 10.0, row.top, row.right - row.left - 14.0, rh), rgb(t.placeholder), Align::Left);
            }
            SideTarget::Header(_) => {}
            SideTarget::Place(p) => {
                if hit {
                    self.hits.add(Hit::SideRow(i), row);
                }
                let current = place.is_some_and(|c| c.same(p)) && it.depth >= 1;
                let hover = if hit { self.fades.hover(Hit::SideRow(i), can_hover && self.hover == Some(Hit::SideRow(i))) } else { 0.0 };
                if current {
                    g.fill_round(row, 4.0, rgb(t.selected));
                } else if hover > 0.0 {
                    g.fill_round(row, 4.0, D2D1_COLOR_F { a: hover, ..hover_tint });
                }
                if hover > 0.0 {
                    ui::stroke(g, row, 4.0, gfx::rgba(t.accent, hover));
                }
                let x = row.left + 4.0 + (it.depth.saturating_sub(1)) as f32 * 14.0;
                if it.node.is_some() {
                    let cr = rect(x, row.top, 16.0, rh);
                    if hit {
                        self.hits.add(Hit::SideChevron(i), cr);
                    }
                    g.text(&[if it.expanded { 0xE70D } else { 0xE76C }], &g.fonts.caption_icons, cr, rgb(t.placeholder), Align::Center);
                }
                let ix = x + 18.0;
                let shell = if it.depth > 1 { Some(shell_icons::Kind::Folder) } else { place_icon(p) };
                let drew = shell.is_some_and(|k| shell_icon(g, &k, rect(ix, row.top, 16.0, rh), 1.0, None));
                if drew {
                    // Windows' own.
                } else if it.glyph == 0xEA3B {
                    let c = it.tint.unwrap_or(t.icon);
                    g.fill_round(rect(ix + 3.0, row.top + (rh - 10.0) / 2.0, 10.0, 10.0), 5.0, rgb(c));
                } else {
                    let glyph = if it.glyph == 0xE8B7 { 0xE8D5 } else { it.glyph };
                    g.text(&[glyph], &g.fonts.body_icons, rect(ix, row.top, 16.0, rh), rgb(it.tint.unwrap_or(t.icon)), Align::Center);
                }
                g.text(&wide(&it.label), &g.fonts.body, rect(ix + 24.0, row.top, (row.right - ix - 28.0).max(0.0), rh), rgb(t.text), Align::Left);
            }
        }
    }

    // --- Picking a section, folding the dock ------------------------------------------------------------

    /// A click on a section's tab shows it; a click on the open one while the dock is folded opens the dock.
    pub(super) fn dock_pick(&mut self, sec: SideSection) {
        if self.settings.dock_tab == Some(sec) && self.settings.dock_open {
            return;
        }
        if self.settings.dock_tab != Some(sec) {
            self.explorer.side_scroll = 0.0;
            self.explorer.side_anim = None;
        }
        self.settings.dock_tab = Some(sec);
        self.settings.dock_open = true;
        settings::save(&self.settings);
        self.invalidate();
    }

    pub(super) fn dock_toggle(&mut self) {
        self.settings.dock_open = !self.settings.dock_open;
        settings::save(&self.settings);
        self.invalidate();
    }

    // --- Its edge ----------------------------------------------------------------------------------------

    pub(super) fn dock_grip_press(&mut self, y: f32) {
        let e = &mut self.explorer;
        e.dock_spring = None;
        e.dock_stretch = 0.0;
        e.dock_settle = None;
        let h = self.settings.dock_height.clamp(DOCK_MIN, e.dock_max.max(DOCK_MIN));
        e.dock_drag = Some((y, h));
    }

    pub(super) fn dock_grip_drag(&mut self, y: f32) {
        let Some((y0, h0)) = self.explorer.dock_drag else { return };
        let max = self.explorer.dock_max.max(DOCK_MIN);
        let raw = (h0 + y - y0).round();
        let h = raw.clamp(DOCK_MIN, max);
        // Pulled on past either end, it gives a little on the panes' rubber band and springs back when let go.
        let stretch = if raw > max {
            rubber(raw - max)
        } else if raw < DOCK_MIN {
            -rubber(DOCK_MIN - raw)
        } else {
            0.0
        };
        if h != self.settings.dock_height || stretch != self.explorer.dock_stretch {
            self.settings.dock_height = h;
            self.explorer.dock_stretch = stretch;
            self.invalidate();
        }
    }

    pub(super) fn dock_grip_release(&mut self) {
        if self.explorer.dock_stretch != 0.0 {
            self.explorer.dock_spring = Some((self.explorer.dock_stretch, Instant::now()));
            self.explorer.dock_stretch = 0.0;
            self.invalidate();
        }
        if self.explorer.dock_drag.take().is_some() {
            settings::save(&self.settings);
        }
    }

    /// A double-click on its edge: it eases back to its default height.
    pub(super) fn dock_grip_reset(&mut self) {
        self.explorer.dock_drag = None;
        self.explorer.dock_settle = Some(crate::motion::Settle::from(self.dock_body_full()));
        self.settings.dock_height = DOCK_HEIGHT;
        settings::save(&self.settings);
        self.invalidate();
    }
}
