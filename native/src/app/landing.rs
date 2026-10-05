//! The landing page (Controls/EmptyState), shown when nothing is open: the wordmark, "Open a file" and
//! "Open a folder", and the recently shown photos with thumbnails (click to open, right-click to remove,
//! Clear for all). The block sits a little above centre (2 : 3) and scrolls when the window is too short.
//! Recent entries whose files are gone are dropped; checking runs off the UI thread, since a sleeping drive
//! or a cloud placeholder can stall it.

use std::os::windows::fs::MetadataExt;

use windows::Win32::Graphics::Imaging::{CLSID_WICImagingFactory2, IWICImagingFactory};

use super::*;

pub(super) const WM_RECENTS: u32 = WM_APP + 10;
static WORDMARK: &[u8] = include_bytes!("../../assets/Brand/looker_wordmark.svg");
const WORDMARK_H: f32 = 52.0;
const COLUMN_W: f32 = 480.0;
const ROW_H: f32 = 42.0;
const THUMB_W: f32 = 48.0;
const THUMB_H: f32 = 36.0;
const SCROLL_STEP: f32 = 48.0;
const FILE_ATTRIBUTE_RECALL: u32 = 0x1000 | 0x40000 | 0x400000;

#[derive(Clone)]
pub(super) struct Recent {
    path: PathBuf,
    name: String,
    folder: String,
    stamp: u64,
    cloud: bool,
}

pub(super) struct Landing {
    /// The recent files that still exist (None: not checked yet).
    entries: Option<Vec<Recent>>,
    checking: bool,
    scroll: f32,
    max_scroll: f32,
    /// The wordmark rasterized for (dpi, bitmap, width px, height px).
    wordmark: Option<(f32, ID2D1Bitmap1)>,
}

impl Landing {
    pub fn new() -> Landing {
        Landing { entries: None, checking: false, scroll: 0.0, max_scroll: 0.0, wordmark: None }
    }

    /// Check the list again next time the page draws.
    pub fn stale(&mut self) {
        self.entries = None;
    }

    pub fn device_lost(&mut self) {
        self.wordmark = None;
    }
}

impl App {
    fn check_recents(&mut self) {
        if self.landing.checking {
            return;
        }
        self.landing.checking = true;
        let paths = self.settings.recents.clone();
        let hwnd = self.hwnd.0 as isize;
        std::thread::Builder::new()
            .name("recents".into())
            .spawn(move || {
                let list: Vec<Recent> = paths
                    .into_iter()
                    .filter_map(|p| {
                        let md = std::fs::metadata(&p).ok().filter(|m| m.is_file())?;
                        Some(Recent {
                            name: file_name(&p),
                            folder: p.parent().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default(),
                            stamp: md.last_write_time(),
                            cloud: md.file_attributes() & FILE_ATTRIBUTE_RECALL != 0,
                            path: p,
                        })
                    })
                    .collect();
                let ptr = Box::into_raw(Box::new(list));
                unsafe {
                    if PostMessageW(Some(HWND(hwnd as _)), WM_RECENTS, WPARAM(0), LPARAM(ptr as isize)).is_err() {
                        drop(Box::from_raw(ptr));
                    }
                }
            })
            .ok();
    }

    pub(super) fn on_recents(&mut self, list: Vec<Recent>) {
        self.landing.checking = false;
        self.landing.entries = Some(list);
        self.invalidate();
    }

    pub(super) fn open_recent(&mut self, i: usize) {
        if let Some(r) = self.landing.entries.as_ref().and_then(|e| e.get(i)).cloned() {
            self.open(r.path);
        }
    }

    pub(super) fn clear_recents(&mut self) {
        self.settings.recents.clear();
        settings::save(&self.settings);
        self.landing.entries = Some(Vec::new());
        self.landing.scroll = 0.0;
        self.invalidate();
    }

    pub(super) fn remove_recent(&mut self, path: &Path) {
        self.settings.remove_recent(path);
        settings::save(&self.settings);
        if let Some(e) = &mut self.landing.entries {
            e.retain(|r| r.path != path);
        }
        self.invalidate();
    }

    pub(super) fn recent_path(&self, i: usize) -> Option<PathBuf> {
        self.landing.entries.as_ref()?.get(i).map(|r| r.path.clone())
    }

    pub(super) fn scroll_landing(&mut self, delta: f32) {
        let l = &mut self.landing;
        l.scroll = (l.scroll - delta / 120.0 * SCROLL_STEP).clamp(0.0, l.max_scroll);
        self.invalidate();
    }

    pub(super) fn landing_wordmark(&mut self, g: &Gfx) -> Option<ID2D1Bitmap1> {
        if let Some((dpi, bmp)) = &self.landing.wordmark {
            if *dpi == g.dpi {
                return Some(bmp.clone());
            }
        }
        let s = g.dpi / 96.0;
        let h = (WORDMARK_H * s).round() as u32;
        let f: IWICImagingFactory = unsafe { CoCreateInstance(&CLSID_WICImagingFactory2, None, CLSCTX_INPROC_SERVER).ok()? };
        let d = crate::imaging::svg::render(&f, WORDMARK, h * 8, h).ok()?;
        let bmp = g.bitmap(d.width, d.height, &d.frames[0].pixels).ok()?;
        self.landing.wordmark = Some((g.dpi, bmp.clone()));
        Some(bmp)
    }

