//! The thumbnail strip (Controls/ThumbnailStrip): a horizontal filmstrip under the viewport, full window
//! width, above the status row. 4:3 cells letterboxed on a faint fill, sitting flush on the bottom edge with
//! an 8 px band above them that is also the resize grip (56–480 DIPs, double-click resets to 96). The
//! current image's cell has the accent border and is glided to the centre on every navigation (jumped to
//! when it is more than a screen away). Clicking a cell opens it; the wheel scrolls the strip.
//!
//! Only cells on screen load, and only after staying on screen for 120 ms (cells within two of the current
//! one start at once), so a fling never queues extractions for what it flew past. Thumbnails stay on the GPU
//! in a small LRU; one at a smaller size keeps showing while a sharper one loads.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::Instant;

use super::*;
use crate::engine::Lru;
use crate::settings::{STRIP_HEIGHT, STRIP_MAX, STRIP_MIN};
use crate::thumbs;

const TOP_PAD: f32 = 8.0;
const SPACING: f32 = 4.0;
const SIDE: f32 = 8.0;
const DEBOUNCE_MS: u128 = 120;
const NEAR: usize = 2;
const SCROLL_MS: f32 = 200.0;
const FADE_W: f32 = 56.0;
const THUMB_BUDGET: usize = 96 << 20;
/// The strip never takes more of the window than leaves this much for the chrome and the image.
const MIN_REST_H: f32 = 260.0;

pub(super) struct Thumb {
    size: u32,
    width: u32,
    height: u32,
    bmp: ID2D1Bitmap1,
}

type FileKey = (PathBuf, u64);

/// Thumbnails for everything that shows them (the strip, the explorer card's rows): one worker pool, one GPU
/// cache keyed by file (any size at least as big as asked for will do), and the per-frame want list that
/// drives loading. Each frame, whatever is on screen calls [`Thumbs::want`]; [`App::pump_thumbs`] then submits
/// what has been on screen long enough and drops queued work for everything that left.
pub(super) struct Thumbs {
    pool: Option<Arc<thumbs::Pool>>,
    cache: Lru<FileKey, Rc<Thumb>>,
    pending: HashSet<thumbs::Job>,
    failed: HashSet<thumbs::Job>,
    /// When each waiting item first came on screen (the load debounce).
    seen: HashMap<FileKey, Instant>,
    wanted: Vec<(thumbs::Job, bool)>,
}

impl Thumbs {
    pub fn new() -> Thumbs {
        Thumbs {
            pool: None,
            cache: Lru::new(THUMB_BUDGET),
            pending: HashSet::new(),
            failed: HashSet::new(),
            seen: HashMap::new(),
            wanted: Vec::new(),
        }
    }

    pub fn get(&mut self, path: &Path, stamp: u64) -> Option<Rc<Thumb>> {
        self.cache.get(&(path.to_path_buf(), stamp)).cloned()
    }

    /// Ask for a thumbnail of at least `size` px this frame (no-op when one that big is cached). `near`: the
    /// user is looking right at it, skip the debounce.
    pub fn want(&mut self, path: &Path, stamp: u64, size: u32, cloud: bool, near: bool) {
        if self.cache.get(&(path.to_path_buf(), stamp)).is_some_and(|t| t.size >= size) {
            return;
        }
        let job = thumbs::Job { path: path.to_path_buf(), stamp, size, cloud };
        if !self.failed.contains(&job) {
            self.wanted.push((job, near));
        }
    }

    /// Bitmaps die with their device (WARP to hardware): reload them.
    pub fn device_lost(&mut self) {
        self.cache.clear();
    }
}

pub(super) struct Strip {
    scroll: f32,
    anim: Option<(f32, f32, Instant)>,
    max_scroll: f32,
    /// The (index, image count) the strip last centred on.
    centered: Option<(usize, usize)>,
    /// A grip drag: the pointer y and the height when it started.
    pub drag: Option<(f32, f32)>,
}

