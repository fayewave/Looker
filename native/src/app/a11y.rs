//! Screen readers: what the window tells UI Automation about itself (uia.rs has the providers). After each frame,
//! while a reader listens, the hit regions the frame recorded become `uia::Node`s: each named from what it is
//! (a toolbar button by its tooltip, a menu item by its label, a setting by its row's label) or else from the
//! text the frame drew inside it (`Gfx::texts`), with its role and state; the node the keyboard is on is the
//! focus. Changes are raised as events, so readers follow the arrow keys, and toasts are spoken. A reader's
//! press clicks the node's middle, as the pointer would.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Accessibility::*;

use windows::core::BSTR;

use super::uia::{DOC_ID, Node, Uia};
use super::*;

/// What a hit is, for a reader.
#[derive(Default)]
struct Info {
    role: i32,
    name: String,
    help: String,
    accel: String,
    value: Option<String>,
    editable: bool,
    toggle: Option<bool>,
    selected: Option<bool>,
    enabled: bool,
    focusable: bool,
    invoke: bool,
}

impl Info {
    fn new(role: UIA_CONTROLTYPE_ID, name: impl Into<String>) -> Info {
        Info { role: role.0, name: name.into(), enabled: true, invoke: true, ..Default::default() }
    }
    fn button(name: impl Into<String>) -> Info {
        Info::new(UIA_ButtonControlTypeId, name)
    }
    fn item(role: UIA_CONTROLTYPE_ID, name: impl Into<String>, selected: bool) -> Info {
        Info { selected: Some(selected), focusable: true, ..Info::new(role, name) }
    }
    fn edit(name: impl Into<String>, value: String) -> Info {
        Info { value: Some(value), editable: true, focusable: true, ..Info::new(UIA_EditControlTypeId, name) }
    }
    fn text(name: impl Into<String>) -> Info {
        Info { invoke: false, ..Info::new(UIA_TextControlTypeId, name) }
    }
    fn help(mut self, h: impl Into<String>) -> Info {
        self.help = h.into();
        self
    }
}

/// The text editor's node (textview.rs fills it).
pub(super) struct DocInfo {
    pub name: String,
}

/// State only the drawing knows (a switch's position, a chip's pick), noted as it draws.
#[derive(Clone, Copy)]
pub(super) enum Mark {
    Toggle(bool),
    Selected(bool),
}

/// A hit's node id: its hash, steady while it's the same thing, never `DOC_ID`.
fn node_id(h: Hit) -> i32 {
    let mut s = std::collections::hash_map::DefaultHasher::new();
    h.hash(&mut s);
    ((s.finish() as u32 & 0x7FFF_FFFF) as i32).max(DOC_ID + 1)
}

/// A tooltip's text without its shortcut: "Rotate right (Ctrl+R)" -> ("Rotate right", "Ctrl+R").
fn split_accel(tip: &str) -> (String, String) {
    if let Some(open) = tip.rfind(" (") {
        if tip.ends_with(')') {
            return (tip[..open].trim_end().to_string(), tip[open + 2..tip.len() - 1].to_string());
        }
    }
    (tip.trim().to_string(), String::new())
}

fn centre_in(r: &D2D_RECT_F, t: &D2D_RECT_F) -> bool {
    let (cx, cy) = ((t.left + t.right) / 2.0, (t.top + t.bottom) / 2.0);
    cx >= r.left && cx <= r.right && cy >= r.top && cy <= r.bottom
}

fn clock(us: i64) -> String {
    let s = (us.max(0) / 1_000_000) as u64;
    if s >= 3600 { format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60) } else { format!("{}:{:02}", s / 60, s % 60) }
}

/// The texts drawn inside `r`, in reading order, joined.
fn texts_in(texts: &[(D2D_RECT_F, String)], r: &D2D_RECT_F) -> String {
    let mut inside: Vec<&(D2D_RECT_F, String)> = texts.iter().filter(|(t, s)| !s.trim().is_empty() && centre_in(r, t)).collect();
    inside.sort_by(|a, b| (a.0.top as i32, a.0.left as i32).cmp(&(b.0.top as i32, b.0.left as i32)));
    let mut out: Vec<&str> = Vec::new();
    for (_, s) in inside {
        let s = s.trim();
        if out.last() != Some(&s) {
            out.push(s);
        }
    }
    out.join(", ")
}