    /// "Set Looker as your default photo viewer" and an X that hides it for good.
    fn draw_default_hint(&mut self, g: &Gfx, cx: f32, y: f32) {
        let label = "Set Looker as your default photo viewer";
        let bw = (g.measure(&wide(label), &g.fonts.body) + 24.0).ceil();
        let total = bw + 6.0 + 32.0;
        let b = rect(g.snap(cx - total / 2.0), g.snap(y), bw, 32.0);
        let x = rect(b.right + 6.0, b.top, 32.0, 32.0);
        self.hits.add(Hit::DefaultHint, b);
        let st = self.state(Hit::DefaultHint, true);
        let fg = ui::button_frame(g, b, ui::Kind::Standard, &st);
        g.text(&wide(label), &g.fonts.body, b, fg, Align::Center);
        self.hits.add(Hit::DefaultHintClose, x);
        let st = self.state(Hit::DefaultHintClose, true);
        let fg = ui::button_frame(g, x, ui::Kind::Standard, &st);
        g.text(&[0xE711], &g.fonts.small_icons, x, fg, Align::Center);
    }

    pub(super) fn draw_landing(&mut self, g: &Gfx) {
        if self.landing.entries.is_none() {
            self.check_recents();
        }
        let v = self.viewport();
        let entries = self.landing.entries.clone().unwrap_or_default();
        let has_recent = !entries.is_empty();

        // Measure the block, then place it 2 : 3 between the spare space above and below.
        let mut content = 16.0 + WORDMARK_H + 8.0 + 16.0 + 8.0 + 32.0 + 16.0;
        if has_recent {
            content += 16.0 + 16.0 + 28.0 + 4.0 + entries.len() as f32 * ROW_H;
        }
        // The default-app hint sits on the bottom edge, under the scrolling block.
        let hint = crate::store::packaged() && !self.settings.default_hint_dismissed;
        let hint_h = if hint { 8.0 + 32.0 + 28.0 } else { 0.0 };
        let height = (v.bottom - v.top - hint_h).max(0.0);
        self.landing.max_scroll = (content - height).max(0.0);
        self.landing.scroll = self.landing.scroll.min(self.landing.max_scroll);
        let mut y = v.top + ((height - content).max(0.0) * 2.0 / 5.0).round() + 16.0 - self.landing.scroll;
        let cx = (v.left + v.right) / 2.0;
        if hint {
            self.draw_default_hint(g, cx, v.bottom - 28.0 - 32.0);
        }
        g.push_clip(D2D_RECT_F { bottom: v.bottom - hint_h, ..v });

        if let Some(bmp) = self.landing_wordmark(g) {
            let size = unsafe { bmp.GetPixelSize() };
            let w = WORDMARK_H * size.width as f32 / size.height.max(1) as f32;
            g.draw_bitmap(&bmp, rect(g.snap(cx - w / 2.0), g.snap(y), w, WORDMARK_H), 1.0);
        }
        y += WORDMARK_H + 8.0 + 16.0 + 8.0;

        for (id, label, x) in [(Hit::Open, "Open a file", cx - 6.0 - 140.0), (Hit::LandingFolder, "Open a folder", cx + 6.0)] {
            let r = rect(g.snap(x), g.snap(y), 140.0, 32.0);
            self.hits.add(id, r);
            let st = self.state(id, true);
            let fg = ui::button_frame(g, r, ui::Kind::Standard, &st);
            g.text(&wide(label), &g.fonts.body, r, fg, Align::Center);
        }
        y += 32.0;

        if has_recent {
            y += 16.0 + 16.0;
            let col = rect(g.snap(cx - COLUMN_W / 2.0), 0.0, COLUMN_W, 0.0);
            g.text(&wide("Recent"), &g.fonts.body_strong, rect(col.left + 8.0, y, 200.0, 24.0), white(TEXT_SECONDARY), Align::Left);
            let cw = g.measure(&wide("Clear"), &g.fonts.caption) + 20.0;
            let clear = rect(col.right - 8.0 - cw, y, cw, 24.0);
            self.hits.add(Hit::RecentClear, clear);
            let st = self.state(Hit::RecentClear, true);
            let fg = ui::button_frame(g, clear, ui::Kind::Standard, &st);
            g.text(&wide("Clear"), &g.fonts.caption, clear, fg, Align::Center);
            y += 28.0 + 4.0;

            let want = ((THUMB_W * self.scale()).ceil() as u32).max(96);
            for (i, e) in entries.iter().enumerate() {
                let row = rect(col.left, g.snap(y), COLUMN_W, ROW_H);
                y += ROW_H;
                if row.bottom < v.top || row.top > v.bottom {
                    continue;
                }
                let id = Hit::RecentRow(i);
                self.hits.add(id, row);
                // Brighter than a subtle button's states: the rows are large and sit on the plain window fill.
                let st = self.state_out(id, true);
                let fill = if st.pressed { white(0x2E) } else { gfx::rgba(0xFFFFFF, 0x1C as f32 / 255.0 * st.hover) };
                if fill.a > 0.0 {
                    g.fill_round(row, 4.0, fill);
                }
                let tb = rect(row.left + 6.0, row.top + 3.0, THUMB_W, THUMB_H);
                g.fill_round(tb, 4.0, white(0x15)); // ControlFillColorSecondary
                if let Some(t) = self.thumbs.get(&e.path, e.stamp) {
                    t.draw_fit(g, tb);
                }
                self.thumbs.want(&e.path, e.stamp, want, e.cloud, true);
                let tx = tb.right + 12.0;
                let tw = (row.right - 6.0 - tx).max(0.0);
                g.text(&wide(&e.name), &g.fonts.body, rect(tx, row.top + 2.0, tw, 20.0), white(0xFF), Align::Left);
                g.text(&wide(&e.folder), &g.fonts.caption, rect(tx, row.top + 22.0, tw, 16.0), white(TEXT_TERTIARY), Align::Left);
            }
        }
        g.pop_clip();
    }
}
