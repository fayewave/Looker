//! The file explorer, after VEIL Transient's file tabs: a card at the left of the viewer (the info card's
//! twin), or, in browse mode, most of the window with the photo as a preview at its right.
//!
//! Its parts, top to bottom: the breadcrumb row (Back, Forward, Up, a History drop-down, the crumbs headed by
//! "This PC", and buttons for the sidebar, the grouping, the bookmark and browse mode), column headers that
//! sort (Name, Date modified, Size; Dimensions and Type as they fit), the list, and a filter box with the
//! folder's count. Once the card is wide enough a sidebar runs down its left: Bookmarks (opening into trees),
//! Collections (favourites and colour tags), History (folders listed lately) and Places (the user's folders
//! and the drives, also trees); a narrower card shows it in place of the list behind a toggle.
//!
//! Click an image to open it; Ctrl-click and Shift-click select several; double-click (or Enter) a folder to
//! list it. Up/Down walk the rows, opening files on the way. Dragging rows onto a folder (a row, a crumb, the
//! sidebar) moves them (Ctrl copies), and Ctrl+C / Ctrl+X / Ctrl+V work with Explorer's clipboard; Ctrl+Z
//! undoes (explorer_ops.rs). The listed folder is watched, so it stays live. An 8 px grip on the right border
//! resizes the card (260–640, double-click resets).

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::Sender;
use std::time::Instant;

use super::*;
use crate::explorer::{self, ColKind, Collection, Group, History, Kind, Place, Row, SideSection, Watcher};
use crate::motion::Ease;
use crate::settings::{EXPLORER_MIN, INFO_MAX};
use crate::textedit::TextEdit;
use crate::ui::TextField;

pub(super) const MARGIN: f32 = 12.0;
const SCROLL_MS: f32 = 150.0;
const SCROLL_STEP: f32 = 52.0;
const MIN_IMAGE_W: f32 = 480.0;
/// The explorer edge's sticky change between the card and browse mode: how far a snap overshoots, and how long a
/// bounce swings.
const BOUNCE: f32 = 14.0;
const SPRING_SECS: f32 = 0.62;
/// Browse mode coming in or out and the edge gliding to its place: slower than the app's 190 ms, so the explorer
/// moves with some weight (Faye, 2026-11-10).
pub(super) const EDGE_SECS: f32 = 0.30;

/// A pull of `d` DIPs off the sticky line as the edge shows it: it gives less the further it's pulled, about 23 DIPs
/// at the zone's end (iOS's rubber band: (1 - 1 / (d c / k + 1)) k).
fn rubber(d: f32) -> f32 {
    let (c, k) = (0.55, 40.0);
    (1.0 - 1.0 / (d.max(0.0) * c / k + 1.0)) * k
}

/// Motion is on (Windows' "Animation effects").
fn motion_on() -> bool {
    crate::motion::enabled()
}

/// Browse mode's share of the window by default.
const BROWSE_SHARE: f32 = 0.5;
/// Dragging browse mode's edge leaves the photo at least this much.
const PREVIEW_DRAGGED_MIN: f32 = 200.0;
/// Changes on disk often come in bursts (a copy writes, renames, sets times): re-list once they settle.
pub(super) const REFRESH_MS: u32 = 150;
/// Moving this far with a row pressed starts dragging it.
const DRAG_START: f32 = 5.0;

pub(super) struct Listed {
    pub serial: usize,
    pub place: Place,
    pub rows: Vec<Row>,
    /// A re-list of the same place (the folder changed on disk): keep the scroll.
    pub refresh: bool,
}

/// What a folder's subfolders were listed for.
pub(super) enum SubFor {
    /// A node of the sidebar's trees, by its key.
    Tree(String),
    /// The crumb popup's listing, by its serial and the folder.
    Popup(usize, PathBuf),
    /// Computers found on the network, by the search's generation; `done` when that way of looking finished.
    Network(u64, bool),
}

pub(super) struct Subfolders {
    pub of: SubFor,
    pub children: Vec<PathBuf>,
    /// The files Looker can open (the popup lists those after the folders).
    pub files: Vec<PathBuf>,
}

/// Transient's crumb popup: under a crumb, the folder's path in a box (selected, to type over), and its
/// folders and photos below. Typing a path lists the folder it names, what's typed after its last separator
/// filtering it; Up / Down move, Enter goes, Tab steps into the folder, Escape closes.
pub(super) struct CrumbPopup {
    pub field: TextField,
    /// The crumb it hangs from.
    pub anchor: D2D_RECT_F,
    /// The folder listed in `entries` (None: This PC, the drives), and the listing on its way.
    pub listed: Option<PathBuf>,
    pub serial: usize,
    /// Each entry and whether it's a folder.
    pub entries: Vec<(PathBuf, bool)>,
    /// The entries the typed text lets through.
    pub shown: Vec<usize>,
    pub cursor: Option<usize>,
    pub scroll: f32,
    pub max_scroll: f32,
    /// When it opened: it fades in growing from 94 % about its top-left corner, as menus do.
    pub opened: Instant,
    /// The crumb it's for (shown outlined while it's open).
    pub crumb: Option<usize>,
}

/// A text box of the explorer's with the keyboard.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum EField {
    Filter,
    Rename,
    Path,
}

/// Rows being dragged with the mouse.
pub(super) struct RowDrag {
    pub start: (f32, f32),
    pub paths: Vec<PathBuf>,
    /// Past `DRAG_START`: it's a drag now, not a click.
    pub active: bool,
    /// The folder a release would drop into, and the rect of what stands for it (a row, a tab, a crumb).
    pub target: Option<PathBuf>,
    pub target_rect: Option<D2D_RECT_F>,
    /// When it became a drag: the card fades in from then.
    pub opened: Instant,
}

/// The key the sidebar's Network computers are kept under in `Explorer::children`.
pub(super) const NET_KEY: &str = "net:";

/// What a sidebar row leads to.
#[derive(Clone, PartialEq, Debug)]
pub(super) enum SideTarget {
    Header(SideSection),
    Hint,
    Place(Place),
}

#[derive(Clone, Debug)]
pub(super) struct SideItem {
    pub depth: u8,
    pub glyph: u16,
    pub tint: Option<u32>,
    pub label: String,
    pub target: SideTarget,
    /// Its key in the trees (section and path), when it can open.
    pub node: Option<String>,
    pub expanded: bool,
}

/// What a lazy column knows about a file, as of its write time `stamp`.
#[derive(Clone, Debug, Default)]
pub(super) struct Meta {
    pub stamp: u64,
    pub dims: Option<(u32, u32)>,
    pub taken: Option<crate::metadata::ExifTime>,
    pub camera: Option<String>,
    pub lens: Option<String>,
    /// A video's length (badges on its thumbnail).
    pub duration_us: Option<i64>,
    /// The other columns' text (camera settings, codecs, tags...), as `meta_fields` reads them.
    pub fields: Vec<(ColKind, String)>,
}

impl Meta {
    pub fn field(&self, c: ColKind) -> Option<&str> {
        self.fields.iter().find(|(k, _)| *k == c).map(|(_, v)| v.as_str())
    }
}

/// A number to sort a column's text by: "1/250 sec" as its fraction, else the first number in it ("f/2.8",
/// "48 kHz", "12.5 Mbit/s", "3/12").
pub(super) fn sort_number(c: ColKind, t: &str) -> Option<f64> {
    if c == ColKind::Exposure {
        if let Some(rest) = t.strip_prefix("1/") {
            let d: f64 = rest.split_whitespace().next()?.parse().ok()?;
            return (d > 0.0).then(|| 1.0 / d);
        }
    }
    let start = t.find(|ch: char| ch.is_ascii_digit() || ch == '-' || ch == '+')?;
    let num: String = t[start..].chars().take_while(|ch| ch.is_ascii_digit() || *ch == '.' || *ch == '-' || *ch == '+').collect();
    num.parse().ok()
}

/// The text of the columns past the basics, from what the meta thread read.
fn photo_fields(x: &crate::metadata::Exif) -> Vec<(ColKind, String)> {
    let mut f = Vec::new();
    for (c, v) in [
        (ColKind::Make, &x.make),
        (ColKind::Aperture, &x.aperture),
        (ColKind::Exposure, &x.exposure),
        (ColKind::Iso, &x.iso),
        (ColKind::FocalLength, &x.focal),
        (ColKind::ExposureBias, &x.bias),
        (ColKind::Flash, &x.flash),
        (ColKind::Metering, &x.metering),
        (ColKind::WhiteBalance, &x.white_balance),
        (ColKind::Altitude, &x.altitude),
    ] {
        if let Some(v) = v {
            f.push((c, v.clone()));
        }
    }
    if let (Some(lat), Some(lon)) = (x.latitude, x.longitude) {
        f.push((ColKind::Gps, format!("{lat:.5}, {lon:.5}")));
    }
    f
}

fn video_fields(i: &crate::imaging::video::Info) -> Vec<(ColKind, String)> {
    let mut f = Vec::new();
    for (label, v) in crate::imaging::video::facts(i) {
        let c = match label {
            "Codec" => ColKind::VideoCodec,
            "Frame rate" => ColKind::FrameRate,
            "Format" => ColKind::VideoFormat,
            "Colour" => ColKind::Colour,
            "Bit rate" => ColKind::BitRate,
            // "AAC, stereo, 48 kHz": the codec, its channels, its rate.
            "Audio" => {
                let mut parts = v.split(", ");
                for c in [ColKind::AudioCodec, ColKind::Channels, ColKind::SampleRate] {
                    if let Some(p) = parts.next() {
                        f.push((c, p.to_string()));
                    }
                }
                continue;
            }
            _ => continue,
        };
        f.push((c, v));
    }
    f
}

fn audio_fields(path: &std::path::Path) -> (Option<i64>, Vec<(ColKind, String)>) {
    let Ok((facts, tags)) = crate::imaging::audio::probe(path) else { return (None, Vec::new()) };
    let duration_us = Some(facts.duration_us).filter(|&d| d > 0);
    let (a, t) = crate::imaging::audio::facts(&facts, &tags);
    let mut f = Vec::new();
    for (label, v) in a.into_iter().chain(t) {
        let c = match label {
            "Codec" => ColKind::AudioCodec,
            "Sample rate" => ColKind::SampleRate,
            "Channels" => ColKind::Channels,
            "Bit depth" => ColKind::BitDepth,
            "Bit rate" => ColKind::BitRate,
            "Title" => ColKind::Title,
            "Artist" => ColKind::Artist,
            "Album" => ColKind::Album,
            "Album artist" => ColKind::AlbumArtist,
            "Year" => ColKind::Year,
            "Genre" => ColKind::Genre,
            "Track" => ColKind::Track,
            "Composer" => ColKind::Composer,
            "BPM" => ColKind::Bpm,
            "Key" => ColKind::Key,
            _ => continue,
        };
        f.push((c, v));
    }
    (duration_us, f)
}

/// A tab that isn't in front: where it is and how it was left (the one in front lives in `Explorer` itself).
#[derive(Default)]
pub(super) struct TabState {
    pub id: u64,
    pub place: Option<Place>,
    pub history: History,
    pub cursor: Option<PathBuf>,
    pub anchor: Option<PathBuf>,
    pub selected: HashSet<String>,
    pub scroll: f32,
    pub filter: String,
    pub xsort: Option<(ColKind, bool)>,
}

/// A tab dragged along the strip, as Transient's: it follows the pointer, the others ease aside around an
/// outlined slot.
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

/// A sidebar section dragged by its heading, as Transient's: the heading follows the pointer, the others make
/// room, an outlined slot shows where it lands.
pub(super) struct SectionDrag {
    pub sec: SideSection,
    pub start_y: f32,
    pub y: f32,
    /// The pointer's offset from the heading's top when pressed.
    pub grab: f32,
    pub active: bool,
    /// Where it would land among the other sections (worked out as the sidebar draws).
    pub insert: usize,
}

/// A rectangle dragged over the list to select the rows it crosses, as Transient's and Windows' details view
/// (Faye, 2026-11-10): from where it was pressed (in the list's own coordinates, so it scrolls with it) to the
/// pointer; with Ctrl the rows it crosses join the selection there was.
pub(super) struct Marquee {
    /// The press, x in DIPs and y down the list's content (scroll included).
    pub start: (f32, f32),
    pub pointer: (f32, f32),
    pub active: bool,
    pub add: bool,
    pub before: Vec<PathBuf>,
}

/// A bookmark or a collection dragged up or down its section (Faye, 2026-11-10), as a section is by its heading:
/// the row (with its open tree) follows the pointer, the others ease aside round an outlined slot.
pub(super) struct EntryDrag {
    pub sec: SideSection,
    /// Its place among the section's entries when pressed.
    pub from: usize,
    pub start_y: f32,
    pub y: f32,
    /// The pointer's offset from the row's top when pressed.
    pub grab: f32,
    pub active: bool,
    /// Where it would land among the others (worked out as the sidebar draws).
    pub insert: usize,
}

/// A column header dragged along the headers (its cells come with it).
pub(super) struct HeaderDrag {
    pub idx: usize,
    pub start_x: f32,
    pub x: f32,
    /// Where in the header the pointer took hold of it.
    pub grab: f32,
    pub active: bool,
    /// Where it would land: before the other shown column at this position (worked out as it's drawn).
    pub insert: usize,
}

/// A column edge being dragged: which column's right edge (its place among the shown ones), where the
/// press was, and every shown column's index and width then.
pub(super) struct ColResize {
    pub pos: usize,
    pub x0: f32,
    pub widths: Vec<(usize, ColKind, f32)>,
}

pub(super) struct Explorer {
    pub(super) place: Option<Place>,
    pub(super) rows: Vec<Row>,
    /// The rows the filter lets through, as indices into `rows`. Hits and the cursor work in these.
    pub(super) view: Vec<usize>,
    serial: usize,
    pub(super) history: History,
    /// The open photo's folder the card last followed.
    photo_folder: Option<PathBuf>,
    /// The keyboard cursor, and where a Shift-range starts.
    pub(super) cursor: Option<PathBuf>,
    pub(super) anchor: Option<PathBuf>,
    /// The selection, by `explorer::norm` keys.
    pub(super) selected: HashSet<String>,
    /// After going up, the cursor lands on the folder we came from.
    came_from: Option<PathBuf>,
    /// Bring this view row into view on the next frame.
    pub(super) reveal: Option<usize>,
    pub(super) scroll: f32,
    pub(super) anim: Option<(f32, f32, Instant)>,
    pub(super) max_scroll: f32,
    watcher: Option<Watcher>,
    /// The resize grip's drag.
    pub drag: Option<(f32, f32)>,
    /// The crumbs collapsed into "…" on the last frame.
    pub(super) hidden_crumbs: Vec<Place>,
    /// The last click went into the explorer: its keys (Ctrl+C, Del, F2…) act on the rows.
    pub focused: bool,
    pub(super) field: Option<EField>,
    pub(super) filter: TextField,
    pub(super) rename: Option<(PathBuf, TextField)>,
    pub(super) path_edit: Option<CrumbPopup>,
    /// A folder just made: its rename opens once it's listed.
    pub(super) pending_rename: Option<PathBuf>,
    pub row_drag: Option<RowDrag>,
    /// Rows cut to the clipboard (shown faded while it still holds them), and its sequence number then.
    pub(super) cut: Option<(HashSet<String>, u32)>,
    /// A narrow card shows the sidebar in place of the list.
    pub(super) sidebar_open: bool,
    pub(super) side_scroll: f32,
    /// The sidebar's scroll easing (from, to, since), as the list's `anim`.
    pub(super) side_anim: Option<(f32, f32, Instant)>,
    pub(super) side_max: f32,
    /// The trees: listed children by node key, open nodes, and nodes being listed.
    pub(super) children: HashMap<String, Vec<PathBuf>>,
    pub(super) expanded: HashSet<String>,
    tree_pending: HashSet<String>,
    /// What the lazy columns show, read from the files themselves (size, EXIF), by norm key.
    pub(super) meta: HashMap<String, Meta>,
    meta_pending: HashSet<String>,
    meta_tx: Option<Sender<Vec<(PathBuf, u64)>>>,
    /// The list sorted by a column the toolbar's sort doesn't have (Type, Date taken…), and descending.
    pub(super) xsort: Option<(ColKind, bool)>,
    /// A column header being dragged to a new place.
    pub(super) header_drag: Option<HeaderDrag>,
    pub(super) col_resize: Option<ColResize>,
    /// Where each column is drawn, eased while columns move (a drag, a menu's Shift or fit).
    pub(super) col_pos: HashMap<ColKind, Ease<2>>,
    pub(super) col_anim_until: Instant,
    /// The shown columns as last laid out, in order: index, kind, width.
    pub(super) col_shown: Vec<(usize, ColKind, f32)>,
    /// The scrollbar being dragged: the pointer's offset from the thumb's top.
    pub(super) thumb_drag: Option<f32>,
    /// How the place showing is shown: its saved view, else the default (`Settings::default_view`).
    pub(super) look: explorer::View,
    /// A sidebar section being dragged by its heading, each section's eased top, and how long they ease
    /// after a drop.
    pub(super) section_drag: Option<SectionDrag>,
    pub(super) section_pos: HashMap<SideSection, Ease<1>>,
    pub(super) section_anim_until: Instant,
    /// A bookmark or collection being dragged into another place, the entries' eased tops in their section (by
    /// key), and until when they ease after a drop.
    pub(super) side_row_drag: Option<EntryDrag>,
    /// The selection rectangle being dragged, and where the list was drawn last (its rect, row height and Name
    /// column's span), for it.
    pub(super) marquee: Option<Marquee>,
    pub(super) list_geom: Option<(D2D_RECT_F, f32, (f32, f32))>,
    /// The hovered row's cells whose text is cut off (their rect and full text), the cell the pointer was last over,
    /// and the tabs whose titles are cut off: for full-text tooltips (Faye, 2026-11-10).
    pub(super) cell_tips: Vec<(D2D_RECT_F, String)>,
    pub(super) tip_cell: Option<usize>,
    pub(super) tab_tips: Vec<(u8, String)>,
    pub(super) row_pos: HashMap<String, Ease<1>>,
    pub(super) row_anim_until: Instant,
    /// How open each section is (1 open, 0 folded to its heading), eased.
    pub(super) section_open: HashMap<SideSection, Ease<1>>,
    /// The box over the folder a drag would drop into: eased from one target to the next, its opacity, and the
    /// card's height (taller once it says what a drop does).
    pub(super) drop_box: Option<Ease<4>>,
    pub(super) drop_shade: Ease<1>,
    pub(super) drop_card_h: Ease<1>,
    /// The tabs, left to right; the one at `active_tab` is a stub (its state is the fields above).
    pub(super) tabs: Vec<TabState>,
    pub(super) active_tab: usize,
    next_tab_id: u64,
    tabs_loaded: bool,
    pub(super) tab_drag: Option<TabDrag>,
    /// Each tab's eased left edge (by id), and how long they ease after a drop, a new tab or a close.
    pub(super) tab_pos: HashMap<u64, Ease<1>>,
    pub(super) tab_anim_until: Instant,
    /// The strip's tab width as last laid out (they jump, not ease, when it changes).
    pub(super) tab_pitch: f32,
    /// The tabs' width, eased when a tab opening or closing changes it (narrow strips squeeze them).
    pub(super) tab_w: Ease<1>,
    /// The sidebar's edge being dragged: where the press was, and its width then.
    pub(super) side_drag: Option<(f32, f32)>,
    /// 0 the card, 1 browse mode, eased between.
    pub(super) browse: Ease<1>,
    /// Browse mode was on when the explorer was closed: it opens in it again.
    pub(super) reopen_browse: bool,
    /// The edge pulled into its sticky zone (DIPs, + wider, - narrower), live while it's dragged; and the bounce
    /// it springs back with (from how far, since when).
    pub(super) stretch: f32,
    pub(super) spring: Option<(f32, Instant)>,
    /// The edge is held on the sticky line (the card's widest) until pulled `STICK` off it; and which side of the line
    /// the pointer was on last (for when it crosses).
    /// Held on a sticky line: the card's widest (false) or browse mode's default (true).
    pub(super) latched: Option<bool>,
    /// Popping across to that line after a jump, not following the pointer until it gets there.
    pub(super) coast: Option<bool>,
    pub(super) last_side: f32,
    /// The card's width and the sidebar's easing back to their defaults (an edge double-clicked).
    pub(super) width_settle: Option<crate::motion::Settle>,
    pub(super) side_settle: Option<crate::motion::Settle>,
    /// What's been typed to jump to a row, and when the last character came (`explorer_type_ahead`).
    pub(super) ahead: (String, Option<Instant>),
    pub(super) tags: crate::tags::Tags,
    /// The tags' version the listed collection was listed at.
    tags_listed: u64,
    /// The sidebar's Places (the user's folders, the drives), read once and again on F5.
    pub(super) places: Option<(Vec<(String, PathBuf, u16)>, Vec<Row>)>,
    /// The sidebar's Network: the drives mapped to shares (read with Places). Its computers, found off the UI
    /// thread, are `children[NET_KEY]`.
    pub(super) net_drives: Vec<Row>,
    /// The network search's generation (F5 starts another) and how many of its two ways are still looking.
    net_gen: u64,
    pub(super) net_searching: u8,
    /// Undo and redo for file operations (explorer_ops.rs).
    pub(super) undo: Vec<super::explorer_ops::Step>,
    pub(super) redo: Vec<super::explorer_ops::Step>,
}