impl Strip {
    pub fn new() -> Strip {
        Strip {
            scroll: 0.0,
            anim: None,
            max_scroll: 0.0,
            centered: None,
            drag: None,
        }
    }

    fn scroll_now(&mut self) -> (f32, bool) {
        let Some((from, to, t0)) = self.anim else { return (self.scroll, false) };
        let t = (t0.elapsed().as_secs_f32() * 1000.0 / SCROLL_MS).min(1.0);
        if t >= 1.0 {
            self.anim = None;
            self.scroll = to;
            return (to, false);
        }
        let k = 1.0 - (1.0 - t).powi(3);
        (from + (to - from) * k, true)
    }

    fn target(&self) -> f32 {
        self.anim.map_or(self.scroll, |(_, to, _)| to)
    }

    fn scroll_to(&mut self, to: f32, animate: bool) {
        let (now, _) = self.scroll_now();
        if animate {
            self.scroll = now;
            self.anim = Some((now, to, Instant::now()));
        } else {
            self.anim = None;
            self.scroll = to;
        }
    }
}

impl Thumb {
    /// Draws it letterboxed (whole, centred) inside `r`.
    pub fn draw_fit(&self, g: &Gfx, r: D2D_RECT_F) {
        let (iw, ih) = (r.right - r.left, r.bottom - r.top);
        let s = (iw / self.width as f32).min(ih / self.height as f32);
        let (dw, dh) = (self.width as f32 * s, self.height as f32 * s);
        g.draw_bitmap(&self.bmp, rect(r.left + (iw - dw) / 2.0, r.top + (ih - dh) / 2.0, dw, dh), 1.0);
    }
}

impl App {
    pub(super) fn strip_shown(&self) -> bool {
        self.settings.strip_visible && self.panels_allowed()
    }

    /// What the strip takes from the bottom of the viewport.
    pub(super) fn bottom_inset(&self) -> f32 {
        self.settings.strip_height * self.slides.strip.value()
    }

    /// Slid down (under the status row, which clips it) by however much of it is hidden.
    fn strip_rect(&self) -> D2D_RECT_F {
        let (w, _) = self.size_dip();
        let bottom = self.content_bottom() + self.settings.strip_height * (1.0 - self.slides.strip.value());
        D2D_RECT_F { left: 0.0, top: bottom - self.settings.strip_height, right: w, bottom }
    }

    fn cell_size(&self) -> (f32, f32) {
        let ch = (self.settings.strip_height - TOP_PAD).max(32.0);
        ((ch * 4.0 / 3.0).round(), ch)
    }

    /// The decode size for the cells: the cell width in device pixels, in 64 px steps so a small resize
    /// doesn't reload every thumbnail.
    fn thumb_px(&self) -> u32 {
        let px = self.cell_size().0 * self.scale();
        ((px / 64.0).ceil() as u32 * 64).clamp(96, 1024)
    }

    pub(super) fn toggle_strip(&mut self) {
        if self.viewer.current.is_none() {
            return;
        }
        self.settings.strip_visible = !self.settings.strip_visible;
        self.strip.centered = None;
        settings::save(&self.settings);
        self.slide_panels();
    }

    /// The wheel over the strip moves it a cell per notch.
    pub(super) fn scroll_strip(&mut self, delta: f32) {
        let pitch = self.cell_size().0 + SPACING;
        let s = &mut self.strip;
        let to = (s.target() - delta / 120.0 * pitch).clamp(0.0, s.max_scroll.max(0.0));
        s.scroll_to(to, true);
        self.invalidate();
    }

    pub(super) fn strip_grip_press(&mut self, y: f32) {
        self.strip.drag = Some((y, self.settings.strip_height));
    }

    pub(super) fn strip_grip_drag(&mut self, y: f32) {
        let Some((y0, h0)) = self.strip.drag else { return };
        let (_, wh) = self.size_dip();
        let cap = STRIP_MAX.min((wh - MIN_REST_H).max(STRIP_MIN));
        // Dragging up makes the strip taller.
        let h = (h0 + (y0 - y)).clamp(STRIP_MIN, cap).round();
        if h != self.settings.strip_height {
            self.settings.strip_height = h;
            self.strip.centered = None;
            self.layout_changed();
        }
    }

