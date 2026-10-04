//! What image is current, what is on screen, and which decodes run: the progressive loading of
//! `ImageViewport.ShowAsync` / `DecodeStagesAsync` and the preload scheduler, minus the async.
//!
//! - Navigation never waits for a decode. The previous image stays up until the new one's first frame lands.
//! - Two tiers: a `LOW_DIM` placeholder, then the viewport's sharp bucket swapped in over it (same native
//!   size, so no zoom jump). When the sharp bucket is no bigger than the placeholder, there is one tier only.
//! - Burst coalescing: a navigation within 200 ms of the previous one (held key, wheel run) waits 50 ms before
//!   its placeholder decode and 120 ms before its sharp one, so a run only pays for the images it stops on.
//!   Cached frames still show at every step, and preloads are scheduled at once.
//! - A queued preload of the image that just became current is promoted rather than decoded again.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{FILETIME, HWND};
use windows::Win32::Graphics::Direct2D::ID2D1Bitmap1;
use windows::Win32::UI::WindowsAndMessaging::{KillTimer, SetTimer};

use crate::decode::{Decoded, Job, Pool, Priority};
use crate::engine::{self, FULL, Key, LOW, LOW_DIM, Lru, Tier};
use crate::folder::Listing;
use crate::format::Format;
use crate::gfx::Gfx;
use crate::imaging::{MAX_EDGE, VECTOR_MAX_EDGE};

pub const TIMER_SETTLE: usize = 4;
pub const TIMER_ANIM: usize = 3;
const BURST_WINDOW: Duration = Duration::from_millis(200);
const LOW_SETTLE: Duration = Duration::from_millis(50);
const SHARP_SETTLE: Duration = Duration::from_millis(120);
pub const DEFAULT_BUDGET: usize = 512 << 20;

/// One decoded image on the GPU.
pub struct Entry {
    pub key: Key,
    /// One bitmap for a still image; every frame of an animation, with its delay.
    pub frames: RefCell<Vec<(ID2D1Bitmap1, u32)>>,
    pub width: u32,
    pub height: u32,
    pub native_w: u32,
    pub native_h: u32,
    pub format: Format,
    pub taken: Option<FILETIME>,
    pub pages: u32,
    pub vector: bool,
    /// The decoded pixels, kept only while drawing on WARP so the bitmaps can be re-made on the GPU.
    pixels: RefCell<Option<Vec<Vec<u8>>>>,
}

impl Entry {
    pub fn animated(&self) -> bool {
        self.frames.borrow().len() > 1
    }
    fn bytes(&self) -> usize {
        self.width as usize * self.height as usize * 4 * self.frames.borrow().len()
    }

    /// The same decode under another key (the file was renamed). Bitmaps are shared, not copied.
    fn rekeyed(&self, key: Key) -> Entry {
        Entry {
            key,
            frames: RefCell::new(self.frames.borrow().clone()),
            width: self.width,
            height: self.height,
            native_w: self.native_w,
            native_h: self.native_h,
            format: self.format,
            taken: self.taken,
            pages: self.pages,
            vector: self.vector,
            pixels: RefCell::new(self.pixels.borrow_mut().take()),
        }
    }
}

/// What the window must do after a viewer call.
#[derive(Default)]
pub struct Outcome {
    /// A new image is on screen: reset the zoom to fit this native size.
    pub reset_view: Option<(u32, u32)>,
    pub redraw: bool,
}

impl Outcome {
    fn merge(&mut self, o: Outcome) {
        if o.reset_view.is_some() {
            self.reset_view = o.reset_view;
        }
        self.redraw |= o.redraw;
    }
}

/// What deleting a file did to the view.
pub enum Removed {
    /// Not the current image: only the folder changed.
    Other,
    /// The current image went; the next one is being shown.
    Current(Outcome),
    /// It was the folder's last image.
    Emptied,
}