/// The label left of `r` on its row (a setting's name beside its drop-down).
fn label_left_of(texts: &[(D2D_RECT_F, String)], r: &D2D_RECT_F) -> Option<String> {
    texts
        .iter()
        .filter(|(t, s)| !s.trim().is_empty() && t.right <= r.left + 2.0 && t.top <= r.bottom + 4.0 && t.bottom >= r.top - 4.0 && r.left - t.right < 900.0)
        // The nearest column of text (the Settings sidebar is further left), its top line (the hint is under it).
        .min_by(|a, b| ((-a.0.left) as i32, a.0.top as i32).cmp(&((-b.0.left) as i32, b.0.top as i32)))
        .map(|(_, s)| s.trim().to_string())
}

impl App {
    /// WM_GETOBJECT: the window's root for UI Automation (made the first time a reader asks).
    pub(super) fn uia_object(&mut self, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
        if lp.0 as i32 != UiaRootObjectId {
            return None;
        }
        if self.uia.is_none() {
            self.uia = Some(Uia::new(self.hwnd));
            self.invalidate(); // the next frame records its texts and publishes the nodes
        }
        self.text_uia_sync();
        self.uia.as_ref().map(|u| u.answer(self.hwnd, wp, lp))
    }

    /// Whether this frame should note what it draws for a reader.
    pub(super) fn a11y_recording(&self) -> bool {
        self.uia.is_some() && Uia::listening()
    }

    /// Notes state only the drawing knows, while a reader listens.
    pub(super) fn a11y_mark(&mut self, h: Hit, m: Mark) {
        if self.uia.is_some() {
            self.a11y_marks.push((h, m));
        }
    }

    /// Said by the reader, if one listens (toasts).
    pub(super) fn a11y_say(&self, text: &str) {
        if let Some(u) = &self.uia {
            if Uia::listening() {
                u.notify(text);
            }
        }
    }