    pub(super) fn strip_grip_release(&mut self) {
        if self.strip.drag.take().is_some() {
            settings::save(&self.settings);
            self.invalidate();
        }
    }

    pub(super) fn strip_grip_reset(&mut self) {
        self.strip.drag = None;
        self.settings.strip_height = STRIP_HEIGHT;
        self.strip.centered = None;
        settings::save(&self.settings);
        self.layout_changed();
    }

    /// Finished thumbnails: onto the GPU and into the cache.
    pub(super) fn on_thumbs(&mut self) {
        let Some(pool) = self.thumbs.pool.clone() else { return };
        let Some(g) = &self.gfx else { return };
        for done in pool.take_results() {
            self.thumbs.pending.remove(&done.job);
            let Some((w, h, px)) = done.image else {
                self.thumbs.failed.insert(done.job);
                continue;
            };
            let Ok(bmp) = g.bitmap(w, h, &px) else { continue };
            let key = (done.job.path.clone(), done.job.stamp);
            let thumb = Thumb { size: done.job.size, width: w, height: h, bmp };
            self.thumbs.cache.insert(key, Rc::new(thumb), (w * h * 4) as usize);
        }
        self.invalidate();
    }

    /// After a frame: load what has been on screen long enough (or is right by the current image), drop
    /// queued loads for everything that left the screen, and wake again for the ones still waiting.
    pub(super) fn pump_thumbs(&mut self) {
        let wanted = std::mem::take(&mut self.thumbs.wanted);
        if wanted.is_empty() && self.thumbs.pending.is_empty() {
            self.thumbs.seen.clear();
            return;
        }
        let hwnd = self.hwnd;
        let t = &mut self.thumbs;
        let pool = t.pool.get_or_insert_with(|| thumbs::Pool::start(hwnd)).clone();
        let now = Instant::now();
        let mut waiting = false;
        let visible: HashSet<thumbs::Job> = wanted.iter().map(|(j, _)| j.clone()).collect();
        for (job, near) in wanted {
            if t.pending.contains(&job) {
                continue;
            }
            let since = *t.seen.entry((job.path.clone(), job.stamp)).or_insert(now);
            if near || now.duration_since(since).as_millis() >= DEBOUNCE_MS {
                t.pending.insert(job.clone());
                pool.submit(job);
            } else {
                waiting = true;
            }
        }
        for j in pool.retain(|j| visible.contains(j)) {
            t.pending.remove(&j);
        }
        let on_screen: HashSet<FileKey> = visible.iter().map(|j| (j.path.clone(), j.stamp)).collect();
        t.seen.retain(|k, _| on_screen.contains(k));
        if waiting {
            unsafe {
                SetTimer(Some(hwnd), TIMER_THUMBS, DEBOUNCE_MS as u32 + 5, None);
            }
        }
    }

    pub(super) fn strip_tooltip(&self, i: usize) -> Option<String> {
        self.viewer.listing.as_ref()?.images.get(i).map(|e| file_name(&e.path))
    }