pub struct Viewer {
    hwnd: HWND,
    pool: Arc<Pool>,
    cache: Lru<Key, Rc<Entry>>,
    /// Submitted and not yet back (queued or running).
    pending: HashSet<Key>,
    /// Current-image requests waiting out a burst.
    deferred: Vec<(Key, Instant)>,
    /// The preload window last scheduled, for deciding whether a finished preload is still wanted.
    window: HashSet<Key>,
    failed: HashMap<(PathBuf, u64), String>,
    pub listing: Option<Listing>,
    pub index: Option<usize>,
    /// The image the user is on (path, last-write stamp).
    pub current: Option<(PathBuf, u64)>,
    /// What is drawn: the current image, or the previous one until the current one's first frame lands.
    pub shown: Option<Rc<Entry>>,
    pub error: Option<String>,
    pub anim_frame: usize,
    forward: bool,
    last_nav: Option<Instant>,
    /// The sharp bucket for the current viewport.
    sharp: i32,
    /// The viewport in device pixels.
    fit_box: (u32, u32),
}

impl Viewer {
    pub fn new(hwnd: HWND, pool: Arc<Pool>) -> Viewer {
        Viewer {
            hwnd,
            pool,
            cache: Lru::new(DEFAULT_BUDGET),
            pending: HashSet::new(),
            deferred: Vec::new(),
            window: HashSet::new(),
            failed: HashMap::new(),
            listing: None,
            index: None,
            current: None,
            shown: None,
            error: None,
            anim_frame: 0,
            forward: true,
            last_nav: None,
            sharp: engine::bucket_for(LOW_DIM),
            fit_box: (LOW_DIM, LOW_DIM),
        }
    }

    /// Decodes submitted before the viewer existed (the launch image, from `main`).
    pub fn adopt_pending(&mut self, keys: impl IntoIterator<Item = Key>) {
        self.pending.extend(keys);
    }

    /// The viewport changed size or DPI: the sharp tier follows it.
    pub fn set_fit_box(&mut self, w: u32, h: u32) {
        self.fit_box = (w, h);
        self.sharp = engine::bucket_for(w.max(h));
    }

    /// The sharp bucket for a fit box, as `main` predicts it before the window exists.
    pub fn sharp_bucket_for(w: u32, h: u32) -> i32 {
        engine::bucket_for(w.max(h))
    }

    /// Whether a placeholder tier is worth decoding: only when the sharp one is bigger than it.
    pub fn uses_low_tier(sharp: i32) -> bool {
        sharp > engine::bucket_for(LOW_DIM)
    }

    /// The shown entry when it is the current image (not the previous one still on screen).
    pub fn current_entry(&self) -> Option<&Rc<Entry>> {
        let (p, s) = self.current.as_ref()?;
        self.shown.as_ref().filter(|e| &e.key.path == p && e.key.stamp == *s)
    }

    pub fn image_count(&self) -> usize {
        self.listing.as_ref().map_or(0, |l| l.images.len())
    }

    // --- Navigation ---------------------------------------------------------------------------------

    pub fn show_index(&mut self, i: usize, gfx: Option<&Gfx>) -> Outcome {
        let Some(l) = &self.listing else { return Outcome::default() };
        let Some(e) = l.images.get(i) else { return Outcome::default() };
        let (path, stamp) = (e.path.clone(), e.stamp);
        if let Some(cur) = self.index {
            self.forward = i >= cur;
        }
        self.index = Some(i);
        self.show(path, stamp, gfx)
    }

    /// One step through the folder, wrapping at the ends like the C# app.
    pub fn step(&mut self, delta: isize, gfx: Option<&Gfx>) -> Outcome {
        let (n, Some(i)) = (self.image_count(), self.index) else { return Outcome::default() };
        if n < 2 {
            return Outcome::default();
        }
        let j = ((i as isize + delta) % n as isize + n as isize) % n as isize;
        let out = self.show_index(j as usize, gfx);
        self.forward = delta > 0;
        out
    }