    /// After a frame: the hits it recorded and the texts it drew become the reader's nodes; what changed is raised.
    /// `at_page`, `at_overlays`: how many texts were drawn before the Settings card and before the overlays
    /// (dialogs): under a modal one, only what it drew names things.
    pub(super) fn a11y_publish(&mut self, mut texts: Vec<(D2D_RECT_F, String)>, at_page: usize, at_overlays: usize) {
        if self.uia.is_none() {
            return;
        }
        if self.dialog.is_some() {
            texts.drain(..at_overlays.min(texts.len()));
        } else if self.page_open() {
            texts.drain(..at_page.min(texts.len()));
        }
        // Icons are glyphs of Segoe Fluent's private use area: not words.
        let texts: Vec<(D2D_RECT_F, String)> = texts
            .into_iter()
            .map(|(r, t)| (r, t.chars().filter(|c| !('\u{E000}'..='\u{F8FF}').contains(c)).collect::<String>().trim().trim_end_matches(',').trim().to_string()))
            .filter(|(_, t)| !t.is_empty())
            .collect();
        let s = self.scale() as f64;
        let mut origin = POINT::default();
        unsafe {
            let _ = ClientToScreen(self.hwnd, &mut origin);
        }
        let marks: HashMap<Hit, Mark> = self.a11y_marks.drain(..).collect();
        let mut items: Vec<(Hit, D2D_RECT_F)> = self.hits.items().to_vec();
        // Under a modal surface (Settings, a dialog, the colour picker) nothing is there for a reader.
        if let Some(i) = items.iter().rposition(|(h, _)| matches!(h, Hit::PageSurface | Hit::DialogSurface | Hit::PickerOutside)) {
            items.drain(..i);
        }
        let sidebar: Vec<String> = if items.iter().any(|(h, _)| matches!(h, Hit::SideRow(_))) { self.sidebar_items().into_iter().map(|i| i.label).collect() } else { Vec::new() };
        let mut nodes: Vec<Node> = Vec::new();
        let mut hits: Vec<(i32, Hit)> = Vec::new();
        let px = |r: &D2D_RECT_F| [origin.x as f64 + r.left as f64 * s, origin.y as f64 + r.top as f64 * s, (r.right - r.left) as f64 * s, (r.bottom - r.top) as f64 * s];
        // Top first is how hits are found; readers want reading order, which the drawing order mostly is.
        for (h, r) in &items {
            let info = if *h == Hit::TextView {
                self.text_doc_info().map(|d| Info { focusable: true, invoke: false, ..Info::new(UIA_DocumentControlTypeId, d.name) })
            } else {
                self.a11y_info(*h, r, &texts, &marks, &sidebar)
            };
            let Some(mut info) = info else { continue };
            if let Some(m) = marks.get(h) {
                match m {
                    Mark::Toggle(on) => info.toggle = Some(*on),
                    Mark::Selected(on) => info.selected = Some(*on),
                }
            }
            if info.name.trim().is_empty() {
                continue;
            }
            let id = if *h == Hit::TextView { DOC_ID } else { node_id(*h) };
            if hits.iter().any(|(i, _)| *i == id) {
                continue; // drawn twice (a hit added again on top): one node
            }
            hits.push((id, *h));
            nodes.push(Node {
                id,
                role: info.role,
                name: info.name,
                auto: format!("{h:?}"),
                help: info.help,
                accel: info.accel,
                value: info.value,
                editable: info.editable,
                toggle: info.toggle,
                selected: info.selected,
                enabled: info.enabled,
                focusable: info.focusable,
                invoke: info.invoke,
                rect: px(r),
            });
        }
        // The open dialog's words, before its buttons.
        if let Some((_, d)) = &self.dialog {
            if let Some(i) = nodes.iter().position(|n| n.auto.starts_with("Dialog")) {
                let words = if d.body.is_empty() { d.title.clone() } else { format!("{}. {}", d.title, d.body) };
                let r = nodes[i].rect;
                nodes.insert(i, Node { id: node_id(Hit::DialogSurface), role: UIA_TextControlTypeId.0, name: words, auto: "DialogText".into(), enabled: true, rect: r, ..Default::default() });
            }
        }
        let focus = self.a11y_focus().map(|h| if h == Hit::TextView { DOC_ID } else { node_id(h) }).filter(|id| nodes.iter().any(|n| n.id == *id));

        let u = self.uia.as_ref().unwrap();
        let (old, old_focus) = {
            let mut t = u.tree.lock().unwrap_or_else(|e| e.into_inner());
            let old = std::mem::replace(&mut t.list, nodes.clone());
            let of = t.focus;
            t.focus = focus;
            (old, of)
        };
        self.a11y_hits = hits;
        if !Uia::listening() {
            return;
        }
        let ids = |l: &[Node]| l.iter().map(|n| n.id).collect::<Vec<_>>();
        if ids(&old) != ids(&nodes) {
            u.structure_changed();
        }
        // A dialog that just came up: its words are said.
        if let Some(d) = nodes.iter().find(|n| n.auto == "DialogText") {
            if !old.iter().any(|n| n.auto == "DialogText") {
                u.notify(&d.name);
            }
        }
        for n in &nodes {
            let Some(o) = old.iter().find(|o| o.id == n.id) else { continue };
            if o.toggle != n.toggle {
                let st = |v: Option<bool>| VARIANT::from(if v == Some(true) { ToggleState_On.0 } else { ToggleState_Off.0 });
                u.changed(n.id, UIA_ToggleToggleStatePropertyId, st(o.toggle), st(n.toggle));
            }
            if o.selected != n.selected {
                u.changed(n.id, UIA_SelectionItemIsSelectedPropertyId, VARIANT::from(o.selected == Some(true)), VARIANT::from(n.selected == Some(true)));
                if n.selected == Some(true) {
                    u.raise_on(n.id, UIA_SelectionItem_ElementSelectedEventId);
                }
            }
            if o.name != n.name && n.id != DOC_ID {
                u.changed(n.id, UIA_NamePropertyId, VARIANT::from(BSTR::from(o.name.as_str())), VARIANT::from(BSTR::from(n.name.as_str())));
            }
            if n.editable && o.value != n.value {
                let v = |x: &Option<String>| VARIANT::from(BSTR::from(x.as_deref().unwrap_or("")));
                u.changed(n.id, UIA_ValueValuePropertyId, v(&o.value), v(&n.value));
            }
        }
        if focus != old_focus {
            if let Some(f) = focus {
                u.raise_on(f, UIA_AutomationFocusChangedEventId);
            }
        } else if let Some(f) = focus {
            // The photo showing changed under the focus (the arrow keys): its name is said.
            if let (Some(o), Some(n)) = (old.iter().find(|o| o.id == f), nodes.iter().find(|n| n.id == f)) {
                if o.name != n.name && n.auto == "Viewport" {
                    u.notify(&n.name);
                }
            }
        }
    }