impl Explorer {
    pub fn new() -> Explorer {
        Explorer {
            place: None,
            rows: Vec::new(),
            view: Vec::new(),
            serial: 0,
            history: History::default(),
            photo_folder: None,
            cursor: None,
            anchor: None,
            selected: HashSet::new(),
            came_from: None,
            reveal: None,
            scroll: 0.0,
            anim: None,
            max_scroll: 0.0,
            watcher: None,
            drag: None,
            hidden_crumbs: Vec::new(),
            focused: false,
            field: None,
            filter: TextField::new(TextEdit::new("")),
            rename: None,
            path_edit: None,
            pending_rename: None,
            row_drag: None,
            cut: None,
            sidebar_open: false,
            side_scroll: 0.0,
            side_anim: None,
            side_max: 0.0,
            children: HashMap::new(),
            expanded: HashSet::new(),
            tree_pending: HashSet::new(),
            meta: HashMap::new(),
            meta_pending: HashSet::new(),
            meta_tx: None,
            xsort: None,
            header_drag: None,
            col_resize: None,
            col_pos: HashMap::new(),
            col_anim_until: Instant::now(),
            col_shown: Vec::new(),
            thumb_drag: None,
            look: explorer::View::default(),
            section_drag: None,
            section_pos: HashMap::new(),
            section_anim_until: Instant::now(),
            side_row_drag: None,
            marquee: None,
            list_geom: None,
            cell_tips: Vec::new(),
            tip_cell: None,
            tab_tips: Vec::new(),
            row_pos: HashMap::new(),
            row_anim_until: Instant::now(),
            section_open: HashMap::new(),
            drop_box: None,
            drop_shade: Ease::at([0.0]),
            drop_card_h: Ease::at([30.0]),
            tabs: vec![TabState::default()],
            active_tab: 0,
            next_tab_id: 1,
            tabs_loaded: false,
            tab_drag: None,
            tab_pos: HashMap::new(),
            tab_anim_until: Instant::now(),
            tab_pitch: 0.0,
            tab_w: Ease::at([0.0]),
            side_drag: None,
            browse: Ease::at([0.0]).over(EDGE_SECS),
            reopen_browse: false,
            stretch: 0.0,
            spring: None,
            latched: None,
            coast: None,
            last_side: 0.0,
            width_settle: None,
            side_settle: None,
            ahead: (String::new(), None),
            tags: crate::tags::Tags::load(),
            tags_listed: 0,
            places: None,
            net_drives: Vec::new(),
            net_gen: 0,
            net_searching: 0,
            undo: Vec::new(),
            redo: Vec::new(),
        }
    }

    pub(super) fn scroll_now(&mut self) -> (f32, bool) {
        let Some((from, to, t0)) = self.anim else { return (self.scroll, false) };
        let t = (t0.elapsed().as_secs_f32() * 1000.0 / SCROLL_MS).min(1.0);
        if t >= 1.0 || !crate::motion::enabled() {
            self.anim = None;
            self.scroll = to;
            return (to, false);
        }
        (from + (to - from) * crate::motion::ease_out(t), true)
    }

    /// The sidebar's scroll this frame, and whether it's still easing.
    pub(super) fn side_scroll_now(&mut self) -> (f32, bool) {
        let Some((from, to, t0)) = self.side_anim else { return (self.side_scroll, false) };
        let t = (t0.elapsed().as_secs_f32() * 1000.0 / SCROLL_MS).min(1.0);
        if t >= 1.0 || !crate::motion::enabled() {
            self.side_anim = None;
            self.side_scroll = to;
            return (to, false);
        }
        (from + (to - from) * crate::motion::ease_out(t), true)
    }

    /// The view row of `path`.
    pub(super) fn index_of(&self, path: &Path) -> Option<usize> {
        self.view.iter().position(|&i| explorer::same_path(&self.rows[i].path, path))
    }

    /// The row at view index `vi`.
    pub(super) fn row(&self, vi: usize) -> Option<&Row> {
        self.view.get(vi).and_then(|&i| self.rows.get(i))
    }

    pub(super) fn is_selected(&self, path: &Path) -> bool {
        self.selected.contains(&explorer::norm(path))
    }

    pub(super) fn is_cut(&self, path: &Path) -> bool {
        self.cut.as_ref().is_some_and(|(set, seq)| !crate::clipboard::changed_since(*seq) && set.contains(&explorer::norm(path)))
    }

    /// What the file actions act on: the selection, else the cursor's row.
    pub(super) fn targets(&self) -> Vec<PathBuf> {
        let sel: Vec<PathBuf> = self.view.iter().map(|&i| &self.rows[i]).filter(|r| self.is_selected(&r.path)).map(|r| r.path.clone()).collect();
        if !sel.is_empty() {
            return sel;
        }
        self.cursor.iter().filter(|c| self.index_of(c).is_some()).cloned().collect()
    }

    /// The filter's words changed (or the rows did): which rows show.
    pub(super) fn refilter(&mut self) {
        let words = explorer::filter_words(&self.filter.edit.text());
        self.view = (0..self.rows.len()).filter(|&i| explorer::matches(&self.rows[i], &words)).collect();
        self.max_scroll = 0.0;
        self.reveal = self.cursor.as_deref().and_then(|c| self.index_of(c));
    }

    pub fn resort(&mut self, sort: crate::folder::Sort, group: Group) {
        explorer::sort_rows(&mut self.rows, sort, group);
        self.sort_local(group);
        self.refilter();
    }

    /// The column sort the toolbar's doesn't have, over the toolbar's order (which breaks its ties): rows
    /// whose value isn't known (yet) go last either way; folders stay grouped.
    fn sort_local(&mut self, group: Group) {
        let Some((c, desc)) = self.xsort else { return };
        let keys: Vec<SortKey> = self.rows.iter().map(|r| sort_key(r, c, self.meta.get(&explorer::norm(&r.path)))).collect();
        let mut order: Vec<usize> = (0..self.rows.len()).collect();
        let folder_rank = |r: &Row| match (group, r.is_folder()) {
            (Group::FoldersFirst, true) | (Group::FilesFirst, false) => 0,
            (Group::Mixed, _) => 0,
            _ => 1,
        };
        order.sort_by(|&a, &b| {
            let (ra, rb) = (&self.rows[a], &self.rows[b]);
            folder_rank(ra).cmp(&folder_rank(rb)).then_with(|| {
                let (ka, kb) = (&keys[a], &keys[b]);
                match (ka, kb) {
                    (SortKey::None, SortKey::None) => std::cmp::Ordering::Equal,
                    (SortKey::None, _) => std::cmp::Ordering::Greater,
                    (_, SortKey::None) => std::cmp::Ordering::Less,
                    _ => {
                        let o = ka.cmp(kb);
                        if desc { o.reverse() } else { o }
                    }
                }
            })
        });
        let rows = std::mem::take(&mut self.rows);
        let mut slots: Vec<Option<Row>> = rows.into_iter().map(Some).collect();
        self.rows = order.into_iter().filter_map(|i| slots[i].take()).collect();
    }

    /// The folder the explorer is listing, if it's a folder (not This PC or a collection).
    pub(super) fn folder(&self) -> Option<PathBuf> {
        self.place.as_ref().and_then(|p| p.folder()).map(Path::to_path_buf)
    }
}

impl App {
    pub(super) fn explorer_shown(&self) -> bool {
        self.settings.explorer_visible && (self.panels_allowed() || self.home_explorer())
    }

    /// The home page takes the explorer too, pushing its block aside (Faye, 2026-11-10); not in fullscreen.
    pub(super) fn home_explorer(&self) -> bool {
        self.viewer.current.is_none() && !self.slideshow.running && self.fullscreen.is_none()
    }

    /// The card's own width (not browsing): as its edge was dragged, else a quarter of the window.
    pub(super) fn card_width(&self) -> f32 {
        self.settings.explorer_width.max(self.card_min())
    }

    /// The card's default width, a quarter of the window, is also its narrowest (Faye, 2026-11-10); never past the
    /// sticky line, so a small window still has room for the photo.
    pub(super) fn card_min(&self) -> f32 {
        let (w, _) = self.size_dip();
        (w * 0.25).round().clamp(EXPLORER_MIN, INFO_MAX).min(self.sticky_line())
    }

    /// The sticky line: the card's widest, where browse mode begins.
    pub(super) fn sticky_line(&self) -> f32 {
        let (w, _) = self.size_dip();
        INFO_MAX.min((w - MIN_IMAGE_W).max(EXPLORER_MIN))
    }

    /// The card's width now: its own, eased out to browse mode's while that's on, stretched or bouncing at the
    /// sticky change between them.
    pub(super) fn explorer_width(&self) -> f32 {
        let card = self.card_width();
        let k = self.explorer.browse.value(Instant::now())[0];
        let w = if k <= 0.0 { card } else { card + (self.browse_width() - card) * k };
        let w = w + self.explorer_stretch();
        // Double-clicked back to its default: it eases there.
        match self.explorer.width_settle {
            Some(s) if s.moving() => s.value(w),
            _ => w,
        }
    }

    /// The sidebar's width, easing back to its default after a double-click on its edge.
    pub(super) fn sidebar_w(&self) -> f32 {
        let w = self.settings.sidebar_width;
        match self.explorer.side_settle {
            Some(s) if s.moving() => s.value(w),
            _ => w,
        }
    }

    /// How far the edge is pulled or bouncing past where it rests: the live rubber band while it's dragged in the
    /// sticky zone, else the damped bounce after a snap or a let-go (two small swings, gone in under half a second).
    pub(super) fn explorer_stretch(&self) -> f32 {
        let e = &self.explorer;
        if e.stretch != 0.0 {
            return e.stretch;
        }
        match e.spring {
            Some((from, at)) if motion_on() => {
                let t = at.elapsed().as_secs_f32();
                if t >= SPRING_SECS {
                    0.0
                } else {
                    from * (-t / 0.15).exp() * (std::f32::consts::TAU * t / 0.34).cos()
                }
            }
            _ => 0.0,
        }
    }

    /// The bounce is still swinging.
    fn spring_moving(&self) -> bool {
        self.explorer.spring.is_some_and(|(_, at)| motion_on() && at.elapsed().as_secs_f32() < SPRING_SECS)
    }

    /// Browse mode's width: half the window (Faye, 2026-11-10), and never less than the card and the sidebar side
    /// by side, so the list keeps at least its width beside the sidebar; while its edge is dragged in browse mode,
    /// where it's dragged. The photo keeps at least 200 DIPs.
    fn browse_width(&self) -> f32 {
        let (w, _) = self.size_dip();
        let side = self.settings.sidebar_width.clamp(super::explorer_view::SIDEBAR_MIN, super::explorer_view::SIDEBAR_MAX);
        let least = self.card_width() + side + 1.0;
        let wide = if self.settings.browse_share > 0.0 { w * self.settings.browse_share } else { (w * BROWSE_SHARE).max(least) };
        wide.min(w - PREVIEW_DRAGGED_MIN).max(self.card_width())
    }

    pub(super) fn browsing(&self) -> bool {
        self.explorer.browse.target()[0] > 0.5
    }

    /// True while browse mode eases in or out (the layout follows each frame).
    pub(super) fn browse_moving(&mut self) -> bool {
        let now = Instant::now();
        let springing = self.spring_moving();
        if !springing && self.explorer.spring.is_some() && self.explorer.stretch == 0.0 {
            self.explorer.spring = None; // settled: one last layout at rest
            return true;
        }
        let settling = self.explorer.width_settle.is_some_and(|s| s.moving());
        if !settling && self.explorer.width_settle.take().is_some() {
            return true; // one last layout at its default
        }
        self.explorer.browse.moving(now) | self.explorer.browse.settle(now) | springing | settling
    }

    pub(super) fn left_inset(&self) -> f32 {
        self.explorer_width() * self.slides.explorer.value()
    }

    /// Slid out to the left by however much of it is hidden.
    pub(super) fn explorer_rect(&self) -> D2D_RECT_F {
        let v = self.viewport();
        let w = self.explorer_width();
        let dx = w * (1.0 - self.slides.explorer.value());
        // Pulled or pushed at the sticky line (and bouncing off it), the whole card goes with it a little, not just
        // its edge (Faye, 2026-11-10): its left side moves a third of the stretch.
        let pull = self.explorer_stretch() * 0.33;
        D2D_RECT_F { left: v.left + MARGIN - dx + pull, top: v.top + MARGIN, right: v.left + w - MARGIN - dx, bottom: v.bottom - MARGIN }
    }

    pub(super) fn toggle_explorer(&mut self) {
        if self.viewer.current.is_none() && !self.home_explorer() {
            return;
        }
        self.settings.explorer_visible = !self.settings.explorer_visible;
        settings::save(&self.settings);
        if !self.settings.explorer_visible {
            self.explorer.watcher = None;
            self.explorer.focused = false;
            self.explorer_blur_field();
            // Opened again, it comes back as it was: browse mode (at its width) or the card (Faye, 2026-11-10).
            self.explorer.reopen_browse = self.browsing();
            self.explorer.browse.jump([0.0]);
        } else {
            if std::mem::take(&mut self.explorer.reopen_browse) {
                self.explorer.browse.jump([1.0]);
                self.explorer.focused = true;
            }
            // Where it was left, scrolled as it was (it used to go back to the photo's folder, losing the place):
            // it follows the photo only if the photo has moved to another folder since.
            let before = self.explorer.place.clone();
            self.explorer_follow();
            // On the home page, with no photo to follow: the folder last listed, else This PC.
            if self.explorer.place.is_none() {
                let start = self.settings.visited.first().cloned().map_or(Place::Computer, Place::Folder);
                self.explorer_go(start, true);
            }
            let stayed = match (&before, &self.explorer.place) {
                (Some(a), Some(b)) => a.same(b),
                _ => false,
            };
            if stayed {
                self.relist(); // what changed while it was shut, and the watcher again
            }
        }
        self.slide_panels();
    }