    pub fn show(&mut self, path: PathBuf, stamp: u64, _gfx: Option<&Gfx>) -> Outcome {
        let now = Instant::now();
        let burst = self.last_nav.is_some_and(|t| now - t < BURST_WINDOW);
        self.last_nav = Some(now);
        self.current = Some((path.clone(), stamp));
        self.error = self.failed.get(&(path.clone(), stamp)).cloned();
        if self.error.is_some() {
            self.shown = None; // "Can't display", not the previous photo
        }
        self.deferred.clear();
        self.restart_animation();
        let mut out = Outcome { redraw: true, ..Default::default() };

        if let Some(best) = self.best_cached(&path, stamp) {
            if self.shown.as_ref().is_none_or(|s| !Rc::ptr_eq(s, &best)) {
                out.reset_view = Some((best.native_w, best.native_h));
                self.shown = Some(best.clone());
                self.restart_animation();
            }
            if !self.covers(&best) && !(best.animated() && best.key.bucket != LOW) {
                self.request(Key::new(&path, stamp, self.sharp), if burst { SHARP_SETTLE } else { Duration::ZERO });
            }
        } else if self.error.is_none() {
            if Self::uses_low_tier(self.sharp) {
                self.request(Key::new(&path, stamp, LOW), if burst { LOW_SETTLE } else { Duration::ZERO });
            }
            self.request(Key::new(&path, stamp, self.sharp), if burst { SHARP_SETTLE } else { Duration::ZERO });
        }
        self.schedule_preloads();
        out
    }

    pub fn close(&mut self) {
        self.listing = None;
        self.index = None;
        self.current = None;
        self.shown = None;
        self.error = None;
        self.deferred.clear();
        self.window.clear();
        self.cache.clear();
        for k in self.pool.retain(|_| false) {
            self.pending.remove(&k);
        }
        self.restart_animation();
    }

    /// A folder listing arrived (or changed): find the current image in it and preload around it.
    pub fn set_listing(&mut self, listing: Listing) -> bool {
        let Some((cur, _)) = &self.current else { return false };
        if Some(listing.folder.as_path()) != cur.parent() {
            return false;
        }
        self.index = listing.images.iter().position(|e| &e.path == cur);
        self.listing = Some(listing);
        self.schedule_preloads();
        true
    }

    /// Drops every cached decode and remembered failure of a file (it was rewritten or deleted).
    pub fn forget(&mut self, path: &Path) {
        let keys: Vec<Key> = self.cache.keys().filter(|k| k.path == path).cloned().collect();
        for k in keys {
            self.cache.remove(&k);
        }
        self.failed.retain(|(p, _), _| p != path);
    }

    /// A file was renamed: its decodes move to the new name, so the pixels on screen stay put and nothing is
    /// decoded again, and the folder re-sorts with the current image still current.
    pub fn renamed(&mut self, from: &Path, to: &Path) {
        let mut moved: Vec<(Rc<Entry>, Rc<Entry>)> = Vec::new();
        let keys: Vec<Key> = self.cache.keys().filter(|k| k.path == from).cloned().collect();
        for k in keys {
            let Some(old) = self.cache.remove(&k) else { continue };
            let key = Key::new(to, k.stamp, k.bucket);
            let new = Rc::new(old.rekeyed(key.clone()));
            self.cache.insert(key, new.clone(), new.bytes());
            moved.push((old, new));
        }
        if let Some(s) = self.shown.clone().filter(|s| s.key.path == from) {
            let new = moved
                .iter()
                .find(|(o, _)| Rc::ptr_eq(o, &s))
                .map(|(_, n)| n.clone())
                .unwrap_or_else(|| Rc::new(s.rekeyed(Key::new(to, s.key.stamp, s.key.bucket))));
            self.shown = Some(new);
        }
        let failed: Vec<_> = self.failed.keys().filter(|(p, _)| p == from).cloned().collect();
        for (p, stamp) in failed {
            if let Some(e) = self.failed.remove(&(p, stamp)) {
                self.failed.insert((to.to_path_buf(), stamp), e);
            }
        }
        if let Some((p, _)) = &mut self.current {
            if p == from {
                *p = to.to_path_buf();
            }
        }
        if let Some(l) = &mut self.listing {
            l.rename(from, to);
            let cur = self.current.as_ref().map(|(p, _)| p.clone());
            self.index = cur.and_then(|c| l.images.iter().position(|e| e.path == c));
        }
        self.schedule_preloads();
    }