    /// A reader pressed node `id`: it's clicked in its middle, as the pointer would.
    pub(super) fn a11y_invoke(&mut self, id: i32) {
        let Some(h) = self.a11y_hits.iter().find(|(i, _)| *i == id).map(|(_, h)| *h) else { return };
        let Some(r) = self.hits.rect_of(h) else { return };
        let s = self.scale();
        let (x, y) = (((r.left + r.right) / 2.0 * s) as i32, ((r.top + r.bottom) / 2.0 * s) as i32);
        let lp = LPARAM(((y as u16 as u32) << 16 | x as u16 as u32) as isize);
        // Handled here and now, not posted: the real pointer is elsewhere, and its WM_MOUSELEAVE landing between a
        // posted press and release lost the click.
        unsafe {
            self.handle(WM_MOUSEMOVE, WPARAM(0), lp);
            self.handle(WM_LBUTTONDOWN, WPARAM(0x0001), lp);
            self.handle(WM_LBUTTONUP, WPARAM(0), lp);
        }
    }

    /// The hit the keyboard is on.
    fn a11y_focus(&self) -> Option<Hit> {
        if let Some(e) = &self.edit {
            if self.dialog.is_none() && self.menu.is_none() {
                return Some(e.focus.unwrap_or(Hit::Viewport));
            }
        }
        if let Some((_, m)) = &self.menu {
            if let Some((_, sub)) = &m.sub {
                if let Some(c) = sub.cursor {
                    return Some(Hit::MenuItem(ui::MENU_SUB + c));
                }
            }
            let first = m.entries.iter().position(|e| matches!(e, ui::Entry::Item(it) if it.enabled) || matches!(e, ui::Entry::Sub(..)));
            return m.cursor.or(first).map(Hit::MenuItem);
        }
        if let Some((_, d)) = &self.dialog {
            return Some(match d.focus {
                ui::Focus::Field => Hit::DialogField,
                ui::Focus::Button(i) => Hit::DialogButton(i),
                ui::Focus::Choices(l) => Hit::DialogChoice(l, d.choices.get(l).map_or(0, |c| c.selected)),
            });
        }
        if self.page_open() {
            return self.page_section_shown().map(|i| Hit::PageNav(i as u8));
        }
        if self.explorer.rename.is_some() {
            return Some(Hit::ExplorerRename);
        }
        match self.explorer.field {
            Some(explorer::EField::Filter) => return Some(Hit::ExplorerFilter),
            Some(explorer::EField::Path) => return Some(Hit::ExplorerPath),
            _ => {}
        }
        if self.explorer.focused {
            if let Some(i) = self.explorer.cursor.as_ref().and_then(|c| self.explorer.view.iter().position(|&r| self.explorer.rows.get(r).is_some_and(|row| &row.path == c))) {
                return Some(Hit::ExplorerRow(i));
            }
            return None;
        }
        if let Some(h) = self.text_a11y_focus() {
            return Some(h);
        }
        Some(Hit::Viewport)
    }