    /// B: browse mode, the explorer across most of the window with the photo as a preview; B again (or
    /// opening an image from it) goes back to the card.
    pub(super) fn toggle_browse(&mut self) {
        // The home page too (Faye, 2026-11-11: its browse button did nothing).
        if self.viewer.current.is_none() && !self.home_explorer() {
            return;
        }
        let on = !self.browsing();
        if on {
            // Always the card's width plus the sidebar's, not where its edge was last dragged.
            self.settings.browse_share = 0.0;
        } else {
            // And off, the card at its default width (Faye, 2026-11-10), not where it was last dragged.
            self.settings.explorer_width = 0.0;
            settings::save(&self.settings);
        }
        if on && !self.settings.explorer_visible {
            self.toggle_explorer();
        }
        self.explorer.browse.set([on as u8 as f32], Instant::now());
        self.explorer.focused = on;
        self.browse_toast(on);
        self.layout_changed();
    }

    /// Says browse mode came on or went off (Faye, 2026-11-10): the button, Ctrl+B, or the edge dragged past the line.
    fn browse_toast(&mut self, on: bool) {
        self.show_toast(if on { "Browse mode on" } else { "Browse mode off" }, false);
    }

    pub(super) fn leave_browse(&mut self) {
        if self.browsing() {
            self.explorer.browse.set([0.0], Instant::now());
            self.settings.explorer_width = 0.0;
            settings::save(&self.settings);
            self.layout_changed();
        }
    }

    /// After every navigation: moving into another folder lists it; within one folder the listing stays where
    /// the user took it, and the photo's row is highlighted (and scrolled to) when it is listed.
    pub(super) fn explorer_follow(&mut self) {
        if !self.explorer_shown() {
            return;
        }
        let Some(file) = self.current_path().map(Path::to_path_buf) else { return };
        let Some(folder) = file.parent().map(Path::to_path_buf) else { return };
        let moved = self.explorer.photo_folder.as_deref().is_none_or(|f| !explorer::same_path(f, &folder));
        if moved {
            self.explorer.photo_folder = Some(folder.clone());
            let here = Place::Folder(folder);
            // A collection being browsed isn't left for the photo's folder.
            let in_collection = matches!(self.explorer.place, Some(Place::Collection(_)));
            if !in_collection && self.explorer.place.as_ref().is_none_or(|p| !p.same(&here)) {
                self.explorer_go(here, true);
                return;
            }
        }
        if let Some(i) = self.explorer.index_of(&file) {
            let e = &mut self.explorer;
            if !e.is_selected(&file) {
                e.selected.clear();
                e.selected.insert(explorer::norm(&file));
            }
            e.cursor = Some(file.clone());
            e.anchor = Some(file);
            e.reveal = Some(i);
        }
        self.invalidate();
    }

    /// Lists a place (a new one goes into the history unless Back/Forward are walking it).
    pub(super) fn explorer_go(&mut self, place: Place, record: bool) {
        self.load_tabs();
        let e = &mut self.explorer;
        // Going up: the cursor will land on the folder we came out of.
        e.came_from = match (&e.place, &place) {
            (Some(Place::Folder(cur)), Place::Folder(to)) => cur.ancestors().find(|a| a.parent().is_some_and(|p| explorer::same_path(p, to))).map(Path::to_path_buf),
            (Some(Place::Folder(cur)), Place::Computer) => cur.ancestors().last().map(Path::to_path_buf),
            _ => None,
        };
        if record {
            e.history.record(&place);
        }
        e.watcher = None;
        e.rows.clear();
        e.view.clear();
        e.selected.clear();
        e.scroll = 0.0;
        e.anim = None;
        e.rename = None;
        e.path_edit = None;
        if e.field != Some(EField::Filter) {
            e.field = None;
        }
        // A new folder starts unfiltered (Transient clears the filter on opening a folder).
        e.filter.edit.clear();
        e.place = Some(place.clone());
        if let Place::Folder(f) = &place {
            if self.settings.push_visited(f) {
                settings::save(&self.settings);
            }
        }
        let v = self.view_for(&place);
        self.adopt_view(v);
        self.explorer_list(place, false);
        self.save_tabs();
        self.invalidate();
    }

    pub(super) fn explorer_list(&mut self, place: Place, refresh: bool) {
        self.explorer.serial += 1;
        let serial = self.explorer.serial;
        let sort = self.settings.sort;
        let group = self.explorer.look.group;
        let deep = self.explorer.look.subfolders && matches!(place, Place::Folder(_));
        let tagged = match &place {
            Place::Collection(c) => self.explorer.tags.in_collection(*c),
            _ => Vec::new(),
        };
        self.explorer.tags_listed = self.explorer.tags.version;
        let hwnd = self.hwnd.0 as isize;
        std::thread::Builder::new()
            .name("explorer".into())
            .spawn(move || {
                let rows = explorer::list(&place, sort, group, &tagged, deep);
                let ptr = Box::into_raw(Box::new(Listed { serial, place, rows, refresh }));
                unsafe {
                    if PostMessageW(Some(HWND(hwnd as _)), explorer::WM_EXPLORER_LISTED, WPARAM(0), LPARAM(ptr as isize)).is_err() {
                        drop(Box::from_raw(ptr));
                    }
                }
            })
            .ok();
    }

    pub(super) fn on_explorer_listed(&mut self, l: Listed) {
        if l.serial != self.explorer.serial {
            return; // superseded
        }
        // The covering file was renamed or deleted: the photo underneath comes back.
        if self.placeholder.as_ref().is_some_and(|p| !p.path.exists()) {
            self.placeholder = None;
            self.current_changed();
        }
        let mut rows = l.rows;
        if !self.explorer.look.other_files {
            rows.retain(|r| r.kind != Kind::Other);
        }
        let e = &mut self.explorer;
        e.rows = rows;
        // Selected rows that went (deleted, moved away) drop out of the selection.
        let present: HashSet<String> = e.rows.iter().map(|r| explorer::norm(&r.path)).collect();
        e.selected.retain(|k| present.contains(k));
        e.refilter();
        if e.watcher.is_none() {
            if let Place::Folder(f) = &l.place {
                e.watcher = Watcher::start(f, self.hwnd, explorer::WM_EXPLORER_CHANGED, e.look.subfolders);
            }
        }
        e.place = Some(l.place);
        if let Some(p) = e.pending_rename.take() {
            if let Some(i) = e.index_of(&p) {
                e.cursor = Some(p.clone());
                e.selected = [explorer::norm(&p)].into();
                e.reveal = Some(i);
                self.explorer_begin_rename(p);
                return;
            }
            e.pending_rename = Some(p);
        }
        let e = &mut self.explorer;
        if !l.refresh {
            // The open photo when it is listed here, else (after going up) the folder we came from.
            let came_from = e.came_from.take();
            let current = self.viewer.current.as_ref().map(|(p, _)| p.clone());
            let target = [current, came_from].into_iter().flatten().find(|c| e.index_of(c).is_some());
            e.reveal = target.as_deref().and_then(|t| e.index_of(t));
            if let Some(t) = &target {
                e.selected = [explorer::norm(t)].into();
            }
            e.anchor = target.clone();
            e.cursor = target;
        } else if let Some(c) = e.cursor.clone() {
            if e.index_of(&c).is_none() {
                e.cursor = None;
            }
        }
        self.invalidate();
    }

    /// The listed folder changed on disk: re-list it once the burst settles. (A late event from a folder
    /// already left only re-lists the current one, which is harmless.)
    pub(super) fn on_explorer_changed(&mut self) {
        if self.explorer.watcher.is_some() {
            unsafe {
                SetTimer(Some(self.hwnd), TIMER_EXPLORER, REFRESH_MS, None);
            }
        }
    }