    /// A file was deleted. When it was the current image, whatever slid into its place is shown (the
    /// previous one when it was last), or nothing when the folder has no images left.
    pub fn remove(&mut self, path: &Path, gfx: Option<&Gfx>) -> Removed {
        self.forget(path);
        let was_current = self.current.as_ref().is_some_and(|(p, _)| p == path);
        let Some(l) = &mut self.listing else { return if was_current { Removed::Emptied } else { Removed::Other } };
        let old = l.images.iter().position(|e| e.path == path);
        l.remove(path);
        if !was_current {
            let cur = self.current.as_ref().map(|(p, _)| p.clone());
            self.index = cur.and_then(|c| l.images.iter().position(|e| e.path == c));
            self.schedule_preloads();
            return Removed::Other;
        }
        if l.images.is_empty() {
            return Removed::Emptied;
        }
        let j = old.unwrap_or(0).min(l.images.len() - 1);
        self.index = None;
        self.forward = true;
        Removed::Current(self.show_index(j, gfx))
    }

    /// Whether a sharp decode is already enough for this viewport. The bucket is sized for the viewport's long
    /// edge whatever the image's shape, so once the native size is known the real fit size decides: a 3:2
    /// photo in a 2560 x 1355 viewport fits at 2033 px wide, so a 2048 decode already covers it.
    fn covers(&self, e: &Entry) -> bool {
        if e.key.bucket == LOW && Self::uses_low_tier(self.sharp) {
            return false;
        }
        if e.key.bucket == FULL || e.key.bucket >= self.sharp {
            return true;
        }
        let (bw, bh) = self.fit_box;
        let (nw, nh) = (e.native_w.max(1) as f64, e.native_h.max(1) as f64);
        let scale = (bw as f64 / nw).min(bh as f64 / nh).min(1.0);
        let needed = (nw.max(nh) * scale).ceil() as u32;
        e.width.max(e.height) + 1 >= needed
    }

    /// Re-sorts the open folder in place; the current image stays current.
    pub fn set_sort(&mut self, sort: crate::folder::Sort) {
        let Some(l) = &mut self.listing else { return };
        l.resort(sort);
        let cur = self.current.as_ref().map(|(p, _)| p.clone());
        self.index = cur.and_then(|c| l.images.iter().position(|e| e.path == c));
        self.schedule_preloads();
    }

    /// The largest cached decode of this file (any tier).
    fn best_cached(&mut self, path: &Path, stamp: u64) -> Option<Rc<Entry>> {
        let best = self
            .cache
            .keys()
            .filter(|k| k.path == path && k.stamp == stamp)
            .max_by_key(|k| k.bucket)
            .cloned()?;
        self.cache.get(&best).cloned()
    }

    // --- Requests -----------------------------------------------------------------------------------

    fn request(&mut self, key: Key, settle: Duration) {
        if self.cache.contains(&key) {
            return;
        }
        if self.pending.contains(&key) {
            self.pool.promote(&key); // a queued preload of this image: move it up instead of decoding twice
            return;
        }
        if settle.is_zero() {
            self.pending.insert(key.clone());
            self.pool.submit(Job { key, priority: Priority::Current });
        } else {
            self.deferred.push((key, Instant::now() + settle));
            self.arm_settle();
        }
    }

    fn arm_settle(&self) {
        if let Some(due) = self.deferred.iter().map(|d| d.1).min() {
            let ms = due.saturating_duration_since(Instant::now()).as_millis().max(1) as u32;
            unsafe {
                SetTimer(Some(self.hwnd), TIMER_SETTLE, ms, None);
            }
        }
    }