    /// What hit `h` (drawn at `r`) is, for a reader; None for what isn't for reading (surfaces, grips, edges).
    fn a11y_info(&mut self, h: Hit, r: &D2D_RECT_F, texts: &[(D2D_RECT_F, String)], marks: &HashMap<Hit, Mark>, sidebar: &[String]) -> Option<Info> {
        let inside = || texts_in(texts, r);
        let tip = |me: &mut App| me.tooltip_for(h).map(|t| split_accel(&t));
        let named = |me: &mut App, fallback: &str| -> Info {
            match tip(me) {
                Some((name, accel)) => Info { accel, ..Info::button(name) },
                None => {
                    let t = texts_in(texts, r);
                    Info::button(if t.is_empty() { fallback.to_string() } else { t })
                }
            }
        };
        Some(match h {
            // Surfaces, grips, edges, scrollbars, fades: nothing to say.
            Hit::PageSurface
            | Hit::PageCard
            | Hit::PageNavOn(_)
            | Hit::StyleRowOn(_)
            | Hit::StyleFooter
            | Hit::PageThumb
            | Hit::ToggleAnim(_)
            | Hit::PickerOutside
            | Hit::PickerCard
            | Hit::MenuSurface
            | Hit::Strip
            | Hit::StripGrip
            | Hit::StripFadeLeft
            | Hit::StripFadeRight
            | Hit::ExplorerCard
            | Hit::ExplorerList
            | Hit::ExplorerGrip
            | Hit::CrumbArea
            | Hit::CrumbPopup
            | Hit::ColEdge(_)
            | Hit::ColumnPane(_)
            | Hit::ColumnEdge(_)
            | Hit::ColumnHScroll
            | Hit::ExplorerThumb
            | Hit::Sidebar
            | Hit::SidebarGrip
            | Hit::SidebarColumn
            | Hit::RailFlyout
            | Hit::StickyGuide
            | Hit::RowSolid(_)
            | Hit::InfoCard
            | Hit::InfoGrip
            | Hit::VideoBar
            | Hit::ArchiveCard
            | Hit::ArchiveList
            | Hit::ArchiveSolid(_)
            | Hit::AudioScroll
            | Hit::TextScroll
            | Hit::TextHScroll
            | Hit::TextBar
            | Hit::PageBar
            | Hit::Caption(_)
            | Hit::DialogSurface
            | Hit::TextView => return None,

            Hit::Viewport => {
                let parts = self.status_parts();
                if parts.is_empty() {
                    return None;
                }
                let mut words: Vec<String> = Vec::new();
                if let Some(p) = self.current_path() {
                    words.push(file_name(p));
                }
                if let (Some(i), Some(l)) = (self.viewer.index, self.viewer.listing.as_ref()) {
                    words.push(format!("{} of {}", i + 1, l.images.len()));
                }
                words.extend(parts.into_iter().map(|p| p.1));
                let name = words.join(", ");
                let role = if self.video_shown() { UIA_PaneControlTypeId } else { UIA_ImageControlTypeId };
                Info { focusable: true, invoke: false, ..Info::new(role, name) }
            }
            Hit::Open => Info::button(format!("Zoom {}", inside())).help("Fit or 100 %"),
            Hit::Tool(Tool::Zoom) => Info { value: Some(inside()), ..Info::button("Zoom") }.help("Fit or 100 %; drag left or right to zoom"),
            Hit::TabNew | Hit::TabClose(_) => match self.explorer_tooltip(h) {
                Some(t) => {
                    let (name, accel) = split_accel(&t);
                    Info { accel, ..Info::button(name) }
                }
                None => Info::button(if h == Hit::TabNew { "New tab" } else { "Close tab" }),
            },
            Hit::Tool(_) | Hit::Reveal | Hit::InfoPath | Hit::InfoEdit(_) | Hit::InfoSave | Hit::InfoDiscard | Hit::InfoAdd => named(self, "Button"),
            Hit::PagePrevious | Hit::PageNext | Hit::DefaultHintClose | Hit::VideoPlay | Hit::VideoStop | Hit::VideoMute | Hit::VideoSettings | Hit::VideoTime | Hit::VideoFullscreen | Hit::MotionBadge => {
                let mut i = named(self, "Button");
                if h == Hit::VideoPlay {
                    let playing = self.player_ref().is_some_and(|p| p.playing());
                    i.name = if playing { "Pause".into() } else { "Play".into() };
                    i.accel = "Space".into();
                }
                if h == Hit::VideoMute {
                    i.toggle = Some(self.settings.video_muted);
                }
                i
            }
            Hit::AudioPrev | Hit::AudioNext | Hit::AudioLoop | Hit::TextButton(_) | Hit::TextFoot(_) | Hit::TextTabNew | Hit::TextTabClose(_) => named(self, "Button"),
            Hit::ExplorerBack | Hit::ExplorerForward | Hit::ExplorerUp | Hit::ExplorerHistory | Hit::ExplorerBookmarks | Hit::ExplorerCollections | Hit::ExplorerGroup | Hit::ExplorerLayout | Hit::ExplorerBrowse | Hit::CrumbMore => named(self, "Button"),
            Hit::RailIcon(_) => named(self, "Section"),
            Hit::PageClose => Info::button("Close Settings").help("Esc"),
            Hit::PageLink(_) | Hit::PickerDone | Hit::PickerCancel | Hit::LandingFolder | Hit::RecentClear | Hit::GetExtension | Hit::DefaultHint | Hit::ArchiveBack | Hit::ArchiveForward | Hit::ArchiveUp | Hit::ArchiveExtractAll | Hit::ArchiveExtractTo | Hit::ArchiveCancel | Hit::AudioClear | Hit::RipCancel | Hit::ExtractCancel | Hit::CrumbPopupUp | Hit::CrumbPopupClose | Hit::PathClear | Hit::ExplorerFilterClear | Hit::DialogFieldClear => {
                let fallback = match h {
                    Hit::ArchiveBack => "Back",
                    Hit::ArchiveForward => "Forward",
                    Hit::ArchiveUp | Hit::CrumbPopupUp => "Up",
                    Hit::AudioClear => "Clear the loop",
                    Hit::CrumbPopupClose => "Close",
                    Hit::PathClear | Hit::ExplorerFilterClear | Hit::DialogFieldClear => "Clear",
                    Hit::RipCancel | Hit::ExtractCancel | Hit::ArchiveCancel | Hit::PickerCancel => "Cancel",
                    _ => "Button",
                };
                let t = inside();
                Info::button(if t.is_empty() { fallback.to_string() } else { t })
            }
            Hit::TextSizeStep(k) => Info::button(if k == 0 { "Smaller text" } else { "Larger text" }),
            Hit::StyleRow(i) => Info::item(UIA_ListItemControlTypeId, self.settings.subtitle_styles.get(i as usize).map_or_else(inside, |s| s.name.clone()), matches!(marks.get(&h), Some(Mark::Selected(true)))),
            Hit::StyleScene(_) => Info::item(UIA_RadioButtonControlTypeId, inside(), matches!(marks.get(&h), Some(Mark::Selected(true)))),
            Hit::StyleAct(_) | Hit::StyleColour(_) => named(self, "Button"),
            Hit::StyleStep(_, k) => {
                // Named after its row's label (the text left of it on the same line).
                let row = texts.iter().filter(|(t, s)| !s.trim().is_empty() && t.right <= r.left && t.top < r.bottom && t.bottom > r.top).map(|(_, s)| s.trim().to_string()).next().unwrap_or_default();
                match k {
                    0 => Info::button(format!("Less {}", row.to_lowercase())),
                    1 => Info::button(format!("More {}", row.to_lowercase())),
                    _ => Info { value: Some(inside()), ..Info::new(UIA_TextControlTypeId, row) },
                }
            }
            Hit::ModelCube => Info::button("View cube").help("Click a face to look from that side"),
            Hit::PageNav(i) => Info::item(UIA_TabItemControlTypeId, inside(), self.page_section_shown() == Some(i as usize)),
            Hit::Toggle(_) => {
                let mut i = Info::new(UIA_CheckBoxControlTypeId, String::new());
                let mut words = texts.iter().filter(|(t, s)| !s.trim().is_empty() && centre_in(r, t)).map(|(_, s)| s.trim().to_string());
                i.name = words.next().unwrap_or_default();
                i.help = words.collect::<Vec<_>>().join(" ");
                i.focusable = true;
                if !marks.contains_key(&h) {
                    i.toggle = Some(false);
                }
                i
            }
            Hit::Combo(_) => {
                let label = label_left_of(texts, r).unwrap_or_else(|| "Choice".into());
                Info { value: Some(inside()), focusable: true, ..Info::new(UIA_ComboBoxControlTypeId, label) }
            }
            Hit::ThemeChip(_) | Hit::AccentChip(_) | Hit::PickerPreset(_) => {
                let t = inside();
                let name = if t.is_empty() { "Colour".to_string() } else { t };
                Info::item(UIA_RadioButtonControlTypeId, name, false)
            }
            Hit::PickerSv => Info { invoke: false, ..Info::new(UIA_SliderControlTypeId, "Saturation and brightness") },
            Hit::PickerHue => Info { invoke: false, ..Info::new(UIA_SliderControlTypeId, "Hue") },
            Hit::PickerHex => Info::edit("Hex colour", inside()),
            Hit::RecentRow(i) => {
                let name = self.recent_path(i).map(|p| file_name(&p)).unwrap_or_else(|| inside());
                Info::item(UIA_ListItemControlTypeId, name, false).help(inside())
            }
            Hit::MenuItem(i) => {
                let entries = match &self.menu {
                    Some((_, m)) if i >= ui::MENU_SUB => &m.sub.as_ref()?.1.entries,
                    Some((_, m)) => &m.entries,
                    None => return None,
                };
                let k = if i >= ui::MENU_SUB { i - ui::MENU_SUB } else { i };
                match entries.get(k)? {
                    ui::Entry::Item(it) => {
                        let mut info = Info::new(UIA_MenuItemControlTypeId, it.label.clone());
                        info.accel = it.accel.unwrap_or("").to_string();
                        info.toggle = it.check;
                        info.enabled = it.enabled;
                        info.focusable = true;
                        info
                    }
                    ui::Entry::Sub(_, label, _) => Info { focusable: true, ..Info::new(UIA_MenuItemControlTypeId, format!("{label}, submenu")) },
                    ui::Entry::Stepper(st) => Info::text(format!("{} {}", st.label, st.value)),
                    ui::Entry::Heading(t) => Info::text(t.clone()),
                    _ => return None,
                }
            }
            Hit::MenuSwatch(_, k) => Info::button(format!("Colour tag {}", k + 1)),
            Hit::DialogButton(i) => {
                let (_, d) = self.dialog.as_ref()?;
                let b = d.buttons.get(i)?;
                Info { enabled: b.enabled, focusable: true, ..Info::button(b.label.clone()) }
            }
            Hit::DialogField => {
                let (_, d) = self.dialog.as_ref()?;
                let name = d.field_label.clone().unwrap_or_else(|| d.title.clone());
                Info::edit(name, d.field.as_ref().map(|f| f.edit.text()).unwrap_or_default())
            }
            Hit::DialogChoice(l, o) => {
                let (_, d) = self.dialog.as_ref()?;
                let c = d.choices.get(l)?;
                let (name, detail) = c.rows.get(o)?;
                Info::item(UIA_RadioButtonControlTypeId, name.clone(), c.selected == o).help(detail.clone())
            }
            Hit::StripCell(i) => {
                let name = self.strip_tooltip(i)?;
                Info::item(UIA_ListItemControlTypeId, name, self.strip_selected() == Some(i))
            }
            Hit::ExplorerRow(i) => {
                let row = self.explorer.view.get(i).and_then(|&k| self.explorer.rows.get(k))?;
                let selected = self.explorer.selected.contains(&crate::explorer::norm(&row.path));
                let detail = inside();
                Info::item(UIA_ListItemControlTypeId, row.name.clone(), selected).help(detail)
            }
            Hit::ColumnRow(..) | Hit::ArchiveRow(_) | Hit::CrumbPopupRow(_) => Info::item(UIA_ListItemControlTypeId, inside(), false),
            Hit::Crumb(_) | Hit::ArchiveCrumb(_) => {
                let t = inside();
                Info::button(if t.is_empty() { tip(self).map(|t| t.0).unwrap_or_default() } else { t })
            }
            Hit::CrumbChevron(_) => Info::button("Folders in here"),
            Hit::ColHeader(_) => Info::new(UIA_HeaderItemControlTypeId, inside()),
            Hit::Tab(i) => {
                let name = tip(self).map(|t| t.0).filter(|t| !t.is_empty()).unwrap_or_else(|| inside());
                Info::item(UIA_TabItemControlTypeId, name, self.explorer.active_tab == i as usize)
            }
            Hit::TextTab(i) => Info::item(UIA_TabItemControlTypeId, inside(), self.text.active == i as usize),
            Hit::TextMenuBar(_) => Info { focusable: true, ..Info::new(UIA_MenuItemControlTypeId, inside()) },
            Hit::SideRow(i) => Info::item(UIA_TreeItemControlTypeId, sidebar.get(i).cloned().unwrap_or_else(|| inside()), false),
            Hit::SideHeader(_) => Info::button(inside()),
            Hit::SideChevron(_) => Info::button("Expand or collapse"),
            Hit::ExplorerFilter => Info::edit("Filter", self.explorer.filter.edit.text()),
            Hit::ExplorerRename => Info::edit("New name", self.explorer.rename.as_ref().map(|(_, f)| f.edit.text()).unwrap_or_default()),
            Hit::ExplorerPath => Info::edit("Path", inside()),
            Hit::InfoRow(_) => Info::text(inside()).help("Click to copy"),
            Hit::InfoField => Info::edit("Field", inside()),
            Hit::InfoStar(k) => Info::button(format!("{} star{}", k + 1, if k == 0 { "" } else { "s" })),
            Hit::TextField(k) => Info::edit(["Find", "Replace", "Go to line"].get(k as usize).copied().unwrap_or("Text"), inside()),
            Hit::VideoSeek => {
                let (pos, dur) = self.player_ref().map_or((0, 0), |p| (p.position(), p.duration()));
                Info { value: Some(format!("{} of {}", clock(pos), clock(dur))), invoke: false, ..Info::new(UIA_SliderControlTypeId, "Seek") }
            }
            Hit::VideoVolume => Info { value: Some(format!("{:.0} %", self.settings.video_volume * 100.0)), invoke: false, ..Info::new(UIA_SliderControlTypeId, "Volume") },
            Hit::AudioWave => Info { invoke: false, ..Info::new(UIA_ImageControlTypeId, "Waveform") },
            Hit::AudioMode(k) => Info::item(UIA_RadioButtonControlTypeId, if k == 0 { "Waveform" } else { "Spectrogram" }, (k == 1) == self.settings.audio_spectrum),
            Hit::EditPanel => return None,
            Hit::EditCrop(part) => {
                let e = self.edit.as_ref()?;
                let (w, h) = (e.p.crop[2] - e.p.crop[0], e.p.crop[3] - e.p.crop[1]);
                let names = ["Crop frame", "Top left corner", "Top edge", "Top right corner", "Right edge", "Bottom right corner", "Bottom edge", "Bottom left corner", "Left edge"];
                let name = names.get(part as usize).copied().unwrap_or("Crop");
                if part != 0 {
                    return None; // the frame itself speaks for its handles
                }
                Info { value: Some(format!("{:.0} × {:.0} pixels", w, h)), invoke: false, focusable: false, ..Info::new(UIA_PaneControlTypeId, name) }.help("Drag inside to move the photo, outside to turn it; the arrow keys nudge it")
            }
            Hit::EditAspect(i) => Info::item(UIA_RadioButtonControlTypeId, format!("Crop {}", super::edit::aspect_name(i)), false),
            Hit::EditOrient(i) => Info { focusable: true, ..Info::item(UIA_RadioButtonControlTypeId, if i == 0 { "Landscape" } else { "Portrait" }, false) },
            Hit::EditRotate => return None,
            Hit::EditStraighten => {
                let a = self.edit.as_ref().map_or(0.0, |e| e.p.angle);
                Info { value: Some(super::edit::degrees_text(a)), invoke: false, focusable: true, ..Info::new(UIA_SliderControlTypeId, "Straighten") }.help("Left and right arrows: 0.1 degree, Shift: 1")
            }
            Hit::EditExposure => {
                let v = self.edit.as_ref().map_or(0.0, |e| e.p.ev);
                Info { value: Some(format!("{} stops", super::edit::ev_text(v))), invoke: false, focusable: true, ..Info::new(UIA_SliderControlTypeId, "Exposure") }.help("Left and right arrows: 0.1 stop, Shift: 0.5")
            }
            Hit::EditReset | Hit::EditCancel | Hit::EditSave | Hit::EditSaveCopy => {
                let name = match h {
                    Hit::EditReset => "Reset",
                    Hit::EditCancel => "Cancel",
                    Hit::EditSave => "Save over the photo",
                    _ => "Save a copy",
                };
                let mut i = Info { focusable: true, ..Info::button(name) };
                i.accel = match h {
                    Hit::EditCancel => "Esc",
                    Hit::EditSave => "Ctrl+S",
                    Hit::EditSaveCopy => "Enter",
                    _ => "",
                }
                .into();
                if h == Hit::EditSave {
                    i.help = "The original goes to the Recycle Bin".into();
                }
                i
            }
            Hit::RipPill | Hit::ExtractPill => Info { invoke: false, ..Info::new(UIA_ProgressBarControlTypeId, inside()) },
        })
    }
}