    pub(super) fn explorer_refresh(&mut self) {
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_EXPLORER);
        }
        if let Some(p) = self.explorer.place.clone() {
            self.explorer_list(p, true);
        }
        // The open trees list their folders again too, and the drives are read again.
        self.explorer.places = None;
        self.explorer.children.remove(NET_KEY);
        let open: Vec<String> = self.explorer.expanded.iter().cloned().collect();
        for k in open {
            self.explorer.children.remove(&k);
        }
    }

    /// A collection on show lists again when the tags change.
    pub(super) fn explorer_tags_changed(&mut self) {
        if matches!(self.explorer.place, Some(Place::Collection(_))) && self.explorer.tags_listed != self.explorer.tags.version {
            if let Some(p) = self.explorer.place.clone() {
                self.explorer_list(p, true);
            }
        }
        self.invalidate();
    }

    pub(super) fn explorer_back(&mut self) {
        if let Some(p) = self.explorer.history.back() {
            self.explorer_go(p, false);
        }
    }

    pub(super) fn explorer_forward(&mut self) {
        if let Some(p) = self.explorer.history.forward() {
            self.explorer_go(p, false);
        }
    }

    pub(super) fn explorer_up(&mut self) {
        if let Some(p) = self.explorer.place.as_ref().and_then(Place::parent) {
            self.explorer_go(p, true);
        }
    }

    pub(super) fn explorer_history_jump(&mut self, i: usize) {
        if let Some(p) = self.explorer.history.jump(i) {
            self.explorer_go(p, false);
        }
    }

    pub(super) fn explorer_crumb(&mut self, place: Place) {
        if self.explorer.place.as_ref().is_none_or(|p| !p.same(&place)) {
            self.explorer_go(place, true);
        }
    }

    /// Opens a file from the card: an index jump when it is in the open folder, else a full open.
    pub(super) fn open_file(&mut self, path: PathBuf) {
        let at = self.viewer.listing.as_ref().and_then(|l| l.images.iter().position(|e| explorer::same_path(&e.path, &path)));
        // Back onto the image the placeholder covered: no navigation happens, so announce it again.
        let reshow = self.placeholder.take().is_some();
        match at {
            Some(i) if Some(i) == self.viewer.index => {
                if reshow {
                    self.current_changed();
                }
            }
            Some(i) => self.go_to(i),
            None => self.open(path),
        }
    }

    /// The explorer landed on a file Looker can't open: a file glyph and its name cover the photo, and the
    /// title, status row and counter describe that file. The photo stays current (←/→ go on from it); any
    /// navigation takes the cover down.
    pub(super) fn show_placeholder(&mut self, path: PathBuf) {
        let md = std::fs::metadata(&path).ok();
        let stamp = md.as_ref().map_or(0, |m| m.last_write_time());
        // Online-only (Dropbox/OneDrive placeholder): only the shell's cached thumbnail, never a download.
        let cloud = md.as_ref().is_some_and(|m| m.file_attributes() & (0x1000 | 0x40000 | 0x400000) != 0);
        let modified = md.as_ref().map(|m| {
            let t = m.last_write_time();
            FILETIME { dwLowDateTime: t as u32, dwHighDateTime: (t >> 32) as u32 }
        });
        unsafe {
            let _ = SetWindowTextW(self.hwnd, &HSTRING::from(format!("{} - Looker", file_name(&path))));
        }
        // The explorer's cursor follows it, as it follows the photo (`explorer_follow`).
        if let Some(i) = self.explorer.index_of(&path) {
            self.explorer.cursor = Some(path.clone());
            self.explorer.reveal = Some(i);
        }
        self.placeholder = Some(Placeholder { size: md.map(|m| m.len()), modified, path, stamp, cloud });
        self.invalidate();
    }

    /// The status row for the covering file: "PS1 file · 2.1 KB · Modified …" (no size in pixels, no zoom).
    pub(super) fn placeholder_status(&self) -> Option<String> {
        let p = self.placeholder.as_ref()?;
        let ext = p.path.extension().map(|e| e.to_string_lossy().to_uppercase()).unwrap_or_default();
        let mut parts = vec![if ext.is_empty() { "File".to_string() } else { format!("{ext} file") }];
        if let Some(s) = p.size {
            parts.push(format_bytes(s));
        }
        if let Some(t) = p.modified {
            parts.push(format!("Modified {}", format_time(t)));
        }
        Some(parts.join("   ·   "))
    }

    // --- The view (the Folders and files menu) -------------------------------------------------------

    /// The saved view covering the folder showing (its own, or a parent's "and subfolders"), by index in
    /// `Settings::views`.
    pub(super) fn view_entry(&self) -> Option<usize> {
        self.explorer.folder().and_then(|f| explorer::saved_for(&self.settings.views, &f))
    }

    fn view_for(&self, place: &Place) -> explorer::View {
        match place.folder().and_then(|f| explorer::saved_for(&self.settings.views, f)) {
            Some(i) => self.settings.views[i].view.clone(),
            None => self.settings.default_view(),
        }
    }

    /// Takes on a view for the place about to list (a column sort on a column it doesn't show goes).
    fn adopt_view(&mut self, v: explorer::View) {
        let e = &mut self.explorer;
        if e.look.columns != v.columns {
            if e.xsort.is_some_and(|(k, _)| v.columns.iter().all(|(c, _)| *c != k)) {
                e.xsort = None;
            }
            e.col_pos.clear();
        }
        if e.look.subfolders != v.subfolders {
            e.watcher = None;
        }
        e.look = v;
    }

    /// The view changed: it goes into the saved view covering the folder, else it's the new default.
    pub(super) fn store_view(&mut self) {
        let v = self.explorer.look.clone();
        match self.view_entry() {
            Some(i) => self.settings.views[i].view = v,
            None => self.settings.set_default_view(&v),
        }
        settings::save(&self.settings);
    }

    fn relist(&mut self) {
        if let Some(p) = self.explorer.place.clone() {
            self.explorer_list(p, true);
        }
    }

    /// Include subfolders: every file under the folder, listed flat.
    pub(super) fn toggle_subfolders(&mut self) {
        self.explorer.look.subfolders = !self.explorer.look.subfolders;
        self.explorer.watcher = None;
        self.store_view();
        self.relist();
    }

    pub(super) fn set_explorer_group(&mut self, g: Group) {
        if self.explorer.look.group != g {
            self.explorer.look.group = g;
            self.store_view();
            self.explorer.resort(self.settings.sort, g);
            self.invalidate();
        }
    }

    /// The rows' height, in rows (taller rows show bigger thumbnails); the cursor's row stays in view.
    pub(super) fn set_row_height(&mut self, h: u8) {
        let h = h.clamp(1, explorer::HEIGHT_MAX);
        if self.explorer.look.height != h {
            let old = self.explorer.look.height as f32;
            self.explorer.look.height = h;
            self.explorer.scroll = self.explorer.scroll * h as f32 / old;
            self.explorer.anim = None;
            self.explorer.reveal = self.explorer.cursor.as_deref().and_then(|c| self.explorer.index_of(c));
            self.store_view();
            self.invalidate();
        }
    }

    /// Saved view: None, This folder, or This folder and subfolders. Saving keeps the view as it is; None
    /// goes back to the default (or to a parent's, which an `Off` entry here then turns away).
    pub(super) fn set_saved_view(&mut self, scope: Option<explorer::Scope>) {
        let Some(f) = self.explorer.folder() else { return };
        let key = explorer::norm(&f);
        let v = self.explorer.look.clone();
        let views = &mut self.settings.views;
        let exact = views.iter().position(|s| explorer::norm(&s.path) == key);
        match scope {
            Some(sc) => match exact {
                Some(i) => {
                    views[i].scope = sc;
                    views[i].view = v;
                }
                None => views.push(explorer::SavedView { path: f.clone(), scope: sc, view: v }),
            },
            None => {
                if let Some(i) = exact {
                    views.remove(i);
                }
                if explorer::saved_for(views, &f).is_some() {
                    views.push(explorer::SavedView { path: f.clone(), scope: explorer::Scope::Off, view: explorer::View::default() });
                }
            }
        }
        settings::save(&self.settings);
        let place = Place::Folder(f);
        let nv = self.view_for(&place);
        if nv != self.explorer.look {
            let relist = nv.subfolders != self.explorer.look.subfolders || nv.other_files != self.explorer.look.other_files;
            let resort = nv.group != self.explorer.look.group;
            self.adopt_view(nv);
            self.animate_columns();
            if relist {
                self.relist();
            } else if resort {
                self.explorer.resort(self.settings.sort, self.explorer.look.group);
            }
        }
        self.show_toast(
            match scope {
                Some(explorer::Scope::Folder) => "View saved for this folder",
                Some(_) => "View saved for this folder and its subfolders",
                None => "Saved view removed",
            },
            false,
        );
        self.invalidate();
    }

    /// Set as default: the folder's saved view becomes the one every other folder shows.
    pub(super) fn set_default_view(&mut self) {
        let v = self.explorer.look.clone();
        self.settings.set_default_view(&v);
        settings::save(&self.settings);
        self.show_toast("Set as the default view", false);
    }

    /// A column header: the same column flips the direction, another sorts by it, ascending. The sort is the
    /// toolbar's, so the viewer's order (←/→, the strip) follows.
    pub(super) fn sort_by_column(&mut self, field: crate::folder::SortField) {
        let s = self.settings.sort;
        let to = if s.field == field { crate::folder::Sort { descending: !s.descending, ..s } } else { crate::folder::Sort { field, descending: false } };
        self.set_sort(to);
    }

    // --- Selection and opening ----------------------------------------------------------------------

    /// The cursor onto view row `vi`, selecting it alone; images open, other files cover the photo.
    fn explorer_pick(&mut self, vi: usize) {
        let Some(r) = self.explorer.row(vi).cloned() else { return };
        let e = &mut self.explorer;
        e.cursor = Some(r.path.clone());
        e.anchor = Some(r.path.clone());
        e.selected = [explorer::norm(&r.path)].into();
        e.reveal = Some(vi);
        match r.kind {
            Kind::Image => self.open_file(r.path),
            Kind::Other => self.show_placeholder(r.path),
            Kind::Drive | Kind::Folder => {}
        }
        self.invalidate();
    }

    /// A click on a row: alone it picks the row; Ctrl adds or removes it; Shift selects from the anchor to it.
    pub(super) fn explorer_click(&mut self, vi: usize) {
        let (ctrl, shift) = unsafe { (GetKeyState(VK_CONTROL.0 as i32) < 0, GetKeyState(VK_SHIFT.0 as i32) < 0) };
        self.explorer.focused = true;
        self.explorer_commit_fields();
        let Some(path) = self.explorer.row(vi).map(|r| r.path.clone()) else { return };
        if shift {
            self.explorer_select_range(vi, ctrl);
        } else if ctrl {
            let e = &mut self.explorer;
            let k = explorer::norm(&path);
            if !e.selected.remove(&k) {
                e.selected.insert(k);
            }
            e.cursor = Some(path.clone());
            e.anchor = Some(path);
            e.reveal = Some(vi);
            self.invalidate();
        } else {
            self.explorer_pick(vi);
        }
    }

    /// Selects from the anchor to view row `vi` (keeping what else was selected when `add`).
    fn explorer_select_range(&mut self, vi: usize, add: bool) {
        let e = &mut self.explorer;
        let a = e.anchor.as_deref().and_then(|a| e.index_of(a)).unwrap_or(vi);
        if !add {
            e.selected.clear();
        }
        for i in a.min(vi)..=a.max(vi) {
            if let Some(r) = e.row(i) {
                let k = explorer::norm(&r.path);
                e.selected.insert(k);
            }
        }
        e.cursor = e.row(vi).map(|r| r.path.clone());
        e.reveal = Some(vi);
        self.invalidate();
    }

    pub(super) fn explorer_select_all(&mut self) {
        let e = &mut self.explorer;
        e.selected = e.view.iter().map(|&i| explorer::norm(&e.rows[i].path)).collect();
        self.invalidate();
    }

    /// Double-click (or Enter): a folder lists; an image opens and, from browse mode, the viewer comes back.
    pub(super) fn explorer_double_click(&mut self, vi: usize) {
        let Some(r) = self.explorer.row(vi).cloned() else { return };
        if r.is_folder() {
            self.explorer_go(Place::Folder(r.path), true);
        } else {
            // Browse mode stays as it is (Faye, 2026-11-09: it used to drop back to the card).
            self.explorer_pick(vi);
        }
    }

    /// Up/Down: the cursor moves a row; a file under it opens. Shift extends the selection instead.
    pub(super) fn explorer_move(&mut self, delta: isize) {
        let n = self.explorer.view.len();
        if n == 0 {
            return;
        }
        let at = self.explorer.cursor.as_deref().and_then(|c| self.explorer.index_of(c));
        let i = match at {
            None => if delta > 0 { 0 } else { n - 1 },
            Some(i) => (i as isize + delta).clamp(0, n as isize - 1) as usize,
        };
        if unsafe { GetKeyState(VK_SHIFT.0 as i32) < 0 } {
            self.explorer_select_range(i, false);
        } else {
            self.explorer_pick(i);
        }
    }

    /// Page Up / Page Down (Home / End while the explorer has the keys): the top or bottom of the list.
    pub(super) fn explorer_jump(&mut self, last: bool) {
        let n = self.explorer.view.len();
        if n > 0 {
            self.explorer_pick(if last { n - 1 } else { 0 });
        }
    }

    /// A character typed while the explorer has the keys, as Windows' File Explorer takes them (Faye, 2026-11-10):
    /// the first row whose name starts with what's been typed in the last second; the same letter again steps to
    /// the next one starting with it. (The app's own keys all take Ctrl, so every letter is free for this.)
    pub(super) fn explorer_type_ahead(&mut self, c: char) {
        let now = Instant::now();
        let fresh = self.explorer.ahead.1.is_none_or(|t| now.duration_since(t) > std::time::Duration::from_millis(1000));
        let c = c.to_lowercase().collect::<String>();
        let prefix = if fresh { c.clone() } else { format!("{}{c}", self.explorer.ahead.0) };
        self.explorer.ahead = (prefix.clone(), Some(now));
        let n = self.explorer.view.len();
        if n == 0 {
            return;
        }
        // One letter over and over cycles through that letter; a longer prefix narrows from where the cursor is.
        let repeat = prefix.chars().count() > 1 && prefix.chars().all(|x| prefix.starts_with(x));
        let (find, step) = if repeat || fresh { (c, 1) } else { (prefix, 0) };
        let at = self.explorer.cursor.as_deref().and_then(|p| self.explorer.index_of(p));
        let start = at.map_or(0, |i| (i + step) % n);
        let hit = (0..n).map(|k| (start + k) % n).find(|&i| self.explorer.rows[self.explorer.view[i]].name.to_lowercase().starts_with(&find));
        if let Some(i) = hit {
            self.explorer_pick(i);
        }
    }

    /// Enter: list the folder under the cursor, or open its file. False when there's nothing to do.
    pub(super) fn explorer_enter(&mut self) -> bool {
        let Some(i) = self.explorer.cursor.as_deref().and_then(|c| self.explorer.index_of(c)) else { return false };
        self.explorer_double_click(i);
        true
    }

    /// Escape with the explorer's keys: a multiple selection goes first, then the explorer lets go of them.
    pub(super) fn explorer_escape(&mut self) -> bool {
        if self.explorer.row_drag.as_ref().is_some_and(|d| d.active) {
            self.explorer.row_drag = None;
            self.invalidate();
            return true;
        }
        if !self.explorer.focused {
            return false;
        }
        if self.explorer.selected.len() > 1 {
            let keep = self.explorer.cursor.clone();
            self.explorer.selected = keep.iter().map(|p| explorer::norm(p)).collect();
        } else if self.browsing() {
            self.leave_browse();
        } else {
            self.explorer.focused = false;
        }
        self.invalidate();
        true
    }

    /// "Open folder" from a folder's menu: open it in the viewer (its first image), as File > Open folder would.
    pub(super) fn open_folder(&mut self, folder: PathBuf) {
        self.pending_folder = Some(folder.clone());
        self.pending_cover = None;
        self.list_folder_async(folder);
    }

    pub(super) fn scroll_explorer(&mut self, delta: f32) {
        let e = &mut self.explorer;
        let (now, _) = e.scroll_now();
        let target = e.anim.map_or(e.scroll, |(_, to, _)| to);
        let to = (target - delta / 120.0 * SCROLL_STEP).clamp(0.0, e.max_scroll.max(0.0));
        e.scroll = now;
        e.anim = Some((now, to, Instant::now()));
        self.invalidate();
    }

    pub(super) fn scroll_sidebar(&mut self, delta: f32) {
        let e = &mut self.explorer;
        let (now, _) = e.side_scroll_now();
        let target = e.side_anim.map_or(e.side_scroll, |(_, to, _)| to);
        let to = (target - delta / 120.0 * SCROLL_STEP).clamp(0.0, e.side_max.max(0.0));
        e.side_scroll = now;
        e.side_anim = Some((now, to, Instant::now()));
        self.invalidate();
    }

    // --- Text boxes: the filter, an inline rename, the path ------------------------------------------

    /// The explorer's focused text box.
    pub(super) fn explorer_field_mut(&mut self) -> Option<&mut TextField> {
        let e = &mut self.explorer;
        match e.field? {
            EField::Filter => Some(&mut e.filter),
            EField::Rename => e.rename.as_mut().map(|(_, f)| f),
            EField::Path => e.path_edit.as_mut().map(|p| &mut p.field),
        }
    }

    /// The hit rect of the focused text box.
    pub(super) fn explorer_field_hit(&self) -> Option<Hit> {
        Some(match self.explorer.field? {
            EField::Filter => Hit::ExplorerFilter,
            EField::Rename => Hit::ExplorerRename,
            EField::Path => Hit::ExplorerPath,
        })
    }

    pub(super) fn explorer_focus_field(&mut self, f: EField) {
        // The other boxes end: a rename elsewhere commits, the path box closes (not the box taking the keys,
        // which may have just been opened).
        if f != EField::Rename && self.explorer.rename.is_some() {
            self.explorer_finish_rename(true);
        }
        if f != EField::Path && self.explorer.path_edit.take().is_some() {
            self.invalidate();
        }
        self.explorer.field = Some(f);
        self.explorer.focused = true;
        self.set_ime(true);
        self.restart_caret();
    }

    /// Ctrl+F: the filter takes the keys (the explorer comes out if it's hidden), its text selected.
    pub(super) fn explorer_find(&mut self) {
        if self.viewer.current.is_none() && !self.home_explorer() {
            return;
        }
        if !self.settings.explorer_visible {
            self.toggle_explorer();
        }
        self.explorer.filter.edit.select_all();
        self.explorer_focus_field(EField::Filter);
    }

    /// The text box lets go of the keys (a rename commits, the path edit closes).
    pub(super) fn explorer_blur_field(&mut self) {
        if self.explorer.field.take().is_some() {
            self.set_ime(false);
            unsafe {
                let _ = KillTimer(Some(self.hwnd), TIMER_CARET);
            }
            self.invalidate();
        }
    }

    /// A press anywhere else ends what's being typed: a rename commits, the path edit closes.
    pub(super) fn explorer_commit_fields(&mut self) {
        if self.explorer.rename.is_some() {
            self.explorer_finish_rename(true);
        }
        if self.explorer.path_edit.take().is_some() {
            self.invalidate();
        }
        if self.explorer.field.is_some() && self.explorer.field != Some(EField::Filter) {
            self.explorer.field = None;
        }
    }

    /// The filter's text changed: the rows it lets through.
    pub(super) fn explorer_filter_changed(&mut self) {
        self.explorer.refilter();
        self.explorer.scroll = 0.0;
        self.explorer.anim = None;
        self.invalidate();
    }

    /// Keys while one of the explorer's boxes has them. True when used.
    pub(super) fn explorer_field_key(&mut self, vk: VIRTUAL_KEY, shift: bool, ctrl: bool) -> bool {
        let Some(f) = self.explorer.field else { return false };
        match (f, vk) {
            (EField::Filter, VK_ESCAPE) => {
                if self.explorer.filter.edit.units().is_empty() {
                    self.explorer_blur_field();
                } else {
                    self.explorer.filter.edit.clear();
                    self.explorer_filter_changed();
                }
            }
            (EField::Filter, VK_RETURN) => {
                self.explorer_blur_field();
                if !self.explorer_enter() {
                    if let Some(vi) = (!self.explorer.view.is_empty()).then_some(0) {
                        self.explorer_double_click(vi);
                    }
                }
            }
            (EField::Filter, VK_UP) => self.explorer_move(-1),
            (EField::Filter, VK_DOWN) => self.explorer_move(1),
            (EField::Rename, VK_ESCAPE) => self.explorer_finish_rename(false),
            (EField::Rename, VK_RETURN) => self.explorer_finish_rename(true),
            (EField::Path, VK_ESCAPE) => self.close_popup(),
            (EField::Path, VK_RETURN) => self.popup_go(),
            (EField::Path, VK_UP) => self.popup_move(-1),
            (EField::Path, VK_DOWN) => self.popup_move(1),
            (EField::Path, VK_TAB) => self.popup_step_in(),
            _ => {
                let before = self.explorer_field_mut().map(|t| t.edit.text());
                let used = self.field_key(vk, shift, ctrl);
                if self.explorer_field_mut().map(|t| t.edit.text()) != before {
                    match f {
                        EField::Filter => self.explorer_filter_changed(),
                        EField::Path => self.popup_typed(),
                        EField::Rename => {}
                    }
                }
                return used;
            }
        }
        self.invalidate();
        true
    }

    /// A typed character for the explorer's box.
    pub(super) fn explorer_char(&mut self, unit: u16) {
        let field = self.explorer.field;
        let Some(t) = self.explorer_field_mut() else { return };
        t.edit.type_unit(unit);
        match field {
            Some(EField::Filter) => self.explorer_filter_changed(),
            Some(EField::Path) => self.popup_typed(),
            _ => {}
        }
        self.restart_caret();
    }

    /// Clicking the last crumb (or the empty end of the crumbs): the popup for the folder showing.
    pub(super) fn explorer_edit_path(&mut self) {
        let Some(anchor) = self.hits.rect_of(Hit::CrumbArea) else { return };
        let all = self.explorer.place.as_ref().map(explorer::crumbs).unwrap_or_default();
        let last = all.len().saturating_sub(1);
        let anchor = self.hits.rect_of(Hit::Crumb(last)).unwrap_or(anchor);
        let dir = self.explorer.folder();
        self.open_popup(dir, anchor);
        if let Some(pp) = &mut self.explorer.path_edit {
            pp.crumb = Some(last);
        }
    }

    /// The popup for crumb `i` (its chevron, a right-click on it): peeks into that folder without leaving
    /// the one showing.
    pub(super) fn crumb_popup(&mut self, i: usize, anchor: D2D_RECT_F) {
        let Some(place) = self.explorer.place.clone() else { return };
        match explorer::crumbs(&place).get(i) {
            Some(Place::Folder(f)) => self.open_popup(Some(f.clone()), anchor),
            Some(Place::Computer) => self.open_popup(None, anchor),
            _ => return,
        }
        if let Some(pp) = &mut self.explorer.path_edit {
            pp.crumb = Some(i);
        }
    }

    fn open_popup(&mut self, dir: Option<PathBuf>, anchor: D2D_RECT_F) {
        self.close_menu();
        let text = dir.as_ref().map(|d| format!("{}\\", d.to_string_lossy().trim_end_matches('\\'))).unwrap_or_default();
        let mut edit = TextEdit::new(&text);
        edit.select_all();
        self.explorer.path_edit =
            Some(CrumbPopup { field: TextField::new(edit), anchor, listed: None, serial: 0, entries: Vec::new(), shown: Vec::new(), cursor: None, scroll: 0.0, max_scroll: 0.0, opened: Instant::now(), crumb: None });
        self.popup_list(dir);
        self.explorer_focus_field(EField::Path);
    }

    pub(super) fn close_popup(&mut self) {
        if self.explorer.path_edit.take().is_some() {
            self.explorer_blur_field();
            self.invalidate();
        }
    }

    /// Lists a folder for the popup (This PC: the drives, at once).
    fn popup_list(&mut self, dir: Option<PathBuf>) {
        let Some(pp) = &mut self.explorer.path_edit else { return };
        pp.serial += 1;
        pp.listed = dir.clone();
        pp.entries.clear();
        pp.shown.clear();
        pp.cursor = None;
        pp.scroll = 0.0;
        match dir {
            None => {
                let drives = self.explorer.places.as_ref().map(|(_, d)| d.clone()).unwrap_or_else(explorer::drives);
                if let Some(pp) = &mut self.explorer.path_edit {
                    pp.entries = drives.into_iter().map(|d| (d.path, true)).collect();
                }
                self.popup_filter();
            }
            Some(d) => {
                let serial = pp.serial;
                let hwnd = self.hwnd.0 as isize;
                std::thread::Builder::new()
                    .name("popup".into())
                    .spawn(move || {
                        let (children, files) = explorer::entries(&d);
                        let ptr = Box::into_raw(Box::new(Subfolders { of: SubFor::Popup(serial, d), children, files }));
                        unsafe {
                            if PostMessageW(Some(HWND(hwnd as _)), explorer::WM_SUBFOLDERS, WPARAM(0), LPARAM(ptr as isize)).is_err() {
                                drop(Box::from_raw(ptr));
                            }
                        }
                    })
                    .ok();
            }
        }
        self.invalidate();
    }

    /// The typed text changed: the folder it names lists (if it isn't already), the rest filters it.
    fn popup_typed(&mut self) {
        let Some(pp) = &self.explorer.path_edit else { return };
        let (dir, _) = explorer::split_typed(&pp.field.edit.text());
        let want = (!dir.is_empty()).then(|| PathBuf::from(&dir));
        let same = match (&want, &pp.listed) {
            (Some(a), Some(b)) => explorer::same_path(a, b),
            (None, None) => true,
            _ => false,
        };
        if !same && want.as_ref().is_none_or(|d| d.is_dir()) {
            self.popup_list(want);
        } else {
            self.popup_filter();
        }
    }

    /// Which entries the text after the last separator lets through (all of them when nothing's typed); the
    /// cursor goes to the first match.
    fn popup_filter(&mut self) {
        let Some(pp) = &mut self.explorer.path_edit else { return };
        let (_, rest) = explorer::split_typed(&pp.field.edit.text());
        let rest = rest.to_lowercase();
        pp.shown = (0..pp.entries.len()).filter(|&i| rest.is_empty() || explorer::folder_name(&pp.entries[i].0).to_lowercase().contains(&rest)).collect();
        // Names that start with it first.
        if !rest.is_empty() {
            let entries = &pp.entries;
            pp.shown.sort_by_key(|&i| !explorer::folder_name(&entries[i].0).to_lowercase().starts_with(&rest));
        }
        pp.cursor = (!rest.is_empty() && !pp.shown.is_empty()).then_some(0);
        pp.scroll = 0.0;
        self.invalidate();
    }

    pub(super) fn popup_move(&mut self, d: isize) {
        let Some(pp) = &mut self.explorer.path_edit else { return };
        let n = pp.shown.len();
        if n == 0 {
            return;
        }
        let i = match pp.cursor {
            None => if d > 0 { 0 } else { n - 1 },
            Some(c) => (c as isize + d).clamp(0, n as isize - 1) as usize,
        };
        pp.cursor = Some(i);
        // Kept in view.
        let row = super::explorer_view::POPUP_ROW;
        let view = super::explorer_view::POPUP_ROWS as f32 * row;
        let (top, bottom) = (i as f32 * row, i as f32 * row + row);
        if top < pp.scroll {
            pp.scroll = top;
        } else if bottom > pp.scroll + view {
            pp.scroll = bottom - view;
        }
        self.invalidate();
    }

    /// Enter: the row under the cursor (a folder lists, a photo lists its folder with it picked), else the
    /// typed path.
    pub(super) fn popup_go(&mut self) {
        let pick = self.explorer.path_edit.as_ref().and_then(|pp| pp.cursor.and_then(|c| pp.shown.get(c)).map(|&i| pp.entries[i].clone()));
        match pick {
            Some((p, _)) => self.popup_open(p),
            None => self.explorer_go_typed_path(),
        }
    }

    /// A row of the popup was clicked.
    pub(super) fn popup_pick(&mut self, row: usize) {
        let pick = self.explorer.path_edit.as_ref().and_then(|pp| pp.shown.get(row).map(|&i| pp.entries[i].0.clone()));
        if let Some(p) = pick {
            self.popup_open(p);
        }
    }

    fn popup_open(&mut self, p: PathBuf) {
        self.close_popup();
        if p.is_dir() {
            self.explorer_go(Place::Folder(p), true);
        } else if let Some(dir) = p.parent().map(Path::to_path_buf) {
            self.explorer_go(Place::Folder(dir), true);
            self.explorer.came_from = Some(p.clone());
            if crate::format::is_supported(&p) {
                self.open_file(p);
            }
        }
    }

    /// Tab: into the folder under the cursor (its path typed in, listed).
    pub(super) fn popup_step_in(&mut self) {
        let pick = self.explorer.path_edit.as_ref().and_then(|pp| pp.cursor.and_then(|c| pp.shown.get(c)).map(|&i| pp.entries[i].clone()));
        if let Some((p, true)) = pick {
            self.popup_set_text(&format!("{}\\", p.to_string_lossy().trim_end_matches('\\')));
        }
    }

    /// The popup's Up: the listed folder's parent (from a drive, This PC).
    pub(super) fn popup_up(&mut self) {
        let Some(pp) = &self.explorer.path_edit else { return };
        let text = match pp.listed.as_ref().and_then(|d| d.parent()) {
            Some(parent) => format!("{}\\", parent.to_string_lossy().trim_end_matches('\\')),
            None => String::new(),
        };
        self.popup_set_text(&text);
    }

    fn popup_set_text(&mut self, text: &str) {
        if let Some(pp) = &mut self.explorer.path_edit {
            let mut edit = TextEdit::new(text);
            let n = edit.units().len();
            edit.select(n, n);
            pp.field = TextField::new(edit);
        }
        self.popup_typed();
        self.explorer.field = Some(EField::Path);
        self.restart_caret();
    }

    pub(super) fn popup_clear(&mut self) {
        self.popup_set_text("");
    }

    pub(super) fn scroll_popup(&mut self, delta: f32) {
        if let Some(pp) = &mut self.explorer.path_edit {
            pp.scroll = (pp.scroll - delta / 120.0 * 75.0).clamp(0.0, pp.max_scroll);
            self.invalidate();
        }
    }

    /// Enter with no row under the cursor: a folder lists; a file lists its folder with it selected;
    /// "This PC" (or nothing) the drives.
    fn explorer_go_typed_path(&mut self) {
        let text = self.explorer.path_edit.as_ref().map(|p| p.field.edit.text()).unwrap_or_default();
        let text = text.trim().trim_matches('"');
        let expanded = if let Some(rest) = text.strip_prefix('~') {
            std::env::var("USERPROFILE").map(|h| format!("{h}{rest}")).unwrap_or_else(|_| text.to_string())
        } else {
            text.to_string()
        };
        let p = PathBuf::from(&expanded);
        if p.exists() {
            self.popup_open(p);
        } else if text.eq_ignore_ascii_case(explorer::COMPUTER_NAME) || text.is_empty() {
            self.close_popup();
            self.explorer_go(Place::Computer, true);
        } else {
            self.show_toast("Couldn't find that folder", false);
        }
    }

    // --- Sidebar ------------------------------------------------------------------------------------

    /// The sidebar's rows this frame: section headings, then their places, open tree nodes' children
    /// indented under them.
    pub(super) fn sidebar_items(&mut self) -> Vec<SideItem> {
        let mut out = Vec::new();
        let heading = |sec: SideSection, label: &str, glyph: u16| SideItem { depth: 0, glyph, tint: None, label: label.into(), target: SideTarget::Header(sec), node: None, expanded: false };
        for sec in self.settings.sidebar_order.clone() {
            match sec {
                SideSection::Bookmarks => {
                    out.push(heading(sec, "Bookmarks", 0xE840));
                    if self.settings.bookmarks.is_empty() {
                        out.push(SideItem { depth: 1, glyph: 0, tint: None, label: "Pin folders with the bookmark button".into(), target: SideTarget::Hint, node: None, expanded: false });
                    }
                    let bookmarks = self.settings.bookmarks.clone();
                    for b in bookmarks {
                        self.push_tree(&mut out, "b", &b, explorer::folder_name(&b), 0xE8B7, 1);
                    }
                }
                SideSection::Collections => {
                    out.push(heading(sec, "Collections", 0xE734));
                    // In the order they were dragged into (Faye, 2026-11-10).
                    for k in self.collection_order() {
                        if k == 0 {
                            out.push(SideItem { depth: 1, glyph: 0xE735, tint: Some(0xF2C94C), label: "Favourites".into(), target: SideTarget::Place(Place::Collection(Collection::Favourites)), node: None, expanded: false });
                        } else {
                            let c = k - 1;
                            let (name, rgb_) = explorer::COLOURS[c as usize];
                            out.push(SideItem { depth: 1, glyph: 0xEA3B, tint: Some(rgb_), label: name.into(), target: SideTarget::Place(Place::Collection(Collection::Colour(c))), node: None, expanded: false });
                        }
                    }
                }
                SideSection::History => {
                    if !self.settings.visited.is_empty() {
                        out.push(heading(sec, "History", 0xE81C));
                        for v in self.settings.visited.clone() {
                            out.push(SideItem { depth: 1, glyph: 0xE8B7, tint: Some(FOLDER_TINT), label: explorer::folder_name(&v), target: SideTarget::Place(Place::Folder(v)), node: None, expanded: false });
                        }
                    }
                }
                SideSection::Places => {
                    out.push(heading(sec, "Places", 0xE8B7));
                    self.read_places();
                    let (known, drives) = self.explorer.places.clone().unwrap_or_default();
                    for (name, path, glyph) in known {
                        self.push_tree(&mut out, "p", &path, name, glyph, 1);
                    }
                    out.push(SideItem { depth: 1, glyph: 0xE7F8, tint: None, label: explorer::COMPUTER_NAME.into(), target: SideTarget::Place(Place::Computer), node: None, expanded: false });
                    for d in drives {
                        // Mapped network drives go under Network.
                        if !self.explorer.net_drives.iter().any(|n| n.path == d.path) {
                            self.push_tree(&mut out, "d", &d.path, d.name.clone(), 0xEDA2, 1);
                        }
                    }
                }
                SideSection::Network => {
                    // As Explorer's Network: the mapped drives, then the computers Windows finds (each opening
                    // into its shares).
                    out.push(heading(sec, "Network", 0xE968));
                    self.read_places();
                    for d in self.explorer.net_drives.clone() {
                        self.push_tree(&mut out, "n", &d.path, d.name.clone(), 0xE8CE, 1);
                    }
                    if !self.explorer.children.contains_key(NET_KEY) {
                        self.discover_network();
                    }
                    let computers = self.explorer.children.get(NET_KEY).cloned().unwrap_or_default();
                    for c in &computers {
                        let name = c.to_string_lossy().trim_start_matches('\\').to_string();
                        self.push_tree(&mut out, "n", c, name, 0xE977, 1);
                    }
                    if self.explorer.net_searching > 0 {
                        out.push(SideItem { depth: 1, glyph: 0, tint: None, label: "Looking for computers\u{2026}".into(), target: SideTarget::Hint, node: None, expanded: false });
                    } else if computers.is_empty() && self.explorer.net_drives.is_empty() {
                        out.push(SideItem { depth: 1, glyph: 0, tint: None, label: "No computers found".into(), target: SideTarget::Hint, node: None, expanded: false });
                    }
                }
            }
        }
        out
    }

    /// Places' folders and drives, and the mapped network drives, read once (again after F5).
    fn read_places(&mut self) {
        if self.explorer.places.is_none() {
            let drives = explorer::drives();
            self.explorer.net_drives = explorer::network_drives_of(&drives);
            self.explorer.places = Some((explorer::known_places(), drives));
        }
    }

    /// Looks for the computers on the network off the UI thread, both ways at once (Explorer's discovery
    /// takes ~14 s, the old browser service ~10): each arrives as it's found, merged into `children[NET_KEY]`.
    fn discover_network(&mut self) {
        let e = &mut self.explorer;
        e.net_gen += 1;
        e.net_searching = 2;
        e.children.insert(NET_KEY.into(), Vec::new());
        let (hwnd, net_gen) = (self.hwnd.0 as isize, e.net_gen);
        let post = move |children: Vec<PathBuf>, done: bool| {
            let ptr = Box::into_raw(Box::new(Subfolders { of: SubFor::Network(net_gen, done), children, files: Vec::new() }));
            unsafe {
                if PostMessageW(Some(HWND(hwnd as _)), explorer::WM_SUBFOLDERS, WPARAM(0), LPARAM(ptr as isize)).is_err() {
                    drop(Box::from_raw(ptr));
                }
            }
        };
        std::thread::Builder::new()
            .name("network-shell".into())
            .spawn(move || {
                unsafe {
                    let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
                }
                explorer::shell_computers(|c| post(vec![c], false));
                post(Vec::new(), true);
            })
            .ok();
        std::thread::Builder::new().name("network-wnet".into()).spawn(move || post(explorer::browser_computers(), true)).ok();
    }

    /// A tree node and, while it's open, its listed children under it (listing them if they aren't).
    fn push_tree(&mut self, out: &mut Vec<SideItem>, section: &str, path: &Path, label: String, glyph: u16, depth: u8) {
        let key = format!("{section}:{}", explorer::norm(path));
        let expanded = self.explorer.expanded.contains(&key);
        let tint = (glyph == 0xE8B7).then_some(FOLDER_TINT);
        out.push(SideItem { depth, glyph, tint, label, target: SideTarget::Place(Place::Folder(path.to_path_buf())), node: Some(key.clone()), expanded });
        if !expanded || depth > 8 {
            return;
        }
        match self.explorer.children.get(&key).cloned() {
            Some(kids) => {
                for k in kids {
                    let name = explorer::folder_name(&k);
                    self.push_tree(out, section, &k, name, 0xE8B7, depth + 1);
                }
            }
            None => {
                if self.explorer.tree_pending.insert(key.clone()) {
                    self.list_subfolders(path.to_path_buf(), SubFor::Tree(key));
                }
            }
        }
    }

    pub(super) fn toggle_node(&mut self, key: String) {
        if !self.explorer.expanded.remove(&key) {
            self.explorer.expanded.insert(key);
        }
        self.invalidate();
    }

    /// A folder's subfolders, listed off the UI thread (a tree node opening, a crumb's menu).
    pub(super) fn list_subfolders(&self, dir: PathBuf, of: SubFor) {
        let hwnd = self.hwnd.0 as isize;
        std::thread::Builder::new()
            .name("subfolders".into())
            .spawn(move || {
                let children = explorer::subfolders(&dir);
                let ptr = Box::into_raw(Box::new(Subfolders { of, children, files: Vec::new() }));
                unsafe {
                    if PostMessageW(Some(HWND(hwnd as _)), explorer::WM_SUBFOLDERS, WPARAM(0), LPARAM(ptr as isize)).is_err() {
                        drop(Box::from_raw(ptr));
                    }
                }
            })
            .ok();
    }

    pub(super) fn on_subfolders(&mut self, s: Subfolders) {
        match s.of {
            SubFor::Tree(key) => {
                self.explorer.tree_pending.remove(&key);
                self.explorer.children.insert(key, s.children);
                self.invalidate();
            }
            SubFor::Network(net_gen, done) => {
                let e = &mut self.explorer;
                if net_gen != e.net_gen {
                    return;
                }
                if done {
                    e.net_searching = e.net_searching.saturating_sub(1);
                }
                explorer::merge_computers(e.children.entry(NET_KEY.into()).or_default(), s.children);
                self.invalidate();
            }
            SubFor::Popup(serial, dir) => {
                let Some(pp) = &mut self.explorer.path_edit else { return };
                if pp.serial != serial || pp.listed.as_ref().is_none_or(|l| !explorer::same_path(l, &dir)) {
                    return;
                }
                pp.entries = s.children.into_iter().map(|p| (p, true)).chain(s.files.into_iter().map(|p| (p, false))).collect();
                self.popup_filter();
            }
        }
    }

    /// A sidebar row was clicked.
    pub(super) fn sidebar_click(&mut self, i: usize) {
        self.explorer.focused = true;
        self.explorer_commit_fields();
        let items = self.sidebar_items();
        let Some(it) = items.get(i) else { return };
        if let SideTarget::Place(p) = &it.target {
            let p = p.clone();
            self.explorer.sidebar_open = false;
            self.explorer_crumb(p);
        }
    }

    // --- Lazy columns -------------------------------------------------------------------------------

    /// Asks for what the lazy columns show (the size from the image header, camera and date from EXIF) for
    /// the rows on screen that aren't known yet: read off the UI thread, never from online-only files.
    pub(super) fn want_meta(&mut self, rows: &[(PathBuf, u64, bool)]) {
        let e = &mut self.explorer;
        let mut batch = Vec::new();
        for (p, stamp, cloud) in rows {
            let k = explorer::norm(p);
            if *cloud || e.meta.get(&k).is_some_and(|m| m.stamp == *stamp) || e.meta_pending.contains(&k) {
                continue;
            }
            e.meta_pending.insert(k);
            batch.push((p.clone(), *stamp));
        }
        if batch.is_empty() {
            return;
        }
        if e.meta_tx.is_none() {
            let (tx, rx) = std::sync::mpsc::channel::<Vec<(PathBuf, u64)>>();
            let hwnd = self.hwnd.0 as isize;
            std::thread::Builder::new()
                .name("explorer-meta".into())
                .spawn(move || {
                    unsafe {
                        let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
                    }
                    let f: Option<windows::Win32::Graphics::Imaging::IWICImagingFactory> = unsafe {
                        windows::Win32::System::Com::CoCreateInstance(&windows::Win32::Graphics::Imaging::CLSID_WICImagingFactory2, None, windows::Win32::System::Com::CLSCTX_INPROC_SERVER).ok()
                    };
                    while let Ok(batch) = rx.recv() {
                        let out: Vec<(PathBuf, Meta)> = batch
                            .into_iter()
                            .map(|(p, stamp)| {
                                // An audio file: its length.
                                if format::from_extension(p.extension().and_then(|e| e.to_str())) == format::Format::Audio {
                                    let (duration_us, fields) = audio_fields(&p);
                                    return (p, Meta { stamp, dims: None, taken: None, camera: None, lens: None, duration_us, fields });
                                }
                                // A video: its size, length and recording date from one probe.
                                if format::from_extension(p.extension().and_then(|e| e.to_str())) == format::Format::Video {
                                    let info = crate::imaging::video::probe(&p).ok();
                                    let dims = info.as_ref().filter(|i| i.width > 0 && i.height > 0).map(|i| (i.width as u32, i.height as u32));
                                    let duration_us = info.as_ref().map(|i| i.duration_us).filter(|&d| d > 0);
                                    let taken = info.as_ref().and_then(|i| crate::metadata::local_time(i.created));
                                    let fields = info.as_ref().map(video_fields).unwrap_or_default();
                                    return (p, Meta { stamp, dims, taken, camera: None, lens: None, duration_us, fields });
                                }
                                let dims = f.as_ref().and_then(|f| explorer::dimensions(f, &p));
                                let exif = crate::metadata::read(&p);
                                let camera = exif.as_ref().and_then(|x| x.model.clone().or_else(|| x.make.clone()));
                                let lens = exif.as_ref().and_then(|x| x.lens.clone());
                                let taken = exif.as_ref().and_then(|x| x.taken);
                                let fields = exif.as_ref().map(photo_fields).unwrap_or_default();
                                (p, Meta { stamp, dims, taken, camera, lens, duration_us: None, fields })
                            })
                            .collect();
                        let ptr = Box::into_raw(Box::new(out));
                        unsafe {
                            if PostMessageW(Some(HWND(hwnd as _)), explorer::WM_DIMENSIONS, WPARAM(0), LPARAM(ptr as isize)).is_err() {
                                drop(Box::from_raw(ptr));
                            }
                        }
                    }
                })
                .ok();
            e.meta_tx = Some(tx);
        }
        if let Some(tx) = &e.meta_tx {
            let _ = tx.send(batch);
        }
    }

    pub(super) fn on_meta(&mut self, got: Vec<(PathBuf, Meta)>) {
        for (p, m) in got {
            let k = explorer::norm(&p);
            self.explorer.meta_pending.remove(&k);
            self.explorer.meta.insert(k, m);
        }
        // Sorted by one of these columns: the rows take their places as the values come in.
        if self.explorer.xsort.is_some_and(|(c, _)| c.lazy()) {
            self.explorer.resort(self.settings.sort, self.explorer.look.group);
        }
        self.invalidate();
    }

    // --- Columns ------------------------------------------------------------------------------------

    fn save_columns(&mut self) {
        self.store_view();
        self.invalidate();
    }

    /// A header was clicked: Name, Date modified and Size sort the toolbar's way (the viewer follows); the
    /// others sort the list alone. Again flips the direction.
    pub(super) fn header_click(&mut self, idx: usize) {
        let Some(&(c, _)) = self.explorer.look.columns.get(idx) else { return };
        if let Some(f) = c.shared_sort() {
            let was_local = self.explorer.xsort.take().is_some();
            if was_local && self.settings.sort.field != f {
                self.set_sort(crate::folder::Sort { field: f, descending: false });
            } else if was_local {
                self.explorer.resort(self.settings.sort, self.explorer.look.group);
            } else {
                self.sort_by_column(f);
            }
        } else {
            let desc = matches!(self.explorer.xsort, Some((k, d)) if k == c && !d);
            self.explorer.xsort = Some((c, desc));
            self.explorer.resort(self.settings.sort, self.explorer.look.group);
        }
        self.invalidate();
    }

    /// Columns ease to where they're going for a moment (a move, a fit, one shown or closed).
    pub(super) fn animate_columns(&mut self) {
        self.explorer.col_anim_until = Instant::now() + std::time::Duration::from_millis(400);
        self.invalidate();
    }

    pub(super) fn header_press(&mut self, idx: usize, x: f32) {
        let left = self.hits.rect_of(Hit::ColHeader(idx as u8)).map_or(x, |r| r.left);
        self.explorer.header_drag = Some(HeaderDrag { idx, start_x: x, x, grab: x - left, active: false, insert: 0 });
    }

    /// Dragging a header: past a few DIPs it lifts and follows the pointer with its cells; the others ease
    /// aside round the slot it would land in (worked out as the headers are drawn).
    pub(super) fn header_drag_move(&mut self, x: f32) {
        let Some(d) = &mut self.explorer.header_drag else { return };
        d.x = x;
        if !d.active && (x - d.start_x).abs() > DRAG_START {
            d.active = true;
            self.hide_tooltip();
        }
        self.explorer.col_anim_until = Instant::now() + std::time::Duration::from_millis(400);
        self.invalidate();
    }

    /// The button came up on a header: a drag drops the column into its slot, easing there from where it was
    /// let go. True when it was a drag (not a click, which sorts).
    pub(super) fn header_release(&mut self) -> bool {
        let Some(d) = self.explorer.header_drag.take() else { return false };
        if !d.active {
            return false;
        }
        // The others, as shown, in order; the column goes before the one at `insert`, else after the last.
        let others: Vec<usize> = self.explorer.col_shown.iter().map(|&(i, _, _)| i).filter(|&i| i != d.idx).collect();
        let cols = &mut self.explorer.look.columns;
        if d.idx < cols.len() {
            let before = others.get(d.insert).map(|&i| cols[i].0);
            let after = others.last().map(|&i| cols[i].0);
            let c = cols.remove(d.idx);
            let at = match (before, after) {
                (Some(b), _) => cols.iter().position(|(k, _)| *k == b).unwrap_or(0),
                (None, Some(a)) => cols.iter().position(|(k, _)| *k == a).map_or(cols.len(), |p| p + 1),
                (None, None) => 0,
            };
            cols.insert(at.min(cols.len()), c);
        }
        self.animate_columns();
        self.save_columns();
        true
    }

    /// A press on the edge between two columns.
    pub(super) fn col_edge_press(&mut self, idx: usize, x: f32) {
        let widths = self.explorer.col_shown.clone();
        if let Some(pos) = widths.iter().position(|&(i, _, _)| i == idx) {
            self.explorer.col_resize = Some(ColResize { pos, x0: x, widths });
        }
    }

    /// Dragging an edge, as File Pilot and Transient do: the column left of it takes the change, and every column to
    /// its right shares it at the same time (Faye, 2026-11-10, after Transient's `kit/list.rs`), so one drag re-fits
    /// the lot and the edge stays under the pointer. Widening, the ones on the right give up room in proportion to
    /// how much each has above its narrowest; narrowing, they grow in proportion to their widths. Name fills what
    /// the others leave, so its share is taken or given implicitly. All from the widths at the press, so a drag
    /// never compounds.
    pub(super) fn col_edge_drag(&mut self, x: f32) {
        let Some(r) = &self.explorer.col_resize else { return };
        let min = |c: ColKind| if c == ColKind::Name { super::explorer_view::NAME_MIN } else { 40.0 };
        let mut w: Vec<f32> = r.widths.iter().map(|&(_, _, w)| w).collect();
        let p = r.pos;
        if p + 1 >= w.len() {
            return;
        }
        let d = x - r.x0;
        let right = p + 1..w.len();
        if d > 0.0 {
            let slack: f32 = right.clone().map(|k| (w[k] - min(r.widths[k].1)).max(0.0)).sum();
            let take = d.min(slack);
            if take > 0.0 {
                for k in right {
                    w[k] -= take * (r.widths[k].2 - min(r.widths[k].1)).max(0.0) / slack;
                }
                w[p] += take;
            }
        } else {
            let give = (-d).min((w[p] - min(r.widths[p].1)).max(0.0));
            let total: f32 = right.clone().map(|k| w[k]).sum();
            if give > 0.0 && total > 0.0 {
                for k in right {
                    w[k] += give * r.widths[k].2 / total;
                }
                w[p] -= give;
            }
        }
        // Name's own width is what the others leave, so only theirs are kept.
        let updates: Vec<(usize, ColKind, f32)> = r.widths.iter().zip(&w).map(|(&(i, c, _), &nw)| (i, c, nw)).collect();
        for (i, c, nw) in updates {
            if c != ColKind::Name {
                if let Some(col) = self.explorer.look.columns.get_mut(i) {
                    col.1 = nw.round().clamp(40.0, 1200.0);
                }
            }
        }
        self.explorer.col_anim_until = Instant::now();
        self.invalidate();
    }

    pub(super) fn col_edge_release(&mut self) {
        if self.explorer.col_resize.take().is_some() {
            self.save_columns();
        }
    }

    /// Size a column to its widest value (and its header) across the folder.
    pub(super) fn col_fit(&mut self, idx: usize) {
        let Some(&(c, _)) = self.explorer.look.columns.get(idx) else { return };
        if c == ColKind::Name {
            return;
        }
        let Some(g) = self.gfx.as_ref() else { return };
        let mut w = g.measure(&wide(c.label()), &g.fonts.body) + 34.0;
        for &i in &self.explorer.view {
            let r = &self.explorer.rows[i];
            let t = super::explorer_view::cell_text(r, c, self.explorer.meta.get(&explorer::norm(&r.path)));
            w = w.max(g.measure(&wide(&t), &g.fonts.body) + 22.0);
        }
        if let Some(col) = self.explorer.look.columns.get_mut(idx) {
            col.1 = w.ceil().clamp(40.0, 600.0);
        }
        self.animate_columns();
        self.save_columns();
    }

    pub(super) fn col_fit_all(&mut self) {
        for i in 0..self.explorer.look.columns.len() {
            if self.explorer.look.columns[i].0 != ColKind::Name {
                self.col_fit(i);
            }
        }
    }

    pub(super) fn col_close(&mut self, idx: usize) {
        if self.explorer.look.columns.get(idx).is_some_and(|(c, _)| *c != ColKind::Name) {
            let (c, _) = self.explorer.look.columns.remove(idx);
            self.animate_columns();
            if self.explorer.xsort.is_some_and(|(k, _)| k == c) {
                self.explorer.xsort = None;
                self.explorer.resort(self.settings.sort, self.explorer.look.group);
            }
            self.save_columns();
        }
    }

    pub(super) fn col_shift(&mut self, idx: usize, right: bool) {
        let cols = &mut self.explorer.look.columns;
        let to = if right { idx + 1 } else { idx.wrapping_sub(1) };
        if idx < cols.len() && to < cols.len() {
            cols.swap(idx, to);
            self.animate_columns();
            self.save_columns();
        }
    }

    pub(super) fn col_restore(&mut self) {
        self.explorer.look.columns = ColKind::defaults();
        self.animate_columns();
        if self.explorer.xsort.take().is_some() {
            self.explorer.resort(self.settings.sort, self.explorer.look.group);
        }
        self.save_columns();
    }

    /// A column ticked in the header menu: shown at the end, or taken off.
    pub(super) fn col_toggle(&mut self, c: ColKind) {
        match self.explorer.look.columns.iter().position(|(k, _)| *k == c) {
            Some(i) => self.col_close(i),
            None => {
                self.explorer.look.columns.push((c, c.default_width()));
                self.animate_columns();
                self.save_columns();
            }
        }
    }

    // --- Tabs, as Transient's -------------------------------------------------------------------------

    /// The tabs as the settings last had them (once, before the first listing): the one in front then takes
    /// the photo's folder.
    fn load_tabs(&mut self) {
        if self.explorer.tabs_loaded {
            return;
        }
        self.explorer.tabs_loaded = true;
        let places: Vec<Option<Place>> = self.settings.tabs.iter().map(|k| explorer::place_from_key(k)).collect();
        if places.len() > 1 {
            let e = &mut self.explorer;
            e.tabs = places
                .into_iter()
                .enumerate()
                .map(|(i, place)| TabState { id: i as u64, place, ..TabState::default() })
                .collect();
            e.next_tab_id = e.tabs.len() as u64;
            e.active_tab = self.settings.tab_active.min(e.tabs.len() - 1);
        }
    }

    /// Where tab `i` is (the one in front: where the explorer is).
    pub(super) fn tab_place(&self, i: usize) -> Option<Place> {
        if i == self.explorer.active_tab { self.explorer.place.clone() } else { self.explorer.tabs.get(i).and_then(|t| t.place.clone()) }
    }

    pub(super) fn save_tabs(&mut self) {
        let keys: Vec<String> = (0..self.explorer.tabs.len()).map(|i| self.tab_place(i).map_or_else(|| "::pc".into(), |p| explorer::place_key(&p))).collect();
        if keys != self.settings.tabs || self.settings.tab_active != self.explorer.active_tab {
            self.settings.tabs = keys;
            self.settings.tab_active = self.explorer.active_tab;
            settings::save(&self.settings);
        }
    }

    /// Puts the tab in front aside: its place, history, cursor, selection, scroll, filter and column sort.
    fn park_tab(&mut self) {
        let e = &mut self.explorer;
        let i = e.active_tab;
        let Some(t) = e.tabs.get_mut(i) else { return };
        t.place = e.place.clone();
        t.history = std::mem::take(&mut e.history);
        t.cursor = e.cursor.take();
        t.anchor = e.anchor.take();
        t.selected = std::mem::take(&mut e.selected);
        t.scroll = e.scroll;
        t.filter = e.filter.edit.text();
        t.xsort = e.xsort.take();
    }

    /// Brings tab `i` to the front as it was left, listing its place again.
    fn unpark_tab(&mut self, i: usize) {
        let e = &mut self.explorer;
        e.active_tab = i;
        let Some(t) = e.tabs.get_mut(i) else { return };
        let place = t.place.clone();
        e.history = std::mem::take(&mut t.history);
        e.cursor = t.cursor.take();
        e.anchor = t.anchor.take();
        e.selected = std::mem::take(&mut t.selected);
        e.xsort = t.xsort.take();
        let scroll = t.scroll;
        e.filter = TextField::new(TextEdit::new(&std::mem::take(&mut t.filter)));
        e.watcher = None;
        e.rows.clear();
        e.view.clear();
        e.rename = None;
        e.path_edit = None;
        e.anim = None;
        e.came_from = None;
        if e.field.is_some() {
            e.field = None;
        }
        e.place = place.clone();
        e.scroll = scroll;
        match place {
            Some(p) => {
                if e.history.entries().0.is_empty() {
                    e.history.record(&p);
                }
                let v = self.view_for(&p);
                self.adopt_view(v);
                self.explorer_list(p, true);
            }
            None => self.explorer_go(Place::Computer, true),
        }
        self.save_tabs();
        self.invalidate();
    }

    pub(super) fn switch_tab(&mut self, i: usize) {
        if i == self.explorer.active_tab || i >= self.explorer.tabs.len() {
            return;
        }
        self.park_tab();
        self.unpark_tab(i);
    }

    /// A new tab right of the one in front, at `place` (the one in front's when None), brought to the front.
    pub(super) fn new_tab(&mut self, place: Option<Place>) {
        self.load_tabs();
        let place = place.or_else(|| self.explorer.place.clone()).unwrap_or(Place::Computer);
        self.park_tab();
        let e = &mut self.explorer;
        let id = e.next_tab_id;
        e.next_tab_id += 1;
        let at = (e.active_tab + 1).min(e.tabs.len());
        e.tabs.insert(at, TabState { id, place: Some(place.clone()), ..TabState::default() });
        e.active_tab = at;
        e.tab_anim_until = Instant::now() + std::time::Duration::from_millis(400);
        e.selected.clear();
        e.cursor = None;
        e.anchor = None;
        e.xsort = None;
        e.place = None;
        self.explorer_go(place, true);
    }

    pub(super) fn close_tab(&mut self, i: usize) {
        let n = self.explorer.tabs.len();
        if n <= 1 || i >= n {
            return;
        }
        let e = &mut self.explorer;
        e.tab_anim_until = Instant::now() + std::time::Duration::from_millis(400);
        let id = e.tabs[i].id;
        e.tab_pos.remove(&id);
        if i == e.active_tab {
            // Its right neighbour comes to the front, else its left.
            e.tabs.remove(i);
            let heir = i.min(e.tabs.len() - 1);
            self.unpark_tab(heir);
        } else {
            e.tabs.remove(i);
            if i < e.active_tab {
                e.active_tab -= 1;
            }
            self.save_tabs();
            self.invalidate();
        }
    }

    pub(super) fn close_other_tabs(&mut self, keep: usize) {
        if keep >= self.explorer.tabs.len() {
            return;
        }
        self.switch_tab(keep);
        let e = &mut self.explorer;
        let id = e.tabs[e.active_tab].id;
        e.tabs.retain(|t| t.id == id);
        e.active_tab = 0;
        e.tab_pos.retain(|k, _| *k == id);
        self.save_tabs();
        self.invalidate();
    }

    /// Ctrl+Tab and Ctrl+Shift+Tab: the next tab, or the one before, round the strip.
    pub(super) fn cycle_tab(&mut self, back: bool) {
        let n = self.explorer.tabs.len();
        if n > 1 {
            let i = self.explorer.active_tab;
            self.switch_tab(if back { (i + n - 1) % n } else { (i + 1) % n });
        }
    }

    /// A folder renamed or moved: tabs in or under it follow.
    pub(super) fn tabs_moved(&mut self, from: &Path, to: &Path) {
        for t in &mut self.explorer.tabs {
            if let Some(Place::Folder(f)) = &t.place {
                if let Some(n) = explorer::rebase(f, from, to) {
                    t.place = Some(Place::Folder(n));
                }
            }
            t.history.moved(from, to);
        }
    }

    /// Rows dragged out of the window: the drag inside ends and Windows' takes the files (copy or link) into
    /// whatever they're dropped on. The system's drag loop is modal and swallows the button release.
    pub(super) fn explorer_drag_out(&mut self) {
        let Some(d) = self.explorer.row_drag.take() else { return };
        self.pressed = None;
        unsafe {
            let _ = ReleaseCapture();
        }
        self.invalidate();
        crate::clipboard::drag_files(self.hwnd, &d.paths);
        self.invalidate();
    }

    /// A press on a tab brings it to the front (Transient's); moving on drags it.
    pub(super) fn tab_press(&mut self, i: usize, x: f32) {
        self.switch_tab(i);
        let left = self.hits.rect_of(Hit::Tab(i as u8)).map_or(x, |r| r.left);
        self.explorer.tab_drag = Some(TabDrag { idx: i, start_x: x, x, grab: x - left, active: false, insert: i });
    }

    pub(super) fn tab_drag_move(&mut self, x: f32) {
        let Some(d) = self.explorer.tab_drag.as_mut() else { return };
        d.x = x;
        if !d.active && (x - d.start_x).abs() > DRAG_START {
            d.active = true;
        }
        if d.active {
            self.invalidate();
        }
    }

    /// Let go: a dragged tab drops into its slot (the one in front stays in front). True when it was a drag.
    pub(super) fn tab_release(&mut self) -> bool {
        let Some(d) = self.explorer.tab_drag.take() else { return false };
        if !d.active {
            return false;
        }
        let e = &mut self.explorer;
        if d.idx < e.tabs.len() {
            let front = e.tabs[e.active_tab].id;
            let t = e.tabs.remove(d.idx);
            e.tabs.insert(d.insert.min(e.tabs.len()), t);
            e.active_tab = e.tabs.iter().position(|t| t.id == front).unwrap_or(0);
        }
        e.tab_anim_until = Instant::now() + std::time::Duration::from_millis(400);
        self.save_tabs();
        self.invalidate();
        true
    }

    // --- Sidebar sections, dragged by their headings ----------------------------------------------------

    /// A click on a heading folds its section away to the heading, or opens it again (Transient's).
    pub(super) fn toggle_section(&mut self, sec: SideSection) {
        let closed = &mut self.settings.sidebar_closed;
        match closed.iter().position(|&s| s == sec) {
            Some(i) => {
                closed.remove(i);
            }
            None => closed.push(sec),
        }
        settings::save(&self.settings);
        self.invalidate();
    }

    pub(super) fn section_press(&mut self, sec: SideSection, y: f32) {
        let top = self.hits.rect_of(Hit::SideHeader(sec)).map_or(y, |r| r.top - 8.0);
        self.explorer.section_drag = Some(SectionDrag { sec, start_y: y, y, grab: y - top, active: false, insert: 0 });
    }

    pub(super) fn section_drag_move(&mut self, y: f32) {
        let Some(d) = self.explorer.section_drag.as_mut() else { return };
        d.y = y;
        if !d.active && (y - d.start_y).abs() > DRAG_START {
            d.active = true;
        }
        if d.active {
            self.invalidate();
        }
    }

    /// Let go: the section moves to its slot (and the order is saved). True when it was a drag.
    pub(super) fn section_release(&mut self) -> bool {
        let Some(d) = self.explorer.section_drag.take() else { return false };
        if !d.active {
            return false;
        }
        let mut order: Vec<SideSection> = self.settings.sidebar_order.iter().copied().filter(|&s| s != d.sec).collect();
        order.insert(d.insert.min(order.len()), d.sec);
        if order != self.settings.sidebar_order {
            self.settings.sidebar_order = order;
            settings::save(&self.settings);
        }
        self.explorer.section_anim_until = Instant::now() + std::time::Duration::from_millis(400);
        self.invalidate();
        true
    }

    /// The sidebar row `i`'s section and its place among the section's entries, when it's a bookmark or a collection
    /// (what can be dragged into another order).
    fn side_entry(&mut self, i: usize) -> Option<(SideSection, usize)> {
        let items = self.sidebar_items();
        let it = items.get(i)?;
        if it.depth != 1 || !matches!(it.target, SideTarget::Place(_)) {
            return None;
        }
        let head = items[..i].iter().rposition(|x| matches!(x.target, SideTarget::Header(_)))?;
        let SideTarget::Header(sec) = items[head].target else { return None };
        if !matches!(sec, SideSection::Bookmarks | SideSection::Collections) {
            return None;
        }
        let n = items[head + 1..i].iter().filter(|x| x.depth == 1 && matches!(x.target, SideTarget::Place(_))).count();
        Some((sec, n))
    }

    /// A press on a sidebar row: a bookmark or collection may become a drag of it.
    pub(super) fn side_row_press(&mut self, i: usize, y: f32) {
        let Some((sec, from)) = self.side_entry(i) else { return };
        let top = self.hits.rect_of(Hit::SideRow(i)).map_or(y, |r| r.top);
        self.explorer.side_row_drag = Some(EntryDrag { sec, from, start_y: y, y, grab: y - top, active: false, insert: from });
    }

    pub(super) fn side_row_drag_move(&mut self, y: f32) {
        let Some(d) = self.explorer.side_row_drag.as_mut() else { return };
        d.y = y;
        let started = !d.active && (y - d.start_y).abs() > DRAG_START;
        if started {
            d.active = true;
        }
        let active = d.active;
        if started {
            self.hide_tooltip();
        }
        if active {
            self.invalidate();
        }
    }

    /// Let go: the entry moves to its slot and the order is saved. True when it was a drag (not a click).
    pub(super) fn side_row_release(&mut self) -> bool {
        let Some(d) = self.explorer.side_row_drag.take() else { return false };
        if !d.active {
            return false;
        }
        if d.insert != d.from {
            match d.sec {
                SideSection::Bookmarks => {
                    let b = &mut self.settings.bookmarks;
                    if d.from < b.len() {
                        let it = b.remove(d.from);
                        b.insert(d.insert.min(b.len()), it);
                    }
                }
                _ => {
                    let mut order = self.collection_order();
                    if d.from < order.len() {
                        let it = order.remove(d.from);
                        order.insert(d.insert.min(order.len()), it);
                    }
                    self.settings.collection_order = order;
                }
            }
            settings::save(&self.settings);
        }
        self.explorer.row_anim_until = Instant::now() + std::time::Duration::from_millis(400);
        self.invalidate();
        true
    }

    /// The Collections section's entries in order, as `settings::collection_order` keys (0 Favourites, 1 + a colour):
    /// the order they were dragged into, then any that aren't in it yet, in the colours' own order.
    pub(super) fn collection_order(&self) -> Vec<u8> {
        let present: Vec<u8> = std::iter::once(0).chain(self.explorer.tags.colours_in_use().into_iter().map(|c| c + 1)).collect();
        let mut out: Vec<u8> = self.settings.collection_order.iter().copied().filter(|k| present.contains(k)).collect();
        out.extend(present.into_iter().filter(|k| !self.settings.collection_order.contains(k)));
        out
    }

    // --- Sidebar width --------------------------------------------------------------------------------

    pub(super) fn sidebar_grip_press(&mut self, x: f32) {
        self.explorer.side_drag = Some((x, self.settings.sidebar_width));
    }

    pub(super) fn sidebar_grip_drag(&mut self, x: f32) {
        let Some((x0, w0)) = self.explorer.side_drag else { return };
        let w = (w0 + x - x0).round().clamp(super::explorer_view::SIDEBAR_MIN, super::explorer_view::SIDEBAR_MAX);
        if w != self.settings.sidebar_width {
            self.settings.sidebar_width = w;
            self.invalidate();
        }
    }

    pub(super) fn sidebar_grip_release(&mut self) {
        if self.explorer.side_drag.take().is_some() {
            settings::save(&self.settings);
        }
    }

    pub(super) fn sidebar_grip_reset(&mut self) {
        self.explorer.side_settle = Some(crate::motion::Settle::from(self.sidebar_w()));
        self.settings.sidebar_width = crate::settings::SIDEBAR_WIDTH;
        settings::save(&self.settings);
        self.invalidate();
    }

    // --- Scrollbar ----------------------------------------------------------------------------------

    /// The list's thumb, as last drawn: its top, its height and the track it runs in.
    fn thumb_geometry(&self) -> Option<(f32, f32, D2D_RECT_F)> {
        let track = self.hits.rect_of(Hit::ExplorerThumb)?;
        let view_h = track.bottom - track.top;
        let content_h = view_h + self.explorer.max_scroll;
        let thumb_h = (view_h * view_h / content_h.max(1.0)).max(28.0);
        let top = track.top + (view_h - thumb_h) * self.explorer.scroll / self.explorer.max_scroll.max(1.0);
        Some((top, thumb_h, track))
    }

    /// A press on the scrollbar: on the thumb it grabs it where it was pressed; elsewhere on the track the
    /// thumb jumps to centre on the pointer first.
    pub(super) fn thumb_press(&mut self, y: f32) {
        let _ = self.explorer.scroll_now();
        self.explorer.anim = None;
        let Some((top, h, _)) = self.thumb_geometry() else { return };
        let offset = if y >= top && y < top + h { y - top } else { h / 2.0 };
        self.explorer.thumb_drag = Some(offset);
        self.thumb_move(y);
    }

    pub(super) fn thumb_move(&mut self, y: f32) {
        let Some(offset) = self.explorer.thumb_drag else { return };
        let Some((_, h, track)) = self.thumb_geometry() else { return };
        let travel = (track.bottom - track.top - h).max(1.0);
        let k = ((y - offset - track.top) / travel).clamp(0.0, 1.0);
        self.explorer.scroll = k * self.explorer.max_scroll;
        self.explorer.anim = None;
        self.invalidate();
    }

    // --- Resize grip    // --- Resize grip (dragging right = wider) -------------------------------------------------------

    pub(super) fn explorer_grip_press(&mut self, x: f32) {
        // From where the edge rests: a bounce still swinging would shift where the sticky zone starts.
        self.explorer.spring = None;
        self.explorer.stretch = 0.0;
        let w = self.explorer_width();
        // Resting on a sticky line: it holds there whichever way it's pulled.
        let (card_line, browse_line) = self.sticky_lines();
        self.explorer.latched = None;
        if (w - card_line).abs() < 1.5 {
            self.explorer.latched = Some(false);
        } else if self.browsing() && (w - browse_line).abs() < 1.5 {
            self.explorer.latched = Some(true);
        }
        self.explorer.last_side = w;
        self.explorer.coast = None;
        self.explorer.drag = Some((x, w));
    }

    /// The two sticky lines: the card's widest, and browse mode's default width (half the window, or the card and
    /// the sidebar side by side when that's wider). None for the second when the window leaves no room between.
    fn sticky_lines(&self) -> (f32, f32) {
        let (ww, _) = self.size_dip();
        let card = self.sticky_line();
        let side = self.settings.sidebar_width.clamp(super::explorer_view::SIDEBAR_MIN, super::explorer_view::SIDEBAR_MAX);
        let browse = (ww * BROWSE_SHARE).max(card + side + 1.0).min(ww - PREVIEW_DRAGGED_MIN).max(card);
        (card, browse)
    }

    /// One drag covers both widths: the card up to its widest, then browse mode from its default width on
    /// (leaving the photo at least 200 DIPs); dragged back, it's the card again.
    pub(super) fn explorer_grip_drag(&mut self, x: f32) {
        let Some((x0, w0)) = self.explorer.drag else { return };
        let (ww, _) = self.size_dip();
        let w = (w0 + (x - x0)).round().clamp(EXPLORER_MIN, (ww - PREVIEW_DRAGGED_MIN).max(EXPLORER_MIN));
        // Two sticky lines (Faye, 2026-11-10): the card's widest and browse mode's default width. Between them the
        // edge never rests: it's held on one line or the other, stretching on a rubber band towards the pointer, and
        // jumps across once pulled far enough towards the other. Pulled `STICK` off the outside of either line it
        // lets go with a bounce, free to size the card short of the first or browse mode past the second. Reaching
        // a line from outside (or, in browse mode, coming within the radius of the second while narrowing: a magnet)
        // latches it. On the first line it's the card, on the second browse mode at its default width.
        const STICK: f32 = 96.0;
        let (card_line, browse_line) = self.sticky_lines();
        let gap = browse_line - card_line;
        // Too little room for two lines: the second is the first.
        let two = gap >= 24.0;
        // How far towards the other line it's pulled before it jumps: well short of halfway, so the two jumps don't
        // meet and a pointer held between them doesn't flick back and forth.
        let jump = STICK.min(gap * 0.4);
        let prev = self.explorer.last_side;
        self.explorer.last_side = w;
        let now = Instant::now();
        let before = self.explorer_width();
        let mut just_latched = false;
        // After a jump the edge pops to the other line on its own and doesn't follow the pointer until the pointer
        // has got there too (Faye, 2026-11-10: one long drag still sizes it, without it being tugged back by a
        // slower hand); then it's held on that line as usual.
        if let Some(to_browse) = self.explorer.coast {
            if (to_browse && w >= browse_line) || (!to_browse && w <= card_line) {
                self.explorer.coast = None;
                self.explorer.latched = Some(to_browse);
                just_latched = true;
            } else {
                return;
            }
        }
        if self.explorer.latched.is_none() {
            let line = if two { browse_line } else { card_line };
            if (prev < card_line) != (w < card_line) || w == card_line || (!two && (prev > card_line) != (w > card_line)) {
                self.explorer.latched = Some(false);
                just_latched = true;
            } else if two && (self.browsing() || prev >= line) && (w <= line || (w < prev && w - line <= STICK - 8.0)) {
                self.explorer.latched = Some(true);
                just_latched = true;
            } else if !self.browsing() && w > card_line {
                // A card resting past the line (a window made smaller): the line takes it.
                self.explorer.latched = Some(false);
                just_latched = true;
            }
        }
        // A jump across from one line to the other lets go of the edge until the pointer reaches the other line
        // (`coast`): it finishes the pop on its own, gliding there with a bounce. Pulled free outward, it stays
        // held, so one pull sizes the card or browse mode.
        let mut let_go = 0.0;
        if !just_latched {
            match self.explorer.latched {
                Some(false) if w <= card_line - STICK => {
                    // Pulled free outward: still held, gliding to the pointer and following it.
                    self.explorer.latched = None;
                    self.explorer.spring = Some((-BOUNCE, now));
                    self.explorer.width_settle = Some(crate::motion::Settle::over(before, EDGE_SECS));
                }
                Some(false) if two && w >= card_line + jump => {
                    self.explorer.latched = Some(true);
                    let_go = 1.0;
                }
                Some(false) if !two && w >= card_line + STICK => {
                    self.explorer.latched = None;
                    self.explorer.spring = Some((BOUNCE, now));
                    self.explorer.width_settle = Some(crate::motion::Settle::over(before, EDGE_SECS));
                }
                Some(true) if w >= browse_line + STICK => {
                    self.explorer.latched = None;
                    self.explorer.spring = Some((BOUNCE, now));
                    self.explorer.width_settle = Some(crate::motion::Settle::over(before, EDGE_SECS));
                }
                Some(true) if w <= browse_line - jump => {
                    self.explorer.latched = Some(false);
                    let_go = -1.0;
                }
                _ => {}
            }
        }
        // Grabbed by a line (the magnet reaches out up to 88 DIPs): the edge glides onto it, never jumps
        // (Faye, 2026-11-10).
        if just_latched {
            self.explorer.width_settle = Some(crate::motion::Settle::over(before, EDGE_SECS));
        }
        let from = self.explorer_width();
        let was = self.browsing();
        match self.explorer.latched {
            Some(on_browse) => {
                let line = if on_browse { browse_line } else { card_line };
                let d = w - line;
                let pull = if just_latched { d.clamp(-STICK * 0.5, STICK * 0.5) } else { d };
                self.explorer.stretch = if let_go != 0.0 { 0.0 } else { pull.signum() * rubber(pull.abs()) };
                self.settings.explorer_width = card_line;
                if on_browse {
                    self.settings.browse_share = 0.0; // the default: half of whatever size the window is
                    if !was {
                        self.explorer.browse.set([1.0], now);
                        self.explorer.focused = true;
                    }
                } else if was {
                    self.explorer.browse.set([0.0], now);
                }
            }
            None => {
                self.explorer.stretch = 0.0;
                if w > card_line {
                    if !was {
                        self.settings.explorer_width = card_line;
                        self.explorer.browse.set([1.0], now);
                        self.explorer.focused = true;
                    }
                    self.settings.browse_share = w / ww;
                } else {
                    if was {
                        self.explorer.browse.set([0.0], now);
                    }
                    // No narrower than its default; dragged down to it, it's the default again (a quarter of
                    // whatever size the window is).
                    self.settings.explorer_width = if w <= self.card_min() { 0.0 } else { w };
                }
            }
        }
        if let_go != 0.0 {
            self.explorer.spring = Some((BOUNCE * let_go, now));
            self.explorer.width_settle = Some(crate::motion::Settle::over(from, EDGE_SECS));
            self.explorer.coast = self.explorer.latched;
            self.explorer.latched = None;
        }
        if self.browsing() != was {
            self.browse_toast(!was);
        }
        self.layout_changed();
    }

    pub(super) fn explorer_grip_release(&mut self) {
        // Let go in the sticky zone: it springs back to where it rests.
        if self.explorer.stretch != 0.0 {
            self.explorer.spring = Some((self.explorer.stretch, Instant::now()));
            self.explorer.stretch = 0.0;
            self.layout_changed();
        }
        if self.explorer.drag.take().is_some() {
            settings::save(&self.settings);
            self.invalidate();
        }
    }

    /// Double-click: browse mode back to its default width, the card to its own.
    pub(super) fn explorer_grip_reset(&mut self) {
        self.explorer.drag = None;
        // It eases from where it is to its default (Faye, 2026-11-10).
        self.explorer.width_settle = Some(crate::motion::Settle::over(self.explorer_width(), EDGE_SECS));
        if self.browsing() {
            self.settings.browse_share = 0.0;
            settings::save(&self.settings);
            self.layout_changed();
            return;
        }
        self.settings.explorer_width = 0.0; // back to a quarter of the window
        settings::save(&self.settings);
        self.layout_changed();
    }

    // --- Dragging rows onto folders -----------------------------------------------------------------

    /// A press on a row may become a drag of it (and the rest of the selection, if it's selected).
    pub(super) fn explorer_row_press(&mut self, vi: usize, x: f32, y: f32) {
        let Some(r) = self.explorer.row(vi).cloned() else { return };
        let paths = if self.explorer.is_selected(&r.path) { self.explorer.targets() } else { vec![r.path] };
        self.explorer.row_drag = Some(RowDrag { start: (x, y), paths, active: false, target: None, target_rect: None, opened: Instant::now() });
    }

    /// Whether a press at (x, y) on the list starts a selection rectangle rather than a drag of rows: on empty
    /// space, or on a row away from its Name column (Windows' details view, Transient's).
    pub(super) fn marquee_starts_at(&self, x: f32, row: bool) -> bool {
        let Some((_, _, (n0, n1))) = self.explorer.list_geom else { return false };
        !row || x < n0 || x > n1
    }

    pub(super) fn marquee_press(&mut self, x: f32, y: f32) {
        let Some((list, _, _)) = self.explorer.list_geom else { return };
        let (scroll, _) = self.explorer.scroll_now();
        let add = unsafe { GetKeyState(VK_CONTROL.0 as i32) < 0 };
        let before = if add { self.explorer.targets() } else { Vec::new() };
        self.explorer.marquee = Some(Marquee { start: (x, y - list.top + scroll), pointer: (x, y), active: false, add, before });
    }

    /// The pointer moved with the rectangle held: past a few DIPs it selects what it crosses (scrolling the list
    /// when it's dragged past its top or bottom).
    pub(super) fn marquee_move(&mut self, x: f32, y: f32) {
        let Some((list, rh, _)) = self.explorer.list_geom else { return };
        let (scroll, _) = self.explorer.scroll_now();
        let Some(m) = &mut self.explorer.marquee else { return };
        m.pointer = (x, y);
        let (sx, sy) = m.start;
        if !m.active {
            let py = y - list.top + scroll;
            if (x - sx).abs() + (py - sy).abs() <= DRAG_START {
                return;
            }
            m.active = true;
        }
        // Past the list's top or bottom: it scrolls, as far as the pointer is out.
        let out = if y < list.top { y - list.top } else if y > list.bottom { y - list.bottom } else { 0.0 };
        if out != 0.0 {
            let e = &mut self.explorer;
            let (now, _) = e.scroll_now();
            e.anim = None;
            e.scroll = (now + out * 0.5).clamp(0.0, e.max_scroll.max(0.0));
        }
        let (scroll, _) = self.explorer.scroll_now();
        let Some(m) = &self.explorer.marquee else { return };
        let py = y.clamp(list.top, list.bottom) - list.top + scroll;
        let (top, bottom) = (sy.min(py), sy.max(py));
        let n = self.explorer.view.len();
        let first = (top / rh).floor().max(0.0) as usize;
        let last = ((bottom / rh).floor().max(0.0) as usize).min(n.saturating_sub(1));
        let mut picked: Vec<PathBuf> = if m.add { m.before.clone() } else { Vec::new() };
        if n > 0 && first < n && bottom >= 0.0 {
            for vi in first..=last {
                if let Some(r) = self.explorer.row(vi) {
                    picked.push(r.path.clone());
                }
            }
        }
        let cursor = (n > 0 && first < n).then(|| self.explorer.row(if py >= sy { last } else { first }).map(|r| r.path.clone())).flatten();
        let e = &mut self.explorer;
        e.selected = picked.iter().map(|p| explorer::norm(p)).collect();
        if let Some(c) = cursor {
            e.cursor = Some(c);
        }
        self.invalidate();
    }

    /// Let go of the rectangle: true when it selected (not a click, which picks or clears as before).
    pub(super) fn marquee_release(&mut self) -> bool {
        let active = self.explorer.marquee.take().is_some_and(|m| m.active);
        if active {
            self.invalidate();
        }
        active
    }

    /// The mouse moved with a row pressed: past a few DIPs it's a drag, and the folder under it the target.
    pub(super) fn explorer_row_drag(&mut self, x: f32, y: f32) {
        let Some(d) = &self.explorer.row_drag else { return };
        let active = d.active || (x - d.start.0).abs() + (y - d.start.1).abs() > DRAG_START;
        if !active {
            return;
        }
        // Out of the window: Windows' own drag takes the rows to another app (Transient's).
        let (w, h) = (self.client_w as f32 / self.scale(), self.client_h as f32 / self.scale());
        if x < 0.0 || y < 0.0 || x >= w || y >= h {
            unsafe {
                let _ = PostMessageW(Some(self.hwnd), explorer::WM_DRAG_OUT, WPARAM(0), LPARAM(0));
            }
            return;
        }
        let target = self.drop_target_at(x, y);
        let started = self.explorer.row_drag.as_ref().is_some_and(|d| !d.active);
        if let Some(d) = &mut self.explorer.row_drag {
            if !d.active {
                d.opened = Instant::now();
            }
            d.active = true;
            d.target_rect = target.as_ref().map(|t| t.1);
            d.target = target.map(|t| t.0);
        }
        if started {
            self.hide_tooltip();
        }
        self.invalidate();
    }

    /// The folder a drop at (x, y) would go into: a folder row (not one being dragged), a crumb, a sidebar
    /// folder. Never the folder the items are already all in, nor one inside what's dragged.
    pub(super) fn drop_target_at(&mut self, x: f32, y: f32) -> Option<(PathBuf, D2D_RECT_F)> {
        let dragged = self.explorer.row_drag.as_ref().map(|d| d.paths.clone()).unwrap_or_default();
        let hit = self.hit(x, y)?;
        // A tab's close button is the tab.
        let hit = if let Hit::TabClose(i) = hit { Hit::Tab(i) } else { hit };
        let folder = match hit {
            Hit::ExplorerRow(vi) => self.explorer.row(vi).filter(|r| r.is_folder()).map(|r| r.path.clone())?,
            Hit::Crumb(i) => explorer::crumbs(self.explorer.place.as_ref()?).get(i)?.folder()?.to_path_buf(),
            Hit::SideRow(i) => match &self.sidebar_items().get(i)?.target {
                SideTarget::Place(Place::Folder(p)) => p.clone(),
                _ => return None,
            },
            // Another tab's folder (Transient's: drop onto a tab).
            Hit::Tab(i) => match self.tab_place(i as usize)? {
                Place::Folder(p) => p,
                _ => return None,
            },
            _ => return None,
        };
        let r = self.hits.rect_of(hit)?;
        if dragged.iter().any(|d| explorer::within(&folder, d)) {
            return None;
        }
        if dragged.iter().all(|d| d.parent().is_some_and(|p| explorer::same_path(p, &folder))) {
            return None;
        }
        Some((folder, r))
    }

    /// The button came up: a drag drops (moving, or copying with Ctrl or onto another drive); a press that
    /// never became a drag was a click. True when it was a drag.
    pub(super) fn explorer_row_release(&mut self) -> bool {
        let Some(d) = self.explorer.row_drag.take() else { return false };
        if !d.active {
            return false;
        }
        if let Some(dest) = d.target {
            let copy = self.drop_copies(&d.paths, &dest);
            self.explorer_transfer(d.paths, dest, copy);
        }
        self.invalidate();
        true
    }

    /// Explorer's rules: Ctrl copies, Shift moves, and across drives it copies.
    pub(super) fn drop_copies(&self, paths: &[PathBuf], dest: &Path) -> bool {
        let (ctrl, shift) = unsafe { (GetKeyState(VK_CONTROL.0 as i32) < 0, GetKeyState(VK_SHIFT.0 as i32) < 0) };
        if ctrl {
            return true;
        }
        if shift {
            return false;
        }
        let drive = |p: &Path| p.components().next().map(|c| c.as_os_str().to_string_lossy().to_lowercase());
        paths.first().is_some_and(|p| drive(p) != drive(dest))
    }

    // --- Tooltips -----------------------------------------------------------------------------------

    pub(super) fn explorer_tooltip(&mut self, h: Hit) -> Option<String> {
        match h {
            // The full text of the cut-off cell under the pointer (Faye, 2026-11-10); over the name of a file Looker
            // can't open, that it can't.
            Hit::ExplorerRow(i) => {
                let (mx, my) = self.mouse;
                if let Some((_, text)) = self.explorer.cell_tips.iter().find(|(r, _)| ui::contains(r, mx, my)) {
                    return Some(text.clone());
                }
                let (n0, n1) = self.explorer.list_geom.map_or((0.0, 0.0), |g| g.2);
                self.explorer.row(i).filter(|r| r.kind == Kind::Other && mx >= n0 && mx <= n1).map(|r| format!("{}: not an image Looker can open", r.name))
            }
            Hit::Tab(i) => self.explorer.tab_tips.iter().find(|(k, _)| *k == i).map(|(_, t)| t.clone()),
            Hit::Crumb(_) | Hit::CrumbMore => self.explorer.place.as_ref().map(|p| match p {
                Place::Folder(f) => f.to_string_lossy().into_owned(),
                other => other.name(),
            }),
            Hit::ExplorerBack => Some("Back (Alt+Left)".into()),
            Hit::ExplorerForward => Some("Forward (Alt+Right)".into()),
            Hit::ExplorerUp => Some("Up (Backspace)".into()),
            Hit::ExplorerHistory => Some("Recent folders".into()),
            Hit::ExplorerBookmarks => Some("Bookmarks".into()),
            Hit::ExplorerCollections => Some("Collections".into()),
            Hit::ExplorerGroup => Some("Folders and files".into()),
            Hit::TabNew => Some("New tab  (Ctrl+T)".into()),
            Hit::TabClose(_) => Some("Close tab  (Ctrl+W)".into()),
            Hit::ExplorerBookmark => Some(if self.explorer.folder().is_some_and(|f| self.settings.is_bookmarked(&f)) { "Remove bookmark".into() } else { "Bookmark this folder".into() }),
            Hit::ExplorerSidebar => Some("Bookmarks and places".into()),
            Hit::ExplorerBrowse => Some(if self.browsing() { "Back to the photo (Ctrl+B)".into() } else { "Browse mode (Ctrl+B)".into() }),
            Hit::SideRow(i) => match self.sidebar_items().get(i).map(|it| it.target.clone()) {
                Some(SideTarget::Place(Place::Folder(p))) => Some(p.to_string_lossy().into_owned()),
                _ => None,
            },
            _ => None,
        }
    }

    /// Where a tooltip for `h` hangs from: a row's from the cell under the pointer, else the hit's own rect.
    pub(super) fn explorer_tip_anchor(&self, h: Hit) -> Option<D2D_RECT_F> {
        if let Hit::ExplorerRow(_) = h {
            let (mx, my) = self.mouse;
            if let Some((r, _)) = self.explorer.cell_tips.iter().find(|(r, _)| ui::contains(r, mx, my)) {
                return Some(*r);
            }
        }
        self.hits.rect_of(h)
    }

    /// The pointer moved over the list: crossing into another cell of the same row starts its tooltip afresh.
    pub(super) fn explorer_tip_moved(&mut self, x: f32, y: f32) {
        if !matches!(self.hover, Some(Hit::ExplorerRow(_))) {
            self.explorer.tip_cell = None;
            return;
        }
        let cell = self.explorer.cell_tips.iter().position(|(r, _)| ui::contains(r, x, y));
        if cell != self.explorer.tip_cell {
            self.explorer.tip_cell = cell;
            self.hide_tooltip();
            if cell.is_some() && self.menu.is_none() {
                unsafe {
                    SetTimer(Some(self.hwnd), super::TIMER_TOOLTIP, ui::TOOLTIP_DELAY_MS, None);
                }
            }
        }
    }

    /// The crumbs "…" hides, for its menu.
    pub(super) fn hidden_crumbs(&self) -> Vec<Place> {
        self.explorer.hidden_crumbs.clone()
    }

    /// A crumb was clicked: an ancestor lists; the last one (where you are) turns into the path, to type over.
    pub(super) fn crumb_click(&mut self, i: usize) {
        let Some(place) = self.explorer.place.clone() else { return };
        let all = explorer::crumbs(&place);
        if i + 1 == all.len() {
            if matches!(place, Place::Folder(_) | Place::Computer) {
                self.explorer_edit_path();
            }
        } else if let Some(p) = all.get(i) {
            self.explorer_crumb(p.clone());
        }
    }

    /// A press decides who has the keys: anywhere in the explorer, it does; on the photo or the toolbar, it
    /// lets go (a rename commits, the filter and path boxes let go of the keys).
    pub(super) fn explorer_press_focus(&mut self, h: Option<Hit>) {
        let inside = matches!(
            h,
            Some(
                Hit::ExplorerCard
                    | Hit::ExplorerList
                    | Hit::ExplorerRow(_)
                    | Hit::ExplorerBack
                    | Hit::ExplorerForward
                    | Hit::ExplorerUp
                    | Hit::ExplorerHistory
                    | Hit::ExplorerBookmarks
                    | Hit::ExplorerCollections
                    | Hit::ExplorerGroup
                    | Hit::ExplorerBookmark
                    | Hit::ExplorerSidebar
                    | Hit::ExplorerBrowse
                    | Hit::Tab(_)
                    | Hit::TabClose(_)
                    | Hit::TabNew
                    | Hit::Crumb(_)
                    | Hit::CrumbChevron(_)
                    | Hit::CrumbMore
                    | Hit::CrumbArea
                    | Hit::ExplorerPath
                    | Hit::CrumbPopup
                    | Hit::CrumbPopupRow(_)
                    | Hit::CrumbPopupUp
                    | Hit::CrumbPopupClose
                    | Hit::PathClear
                    | Hit::ColHeader(_)
                    | Hit::ColEdge(_)
                    | Hit::ExplorerThumb
                    | Hit::SidebarGrip
                    | Hit::ExplorerFilter
                    | Hit::ExplorerFilterClear
                    | Hit::ExplorerRename
                    | Hit::Sidebar
                    | Hit::SideRow(_)
                    | Hit::SideChevron(_)
            )
        );
        if matches!(h, Some(Hit::MenuItem(_) | Hit::MenuSurface | Hit::MenuSwatch(..) | Hit::DialogSurface | Hit::DialogButton(_) | Hit::DialogField | Hit::DialogFieldClear)) {
            return;
        }
        let keep = match self.explorer.field {
            Some(EField::Filter) => matches!(h, Some(Hit::ExplorerFilter | Hit::ExplorerFilterClear)),
            Some(EField::Rename) => h == Some(Hit::ExplorerRename),
            Some(EField::Path) => matches!(h, Some(Hit::ExplorerPath | Hit::CrumbPopup | Hit::CrumbPopupRow(_) | Hit::CrumbPopupUp | Hit::CrumbPopupClose | Hit::PathClear)),
            None => true,
        };
        if !keep {
            self.explorer_commit_fields();
            self.explorer_blur_field();
        }
        self.explorer.focused = inside;
    }

    /// Keys for the explorer: Ctrl+F, Ctrl+Z / Ctrl+Y and B anywhere; the rest while it has the keys (a click
    /// went into it). None for keys it doesn't use.
    pub(super) fn explorer_key(&mut self, vk: VIRTUAL_KEY, ctrl: bool, shift: bool) -> Option<LRESULT> {
        let has = self.viewer.current.is_some() || self.home_explorer();
        match vk {
            VK_F if ctrl && has => {
                self.explorer_find();
                return Some(LRESULT(0));
            }
            VK_Z if ctrl && !shift => {
                self.explorer_undo();
                return Some(LRESULT(0));
            }
            VK_Y if ctrl => {
                self.explorer_redo();
                return Some(LRESULT(0));
            }
            VK_B if ctrl && has => {
                self.toggle_browse();
                return Some(LRESULT(0));
            }
            VK_T if ctrl && !shift && self.explorer_shown() => {
                self.new_tab(None);
                return Some(LRESULT(0));
            }
            VK_W if ctrl && self.explorer_shown() && self.explorer.tabs.len() > 1 => {
                let i = self.explorer.active_tab;
                self.close_tab(i);
                return Some(LRESULT(0));
            }
            VK_TAB if ctrl && self.explorer_shown() => {
                self.cycle_tab(shift);
                return Some(LRESULT(0));
            }
            _ => {}
        }
        if !(self.explorer_shown() && self.explorer.focused) {
            return None;
        }
        match vk {
            VK_A if ctrl => self.explorer_select_all(),
            VK_C if ctrl && !shift => self.explorer_clip(false),
            VK_X if ctrl => self.explorer_clip(true),
            VK_V if ctrl => self.explorer_paste(None),
            VK_N if ctrl && shift => self.explorer_new_folder(),
            VK_D if ctrl => self.explorer_favourite(),
            VK_DELETE => self.explorer_delete(),
            VK_F2 => self.explorer_rename_cursor(),
            VK_F5 => self.explorer_refresh(),
            VK_BACK => self.explorer_up(),
            VK_HOME => self.explorer_jump(false),
            VK_END => self.explorer_jump(true),
            VK_ESCAPE => {
                if !self.explorer_escape() {
                    return None;
                }
            }
            // Letters and digits jump to a row (their character comes as WM_CHAR: `explorer_type_ahead`), not
            // the viewer's single-key commands.
            _ if !ctrl && unsafe { GetKeyState(VK_MENU.0 as i32) } >= 0 && ((0x30..=0x39).contains(&vk.0) || (0x41..=0x5A).contains(&vk.0)) => {}
            _ => return None,
        }
        Some(LRESULT(0))
    }
}