    /// The burst settled on this image: submit what was held back.
    pub fn on_settle_timer(&mut self) {
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_SETTLE);
        }
        let now = Instant::now();
        let (due, later): (Vec<_>, Vec<_>) = std::mem::take(&mut self.deferred).into_iter().partition(|d| d.1 <= now);
        self.deferred = later;
        for (key, _) in due {
            if self.current.as_ref().is_some_and(|(p, s)| *p == key.path && *s == key.stamp) {
                self.request(key, Duration::ZERO);
            }
        }
        self.arm_settle();
    }

    /// The preload window around the current image; drops queued work for images that left it.
    fn schedule_preloads(&mut self) {
        let mut desired: Vec<Key> = Vec::new();
        if let (Some(l), Some(i)) = (&self.listing, self.index) {
            let low_tier = Self::uses_low_tier(self.sharp);
            for (j, tier) in engine::preload_window(l.images.len(), i, self.forward) {
                let e = &l.images[j];
                if e.cloud || self.failed.contains_key(&(e.path.clone(), e.stamp)) {
                    continue; // reading even the header of an online-only file downloads it
                }
                let bucket = if tier == Tier::Low && low_tier { LOW } else { self.sharp };
                let k = Key::new(&e.path, e.stamp, bucket);
                if !desired.contains(&k) {
                    desired.push(k);
                }
            }
        }
        let cur = self.current.clone();
        let set: HashSet<Key> = desired.iter().cloned().collect();
        let dropped = self.pool.retain(|j| {
            set.contains(&j.key) || cur.as_ref().is_some_and(|(p, s)| j.priority == Priority::Current && *p == j.key.path && *s == j.key.stamp)
        });
        for k in dropped {
            self.pending.remove(&k);
        }
        for k in &desired {
            // A sharp decode already cached covers the placeholder too.
            let covered = self
                .cache
                .values()
                .any(|c| c.key.path == k.path && c.key.stamp == k.stamp && (c.key.bucket >= k.bucket || (k.bucket != LOW && self.covers(c))));
            if !covered && !self.pending.contains(k) {
                self.pending.insert(k.clone());
                self.pool.submit(Job { key: k.clone(), priority: Priority::Preload });
            }
        }
        self.window = set;
    }

    /// Zoomed or resized past what is on screen: decode sharper. `needed` is the drawn long edge in device px.
    pub fn upgrade(&mut self, needed: u32, max_bitmap: u32) -> Outcome {
        let Some(e) = self.current_entry().cloned() else { return Outcome::default() };
        if e.animated() {
            return Outcome::default(); // full resolution x N frames explodes memory
        }
        let max = max_bitmap.min(MAX_EDGE);
        let native = e.native_w.max(e.native_h);
        let cap = if e.vector { VECTOR_MAX_EDGE } else { native };
        let needed = needed.min(cap).min(max);
        let have = e.width.max(e.height);
        if have + 1 >= needed {
            return Outcome::default();
        }
        let bucket = if !e.vector && needed * 10 >= native * 7 && native <= max { FULL } else { engine::bucket_for(needed) };
        if bucket <= e.key.bucket {
            return Outcome::default();
        }
        let key = Key::new(&e.key.path, e.key.stamp, bucket);
        if let Some(better) = self.cache.get(&key).cloned() {
            self.shown = Some(better);
            return Outcome { redraw: true, ..Default::default() };
        }
        self.request(key, Duration::ZERO);
        Outcome::default()
    }

    // --- Results ------------------------------------------------------------------------------------

    pub fn on_results(&mut self, gfx: Option<&Gfx>) -> Outcome {
        let mut out = Outcome::default();
        for done in self.pool.take_results() {
            self.pending.remove(&done.key);
            let is_current = self.current.as_ref().is_some_and(|(p, s)| *p == done.key.path && *s == done.key.stamp);
            match done.result {
                Ok(img) => {
                    if !is_current && !self.window.contains(&done.key) {
                        continue; // left the window while decoding: not worth a GPU upload
                    }
                    let Some(g) = gfx else { continue };
                    let Some(entry) = upload(g, done.key.clone(), img) else { continue };
                    let entry = Rc::new(entry);
                    self.cache.insert(done.key.clone(), entry.clone(), entry.bytes());
                    if is_current {
                        out.merge(self.consider(entry));
                    }
                }
                Err(e) => {
                    if is_current && self.current_entry().is_none() && !self.has_pending_for_current() {
                        self.error = Some(e.clone());
                        self.shown = None;
                        out.redraw = true;
                    }
                    self.failed.insert((done.key.path, done.key.stamp), e);
                }
            }
        }
        out
    }

    fn has_pending_for_current(&self) -> bool {
        let Some((p, s)) = &self.current else { return false };
        self.pending.iter().any(|k| &k.path == p && k.stamp == *s) || self.deferred.iter().any(|d| &d.0.path == p && d.0.stamp == *s)
    }

    /// A decode of the current image landed: show it if it beats what is up.
    fn consider(&mut self, entry: Rc<Entry>) -> Outcome {
        let mut out = Outcome { redraw: true, ..Default::default() };
        match self.current_entry() {
            None => {
                out.reset_view = Some((entry.native_w, entry.native_h));
                self.error = None;
                self.shown = Some(entry);
                self.restart_animation();
            }
            Some(s) if entry.key.bucket > s.key.bucket || (entry.animated() && !s.animated()) => {
                let became_animated = entry.animated() && !s.animated();
                self.shown = Some(entry);
                if became_animated {
                    self.restart_animation();
                }
            }
            _ => out.redraw = false,
        }
        out
    }

    // --- Animation ----------------------------------------------------------------------------------

    fn restart_animation(&mut self) {
        self.anim_frame = 0;
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_ANIM);
        }
        self.arm_animation();
    }

    fn arm_animation(&self) {
        let Some(e) = self.current_entry() else { return };
        let frames = e.frames.borrow();
        if frames.len() > 1 {
            let delay = frames[self.anim_frame % frames.len()].1.max(10);
            unsafe {
                SetTimer(Some(self.hwnd), TIMER_ANIM, delay, None);
            }
        }
    }

    pub fn next_frame(&mut self) -> bool {
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_ANIM);
        }
        let Some(n) = self.current_entry().map(|e| e.frames.borrow().len()) else { return false };
        if n < 2 {
            return false;
        }
        self.anim_frame = (self.anim_frame + 1) % n;
        self.arm_animation();
        true
    }

    // --- Device switch ------------------------------------------------------------------------------

    /// Re-makes every bitmap on the new device (WARP to hardware). Entries that can't be are dropped and the
    /// current image is requested again.
    pub fn reupload(&mut self, gfx: &Gfx) {
        let mut lost = Vec::new();
        let mut entries: Vec<Rc<Entry>> = self.cache.values().cloned().collect();
        if let Some(s) = &self.shown {
            entries.push(s.clone());
        }
        for e in entries {
            let Some(all) = e.pixels.borrow_mut().take() else { continue };
            let remade: Option<Vec<ID2D1Bitmap1>> = all.iter().map(|px| gfx.bitmap(e.width, e.height, px).ok()).collect();
            match remade {
                Some(bmps) => {
                    for (slot, bmp) in e.frames.borrow_mut().iter_mut().zip(bmps) {
                        slot.0 = bmp;
                    }
                }
                None => lost.push(e.key.clone()),
            }
        }
        for k in lost {
            self.cache.remove(&k);
            if self.shown.as_ref().is_some_and(|s| s.key == k) {
                self.shown = None;
            }
        }
        if let Some((p, s)) = self.current.clone() {
            if self.current_entry().is_none() {
                self.request(Key::new(&p, s, self.sharp), Duration::ZERO);
            }
        }
    }
}

fn upload(g: &Gfx, key: Key, img: Decoded) -> Option<Entry> {
    let mut frames = Vec::with_capacity(img.frames.len());
    for f in &img.frames {
        frames.push((g.bitmap(img.width, img.height, &f.pixels).ok()?, f.delay_ms));
    }
    let pixels = g.is_warp().then(|| img.frames.into_iter().map(|f| f.pixels).collect());
    Some(Entry {
        key,
        frames: RefCell::new(frames),
        width: img.width,
        height: img.height,
        native_w: img.native_width,
        native_h: img.native_height,
        format: img.format,
        taken: img.taken,
        pages: img.pages,
        vector: img.vector,
        pixels: RefCell::new(pixels),
    })
}