    /// Draws the strip, loads what it shows, and records its hits. Returns whether it is still animating.
    pub(super) fn draw_strip(&mut self, g: &Gfx) -> bool {
        if self.slides.strip.value() <= 0.0 {
            return false;
        }
        let r = self.strip_rect();
        // Only what is above the status row shows while it slides.
        let visible = D2D_RECT_F { bottom: r.bottom.min(self.content_bottom()), ..r };
        g.push_clip(visible);
        let (cw, ch) = self.cell_size();
        let pitch = cw + SPACING;
        let n = self.viewer.image_count();
        let view_w = r.right - r.left;
        let content_w = SIDE * 2.0 + n as f32 * pitch - if n > 0 { SPACING } else { 0.0 };
        self.strip.max_scroll = (content_w - view_w).max(0.0);
        self.hits.add(Hit::Strip, visible);

        // Follow the current image: glide to centre it, or jump when it is more than a screen away.
        let sel = self.viewer.index;
        if let Some(i) = sel {
            if self.strip.centered != Some((i, n)) {
                let target = (SIDE + i as f32 * pitch + cw / 2.0 - view_w / 2.0).clamp(0.0, self.strip.max_scroll);
                let animate = self.strip.centered.is_some() && (target - self.strip.target()).abs() <= view_w;
                self.strip.scroll_to(target, animate);
                self.strip.centered = Some((i, n));
            }
        }
        let (mut scroll, moving) = self.strip.scroll_now();
        scroll = scroll.clamp(0.0, self.strip.max_scroll);

        let want = self.thumb_px();
        let top = r.bottom - ch;
        let first = (((scroll - SIDE) / pitch).floor().max(0.0)) as usize;
        let last = (((scroll + view_w - SIDE) / pitch).ceil().max(0.0) as usize).min(n);
        // Edge fades where there is more to scroll to: the cells fade out into the photo's background.
        let lf = self.fades.get(Hit::StripFadeLeft, scroll > 0.5);
        let rf = self.fades.get(Hit::StripFadeRight, self.strip.max_scroll - scroll > 0.5);
        g.push_clip(r);
        g.push_hfade_layer(r, FADE_W, lf, rf);
        for i in first..last {
            let Some(e) = self.viewer.listing.as_ref().and_then(|l| l.images.get(i)) else { break };
            let (path, stamp, cloud) = (e.path.clone(), e.stamp, e.cloud);
            let cell = rect(g.snap(SIDE + i as f32 * pitch - scroll), top, cw, ch);
            self.hits.add(Hit::StripCell(i), cell);
            let selected = sel == Some(i);
            let hover = self.fades.get_out(Hit::StripCell(i), self.hover == Some(Hit::StripCell(i)));
            let pressed = self.pressed == Some(Hit::StripCell(i)) && self.hover == Some(Hit::StripCell(i));
            g.fill_round(cell, 4.0, gfx::rgb(0x2C2C2C));

            // Thumbnail, letterboxed in the cell inside its 2 px border.
            let inner = D2D_RECT_F { left: cell.left + 2.0, top: cell.top + 2.0, right: cell.right - 2.0, bottom: cell.bottom - 2.0 };
            if let Some(t) = self.thumbs.get(&path, stamp) {
                t.draw_fit(g, inner);
            }
            if pressed {
                g.fill_round(inner, 2.0, gfx::rgba(0xFFFFFF, 0.35));
            } else if hover > 0.0 {
                g.fill_round(inner, 2.0, gfx::rgba(0xFFFFFF, 0.2 * hover));
            }
            let border = if selected {
                Some(gfx::rgba(ui::ACCENT, 1.0))
            } else if pressed {
                Some(gfx::rgba(0xFFFFFF, 0.9))
            } else if hover > 0.0 {
                Some(gfx::rgba(0xFFFFFF, 0.6 * hover))
            } else {
                None
            };
            if let Some(c) = border {
                g.outline_round(cell, 4.0, c, 2.0);
            }

            // Load it (again, sharper) unless one of the right size is here or on its way.
            self.thumbs.want(&path, stamp, want, cloud, sel.is_some_and(|s| s.abs_diff(i) <= NEAR));
        }
        g.pop_layer();
        g.pop_clip();

        // The grip: the top band, a line that shows on hover and turns accent while dragging.
        let grip = rect(r.left, r.top, r.right - r.left, TOP_PAD);
        self.hits.add(Hit::StripGrip, grip);
        let dragging = self.strip.drag.is_some();
        let t = self.fades.get(Hit::StripGrip, self.hover == Some(Hit::StripGrip) || dragging);
        if t > 0.0 {
            let c = if dragging { gfx::rgba(ui::ACCENT, 1.0) } else { gfx::rgba(0xFFFFFF, 0.4 * t) };
            g.fill(rect(r.left, r.top, r.right - r.left, 2.0), c);
        }
        g.pop_clip();
        moving
    }
}