/// A column's value for sorting.
#[derive(PartialEq, PartialOrd)]
enum SortKey {
    None,
    Num(f64),
    Text(String),
}

impl Eq for SortKey {}

impl Ord for SortKey {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        match (self, other) {
            (SortKey::Num(a), SortKey::Num(b)) => a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal),
            (SortKey::Text(a), SortKey::Text(b)) => explorer::natural(a, b),
            _ => self.partial_cmp(other).unwrap_or(std::cmp::Ordering::Equal),
        }
    }
}

fn sort_key(r: &Row, c: ColKind, m: Option<&Meta>) -> SortKey {
    let text = |s: Option<String>| s.filter(|s| !s.is_empty()).map_or(SortKey::None, SortKey::Text);
    match c {
        ColKind::Name => SortKey::Text(r.name.clone()),
        ColKind::Extension => text(r.path.extension().map(|e| e.to_string_lossy().to_lowercase()).filter(|_| !r.is_folder())),
        ColKind::Type => SortKey::Text(r.type_name()),
        ColKind::Size => if r.is_folder() { SortKey::None } else { SortKey::Num(r.size as f64) },
        ColKind::Parent => text(r.path.parent().map(explorer::folder_name)),
        ColKind::Path => text(r.path.parent().map(|p| p.to_string_lossy().into_owned())),
        ColKind::Modified => SortKey::Num(r.stamp as f64),
        ColKind::Created => SortKey::Num(r.created as f64),
        ColKind::Taken => m.and_then(|m| m.taken).map_or(SortKey::None, |t| {
            SortKey::Num(((((t.year as u64 * 13 + t.month as u64) * 32 + t.day as u64) * 24 + t.hour as u64) * 60 + t.minute as u64) as f64 * 60.0 + t.second as f64)
        }),
        ColKind::Dimensions | ColKind::Megapixels => m.and_then(|m| m.dims).map_or(SortKey::None, |(w, h)| SortKey::Num(w as f64 * h as f64)),
        ColKind::Camera => text(m.and_then(|m| m.camera.clone())),
        ColKind::Lens => text(m.and_then(|m| m.lens.clone())),
        ColKind::Width => m.and_then(|m| m.dims).map_or(SortKey::None, |(w, _)| SortKey::Num(w as f64)),
        ColKind::Height => m.and_then(|m| m.dims).map_or(SortKey::None, |(_, h)| SortKey::Num(h as f64)),
        ColKind::AspectRatio => m.and_then(|m| m.dims).filter(|d| d.1 > 0).map_or(SortKey::None, |(w, h)| SortKey::Num(w as f64 / h as f64)),
        ColKind::Duration => m.and_then(|m| m.duration_us).map_or(SortKey::None, |d| SortKey::Num(d as f64)),
        c => match m.and_then(|m| m.field(c)) {
            None => SortKey::None,
            Some(t) if c.right_aligned() => sort_number(c, t).map_or(SortKey::Text(t.to_string()), SortKey::Num),
            Some(t) => SortKey::Text(t.to_string()),
        },
    }
}

/// Folders in Transient's yellow.
pub(super) const FOLDER_TINT: u32 = 0xE8B64C;
