//! Screen readers (Narrator, NVDA, JAWS) through UI Automation. Looker draws everything itself, so there are no
//! controls for UI Automation to find: the window answers WM_GETOBJECT with a fragment root of its own, whose
//! children are `Node`s: one for each thing the last frame drew that can be pressed, picked or read (a toolbar
//! button, a menu item, an explorer row, a setting's switch, the photo), rebuilt by the window from its hit
//! regions after each frame while a client listens (app/a11y.rs). Each has a role, a name, its state, its place
//! on screen, and the patterns its role calls for: Invoke, Toggle and SelectionItem (all of which click it, as
//! the pointer would: the window does that), Value (a text box's text, a slider's reading). The node the keyboard
//! is on is the focus, and moving it raises the focus event, so readers follow the arrow keys through the
//! explorer, menus and dialogs. Toasts are spoken as notifications.
//!
//! While a text file is up one node is the editor: a Document with UI Automation's text pattern, as Notepad
//! offers it: it reads the document by character, word, line and the whole; follows the caret and selection
//! (TextSelectionChanged as they move, TextChanged on edits); and lets the reader select text.
//!
//! The providers run on UI Automation's threads and read copies (`Doc`, `Nodes`) that the window refreshes as
//! they change, only while some client is listening (`UiaClientsAreListening`).

#![allow(non_upper_case_globals)]

use std::sync::{Arc, Mutex, MutexGuard};

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::SAFEARRAY;
use windows::Win32::System::Ole::{SafeArrayCreateVector, SafeArrayPutElement};
use windows::Win32::System::Variant::{VARIANT, VT_I4, VT_R8, VT_UNKNOWN};
use windows::Win32::UI::Accessibility::*;
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};
use windows::core::{AsImpl, BOOL, BSTR, IUnknown, IUnknownImpl, Interface, PCWSTR, Ref, Result, implement};

/// Posted when a reader selects text: the window picks the range up from `Doc::select`.
pub(super) const WM_UIA_SELECT: u32 = WM_APP + 31;
/// Posted when a reader presses, toggles or picks a node (wparam: its id): the window clicks it.
pub(super) const WM_UIA_INVOKE: u32 = WM_APP + 32;

/// The text editor's node.
pub(super) const DOC_ID: i32 = 1;

const UIA_E_ELEMENTNOTAVAILABLE: windows::core::HRESULT = windows::core::HRESULT(0x8004_0201_u32 as i32);

#[derive(Default)]
pub(super) struct Doc {
    pub text: Vec<u16>,
    pub sel: (usize, usize),
    pub name: String,
    pub focused: bool,
    /// A selection the reader asked for, waiting for the window.
    pub select: Option<(usize, usize)>,
}

pub(super) type Shared = Arc<Mutex<Doc>>;

/// One element: what a reader is told about something on screen.
#[derive(Clone, PartialEq, Default, Debug)]
pub(super) struct Node {
    /// Stable while it's the same thing (from its hit), and never `DOC_ID` unless it's the editor.
    pub id: i32,
    /// UIA_*ControlTypeId.
    pub role: i32,
    pub name: String,
    /// The hit's name (`Tool(Next)`): UI Automation's AutomationId, for scripts and tests.
    pub auto: String,
    /// A longer description (a tooltip, a setting's hint).
    pub help: String,
    /// Its shortcut ("Ctrl+R").
    pub accel: String,
    /// A text box's text or a slider's reading.
    pub value: Option<String>,
    /// The value can be typed into (a text box).
    pub editable: bool,
    /// A switch or tick: on or off.
    pub toggle: Option<bool>,
    /// A row, tab or choice: picked or not.
    pub selected: Option<bool>,
    pub enabled: bool,
    pub focusable: bool,
    /// Pressing it does something.
    pub invoke: bool,
    /// On screen: left, top, width, height in pixels.
    pub rect: [f64; 4],
}

#[derive(Default)]
pub(super) struct Nodes {
    pub list: Vec<Node>,
    pub focus: Option<i32>,
}

pub(super) type Tree = Arc<Mutex<Nodes>>;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The window's end: the root, and what it reads.
pub(super) struct Uia {
    pub shared: Shared,
    pub tree: Tree,
    pub root: IRawElementProviderSimple,
    hwnd: isize,
    /// What was last handed over: the document's id and version, and the selection.
    pub synced: (u64, u64, (usize, usize), bool),
}

impl Uia {
    pub(super) fn new(hwnd: HWND) -> Uia {
        let shared: Shared = Arc::new(Mutex::new(Doc::default()));
        let tree: Tree = Arc::new(Mutex::new(Nodes::default()));
        let root: IRawElementProviderSimple = Root { hwnd: hwnd.0 as isize, shared: shared.clone(), tree: tree.clone() }.into();
        Uia { shared, tree, root, hwnd: hwnd.0 as isize, synced: (0, u64::MAX, (usize::MAX, 0), false) }
    }

    /// Answers WM_GETOBJECT for the window's root.
    pub(super) fn answer(&self, hwnd: HWND, wp: WPARAM, lp: LPARAM) -> windows::Win32::Foundation::LRESULT {
        unsafe { UiaReturnRawElementProvider(hwnd, wp, lp, &self.root) }
    }

    pub(super) fn listening() -> bool {
        unsafe { UiaClientsAreListening().as_bool() }
    }

    /// The element for node `id`.
    pub(super) fn elem(&self, id: i32) -> IRawElementProviderSimple {
        let root: IRawElementProviderFragmentRoot = self.root.cast().expect("the root is a fragment root");
        Elem { id, hwnd: self.hwnd, shared: self.shared.clone(), tree: self.tree.clone(), root }.into()
    }

    /// Raises `event` on node `id`.
    pub(super) fn raise_on(&self, id: i32, event: UIA_EVENT_ID) {
        unsafe {
            let _ = UiaRaiseAutomationEvent(&self.elem(id), event);
        }
    }

    /// Node `id`'s property `prop` changed from `old` to `new`.
    pub(super) fn changed(&self, id: i32, prop: UIA_PROPERTY_ID, old: VARIANT, new: VARIANT) {
        unsafe {
            let _ = UiaRaiseAutomationPropertyChangedEvent(&self.elem(id), prop, &old, &new);
        }
    }

    /// The nodes came or went.
    pub(super) fn structure_changed(&self) {
        unsafe {
            let _ = UiaRaiseStructureChangedEvent(&self.root, StructureChangeType_ChildrenInvalidated, std::ptr::null_mut(), 0);
        }
    }

    /// Said by the reader (a toast, the photo now showing).
    pub(super) fn notify(&self, text: &str) {
        unsafe {
            let _ = UiaRaiseNotificationEvent(&self.root, NotificationKind_ActionCompleted, NotificationProcessing_MostRecent, &BSTR::from(text), &BSTR::from("Looker"));
        }
    }
}

#[implement(IRawElementProviderSimple, IRawElementProviderFragment, IRawElementProviderFragmentRoot)]
struct Root {
    hwnd: isize,
    shared: Shared,
    tree: Tree,
}

#[implement(IRawElementProviderSimple, IRawElementProviderFragment, IInvokeProvider, IToggleProvider, ISelectionItemProvider, IValueProvider, ITextProvider)]
struct Elem {
    id: i32,
    hwnd: isize,
    shared: Shared,
    tree: Tree,
    root: IRawElementProviderFragmentRoot,
}

#[implement(ITextRangeProvider)]
struct Range {
    shared: Shared,
    hwnd: isize,
    /// The document element, which encloses every range.
    root: IRawElementProviderSimple,
    span: Mutex<(usize, usize)>,
}

unsafe fn unknown_array(items: &[IUnknown]) -> *mut SAFEARRAY {
    unsafe {
        let sa = SafeArrayCreateVector(VT_UNKNOWN, 0, items.len() as u32);
        for (i, it) in items.iter().enumerate() {
            let idx = i as i32;
            let _ = SafeArrayPutElement(sa, &idx, it.as_raw());
        }
        sa
    }
}

fn none<T>() -> Result<T> {
    Err(windows::core::Error::empty())
}

impl Root_Impl {
    fn elem(&self, id: i32) -> IRawElementProviderFragment {
        let root: IRawElementProviderFragmentRoot = self.to_interface();
        Elem { id, hwnd: self.hwnd, shared: self.shared.clone(), tree: self.tree.clone(), root }.into()
    }
}

impl IRawElementProviderSimple_Impl for Root_Impl {
    fn ProviderOptions(&self) -> Result<ProviderOptions> {
        Ok(ProviderOptions_ServerSideProvider | ProviderOptions_UseComThreading)
    }

    fn GetPatternProvider(&self, _id: UIA_PATTERN_ID) -> Result<IUnknown> {
        none()
    }

    fn GetPropertyValue(&self, id: UIA_PROPERTY_ID) -> Result<VARIANT> {
        Ok(match id {
            UIA_NamePropertyId => VARIANT::from(BSTR::from("Looker")),
            UIA_FrameworkIdPropertyId => VARIANT::from(BSTR::from("Looker")),
            _ => VARIANT::default(),
        })
    }

    fn HostRawElementProvider(&self) -> Result<IRawElementProviderSimple> {
        unsafe { UiaHostProviderFromHwnd(HWND(self.hwnd as _)) }
    }
}

impl IRawElementProviderFragment_Impl for Root_Impl {
    fn Navigate(&self, direction: NavigateDirection) -> Result<IRawElementProviderFragment> {
        let t = lock(&self.tree);
        let id = match direction {
            NavigateDirection_FirstChild => t.list.first().map(|n| n.id),
            NavigateDirection_LastChild => t.list.last().map(|n| n.id),
            _ => None,
        };
        drop(t);
        id.map_or_else(none, |id| Ok(self.elem(id)))
    }

    fn GetRuntimeId(&self) -> Result<*mut SAFEARRAY> {
        Ok(std::ptr::null_mut())
    }

    fn BoundingRectangle(&self) -> Result<UiaRect> {
        Ok(UiaRect::default())
    }

    fn GetEmbeddedFragmentRoots(&self) -> Result<*mut SAFEARRAY> {
        Ok(std::ptr::null_mut())
    }

    fn SetFocus(&self) -> Result<()> {
        Ok(())
    }

    fn FragmentRoot(&self) -> Result<IRawElementProviderFragmentRoot> {
        Ok(self.to_interface())
    }
}

impl IRawElementProviderFragmentRoot_Impl for Root_Impl {
    fn ElementProviderFromPoint(&self, x: f64, y: f64) -> Result<IRawElementProviderFragment> {
        let t = lock(&self.tree);
        let id = t.list.iter().rev().find(|n| x >= n.rect[0] && y >= n.rect[1] && x < n.rect[0] + n.rect[2] && y < n.rect[1] + n.rect[3]).map(|n| n.id);
        drop(t);
        id.map_or_else(none, |id| Ok(self.elem(id)))
    }

    fn GetFocus(&self) -> Result<IRawElementProviderFragment> {
        let focus = lock(&self.tree).focus;
        focus.map_or_else(none, |id| Ok(self.elem(id)))
    }
}

impl Elem_Impl {
    fn node(&self) -> Result<Node> {
        lock(&self.tree).list.iter().find(|n| n.id == self.id).cloned().ok_or_else(|| UIA_E_ELEMENTNOTAVAILABLE.into())
    }

    fn sibling(&self, step: isize) -> Option<i32> {
        let t = lock(&self.tree);
        let i = t.list.iter().position(|n| n.id == self.id)? as isize + step;
        (i >= 0).then(|| t.list.get(i as usize).map(|n| n.id)).flatten()
    }

    fn other(&self, id: i32) -> IRawElementProviderFragment {
        Elem { id, hwnd: self.hwnd, shared: self.shared.clone(), tree: self.tree.clone(), root: self.root.clone() }.into()
    }

    /// Asks the window to click it.
    fn press(&self) -> Result<()> {
        let n = self.node()?;
        if !n.enabled {
            return Err(windows::core::Error::from(windows::Win32::Foundation::E_ACCESSDENIED));
        }
        unsafe {
            let _ = PostMessageW(Some(HWND(self.hwnd as _)), WM_UIA_INVOKE, WPARAM(self.id as u32 as usize), LPARAM(0));
        }
        Ok(())
    }

    fn range(&self, a: usize, b: usize) -> ITextRangeProvider {
        Range { shared: self.shared.clone(), hwnd: self.hwnd, root: self.to_interface(), span: Mutex::new((a, b)) }.into()
    }
}

impl IRawElementProviderSimple_Impl for Elem_Impl {
    fn ProviderOptions(&self) -> Result<ProviderOptions> {
        Ok(ProviderOptions_ServerSideProvider | ProviderOptions_UseComThreading)
    }

    fn GetPatternProvider(&self, id: UIA_PATTERN_ID) -> Result<IUnknown> {
        let n = self.node()?;
        let has = match id {
            UIA_InvokePatternId => n.invoke && n.toggle.is_none() && n.selected.is_none(),
            UIA_TogglePatternId => n.toggle.is_some(),
            UIA_SelectionItemPatternId => n.selected.is_some(),
            UIA_ValuePatternId => n.value.is_some(),
            UIA_TextPatternId => n.id == DOC_ID,
            _ => false,
        };
        if !has {
            return none();
        }
        let me: IRawElementProviderSimple = self.to_interface();
        me.cast()
    }

    fn GetPropertyValue(&self, id: UIA_PROPERTY_ID) -> Result<VARIANT> {
        let n = self.node()?;
        let focused = lock(&self.tree).focus == Some(self.id);
        Ok(match id {
            UIA_ControlTypePropertyId => VARIANT::from(n.role),
            UIA_NamePropertyId => VARIANT::from(BSTR::from(n.name.as_str())),
            UIA_AutomationIdPropertyId => VARIANT::from(BSTR::from(n.auto.as_str())),
            UIA_HelpTextPropertyId if !n.help.is_empty() => VARIANT::from(BSTR::from(n.help.as_str())),
            UIA_AcceleratorKeyPropertyId if !n.accel.is_empty() => VARIANT::from(BSTR::from(n.accel.as_str())),
            UIA_IsEnabledPropertyId => VARIANT::from(n.enabled),
            UIA_IsKeyboardFocusablePropertyId => VARIANT::from(n.focusable),
            UIA_HasKeyboardFocusPropertyId => VARIANT::from(focused),
            UIA_IsTextPatternAvailablePropertyId => VARIANT::from(n.id == DOC_ID),
            UIA_FrameworkIdPropertyId => VARIANT::from(BSTR::from("Looker")),
            UIA_IsOffscreenPropertyId => VARIANT::from(false),
            _ => VARIANT::default(),
        })
    }

    fn HostRawElementProvider(&self) -> Result<IRawElementProviderSimple> {
        none()
    }
}

impl IRawElementProviderFragment_Impl for Elem_Impl {
    fn Navigate(&self, direction: NavigateDirection) -> Result<IRawElementProviderFragment> {
        match direction {
            NavigateDirection_Parent => self.root.cast(),
            NavigateDirection_NextSibling => self.sibling(1).map_or_else(none, |id| Ok(self.other(id))),
            NavigateDirection_PreviousSibling => self.sibling(-1).map_or_else(none, |id| Ok(self.other(id))),
            _ => none(),
        }
    }

    fn GetRuntimeId(&self) -> Result<*mut SAFEARRAY> {
        unsafe {
            let sa = SafeArrayCreateVector(VT_I4, 0, 2);
            for (i, v) in [UiaAppendRuntimeId as i32, self.id].iter().enumerate() {
                let idx = i as i32;
                let _ = SafeArrayPutElement(sa, &idx, v as *const i32 as *const std::ffi::c_void);
            }
            Ok(sa)
        }
    }

    fn BoundingRectangle(&self) -> Result<UiaRect> {
        let n = self.node()?;
        Ok(UiaRect { left: n.rect[0], top: n.rect[1], width: n.rect[2], height: n.rect[3] })
    }

    fn GetEmbeddedFragmentRoots(&self) -> Result<*mut SAFEARRAY> {
        Ok(std::ptr::null_mut())
    }

    fn SetFocus(&self) -> Result<()> {
        Ok(())
    }

    fn FragmentRoot(&self) -> Result<IRawElementProviderFragmentRoot> {
        Ok(self.root.clone())
    }
}

impl IInvokeProvider_Impl for Elem_Impl {
    fn Invoke(&self) -> Result<()> {
        self.press()
    }
}

impl IToggleProvider_Impl for Elem_Impl {
    fn Toggle(&self) -> Result<()> {
        self.press()
    }

    fn ToggleState(&self) -> Result<ToggleState> {
        Ok(if self.node()?.toggle == Some(true) { ToggleState_On } else { ToggleState_Off })
    }
}

impl ISelectionItemProvider_Impl for Elem_Impl {
    fn Select(&self) -> Result<()> {
        self.press()
    }

    fn AddToSelection(&self) -> Result<()> {
        Err(windows::Win32::Foundation::E_NOTIMPL.into())
    }

    fn RemoveFromSelection(&self) -> Result<()> {
        Err(windows::Win32::Foundation::E_NOTIMPL.into())
    }

    fn IsSelected(&self) -> Result<BOOL> {
        Ok((self.node()?.selected == Some(true)).into())
    }

    fn SelectionContainer(&self) -> Result<IRawElementProviderSimple> {
        none()
    }
}

impl IValueProvider_Impl for Elem_Impl {
    fn SetValue(&self, _val: &PCWSTR) -> Result<()> {
        Err(windows::Win32::Foundation::E_NOTIMPL.into())
    }

    fn Value(&self) -> Result<BSTR> {
        Ok(BSTR::from(self.node()?.value.unwrap_or_default().as_str()))
    }

    fn IsReadOnly(&self) -> Result<BOOL> {
        Ok((!self.node()?.editable).into())
    }
}

impl ITextProvider_Impl for Elem_Impl {
    fn GetSelection(&self) -> Result<*mut SAFEARRAY> {
        let (a, b) = lock(&self.shared).sel;
        let r: IUnknown = self.range(a, b).cast()?;
        Ok(unsafe { unknown_array(&[r]) })
    }

    fn GetVisibleRanges(&self) -> Result<*mut SAFEARRAY> {
        let n = lock(&self.shared).text.len();
        let r: IUnknown = self.range(0, n).cast()?;
        Ok(unsafe { unknown_array(&[r]) })
    }

    fn RangeFromChild(&self, _child: Ref<IRawElementProviderSimple>) -> Result<ITextRangeProvider> {
        none()
    }

    fn RangeFromPoint(&self, _point: &UiaPoint) -> Result<ITextRangeProvider> {
        let (a, _) = lock(&self.shared).sel;
        Ok(self.range(a, a))
    }

    fn DocumentRange(&self) -> Result<ITextRangeProvider> {
        let n = lock(&self.shared).text.len();
        Ok(self.range(0, n))
    }

    fn SupportedTextSelection(&self) -> Result<SupportedTextSelection> {
        Ok(SupportedTextSelection_Single)
    }
}

// --- Text units ---------------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum Class {
    Space,
    Word,
    Punct,
}

fn class(u: u16) -> Class {
    match char::from_u32(u as u32) {
        Some(c) if c.is_whitespace() => Class::Space,
        Some(c) if c.is_alphanumeric() || c == '_' || c == '\'' => Class::Word,
        None => Class::Word,
        _ => Class::Punct,
    }
}

/// Whether a unit starts at `p` (the text's start always is one).
fn is_start(t: &[u16], p: usize, unit: TextUnit) -> bool {
    if p == 0 {
        return true;
    }
    if p >= t.len() {
        return false;
    }
    match unit {
        TextUnit_Character => !((0xDC00..0xE000).contains(&t[p]) && (0xD800..0xDC00).contains(&t[p - 1])),
        TextUnit_Word => class(t[p]) != Class::Space && class(t[p]) != class(t[p - 1]),
        TextUnit_Line | TextUnit_Paragraph => t[p - 1] == b'\n' as u16,
        _ => false,
    }
}

/// The start of the unit holding `p`.
fn unit_start(t: &[u16], p: usize, unit: TextUnit) -> usize {
    let mut i = p.min(t.len());
    while i > 0 && !is_start(t, i, unit) {
        i -= 1;
    }
    i
}

fn next_start(t: &[u16], p: usize, unit: TextUnit) -> Option<usize> {
    if p >= t.len() {
        return None;
    }
    let mut i = p + 1;
    while i < t.len() && !is_start(t, i, unit) {
        i += 1;
    }
    Some(i.min(t.len()))
}

fn prev_start(t: &[u16], p: usize, unit: TextUnit) -> Option<usize> {
    if p == 0 {
        return None;
    }
    Some(unit_start(t, p - 1, unit))
}

/// Moves `p` by `count` unit starts; returns where it ends and how many it moved. The text's end counts as a
/// stop only for an endpoint (`to_end`): a range moved forward from the last unit stays put.
fn step(t: &[u16], mut p: usize, unit: TextUnit, count: i32, to_end: bool) -> (usize, i32) {
    let whole = !matches!(unit, TextUnit_Character | TextUnit_Word | TextUnit_Line | TextUnit_Paragraph);
    if whole {
        return match count.signum() {
            1 if p < t.len() => (t.len(), 1),
            -1 if p > 0 => (0, -1),
            _ => (p, 0),
        };
    }
    let mut moved = 0;
    while moved != count {
        let next = if count > 0 { next_start(t, p, unit) } else { prev_start(t, p, unit) };
        match next {
            Some(n) if n != p && (to_end || n < t.len()) => {
                p = n;
                moved += count.signum();
            }
            _ => break,
        }
    }
    (p, moved)
}

fn end_of_unit(t: &[u16], start: usize, unit: TextUnit) -> usize {
    match unit {
        TextUnit_Character | TextUnit_Word | TextUnit_Line | TextUnit_Paragraph => next_start(t, start, unit).unwrap_or(t.len()),
        _ => t.len(),
    }
}

impl Range_Impl {
    fn range(&self, a: usize, b: usize) -> ITextRangeProvider {
        Range { shared: self.shared.clone(), hwnd: self.hwnd, root: self.root.clone(), span: Mutex::new((a, b)) }.into()
    }

    fn get(&self) -> (usize, usize) {
        let n = lock(&self.shared).text.len();
        let (a, b) = *self.span.lock().unwrap();
        (a.min(n), b.min(n))
    }

    fn set(&self, a: usize, b: usize) {
        *self.span.lock().unwrap() = (a.min(b), a.max(b));
    }

    fn other(r: Ref<ITextRangeProvider>) -> Result<(usize, usize)> {
        let r = r.ok()?;
        let imp: &Range = unsafe { r.as_impl() };
        Ok(*imp.span.lock().unwrap())
    }
}

impl ITextRangeProvider_Impl for Range_Impl {
    fn Clone(&self) -> Result<ITextRangeProvider> {
        let (a, b) = self.get();
        Ok(self.range(a, b))
    }

    fn Compare(&self, r: Ref<ITextRangeProvider>) -> Result<BOOL> {
        Ok((Self::other(r)? == self.get()).into())
    }

    fn CompareEndpoints(&self, endpoint: TextPatternRangeEndpoint, target: Ref<ITextRangeProvider>, target_endpoint: TextPatternRangeEndpoint) -> Result<i32> {
        let (a, b) = self.get();
        let (c, d) = Self::other(target)?;
        let mine = if endpoint == TextPatternRangeEndpoint_Start { a } else { b };
        let theirs = if target_endpoint == TextPatternRangeEndpoint_Start { c } else { d };
        Ok(mine as i32 - theirs as i32)
    }

    fn ExpandToEnclosingUnit(&self, unit: TextUnit) -> Result<()> {
        let d = lock(&self.shared);
        let (a, _) = *self.span.lock().unwrap();
        let s = unit_start(&d.text, a.min(d.text.len()), unit);
        let (s, e) = match unit {
            TextUnit_Character | TextUnit_Word | TextUnit_Line | TextUnit_Paragraph => (s, end_of_unit(&d.text, s, unit)),
            _ => (0, d.text.len()),
        };
        drop(d);
        self.set(s, e);
        Ok(())
    }

    fn FindAttribute(&self, _id: UIA_TEXTATTRIBUTE_ID, _val: &VARIANT, _backward: BOOL) -> Result<ITextRangeProvider> {
        Err(windows::core::Error::empty())
    }

    fn FindText(&self, text: &BSTR, backward: BOOL, ignore_case: BOOL) -> Result<ITextRangeProvider> {
        let (a, b) = self.get();
        let d = lock(&self.shared);
        let hay = &d.text[a..b];
        let fold = |u: u16| if ignore_case.as_bool() && (b'A' as u16..=b'Z' as u16).contains(&u) { u + 32 } else { u };
        let needle: Vec<u16> = text.as_ref().iter().map(|&u| fold(u)).collect();
        if needle.is_empty() || needle.len() > hay.len() {
            return Err(windows::core::Error::empty());
        }
        let hit = |i: usize| hay[i..i + needle.len()].iter().zip(&needle).all(|(&x, &y)| fold(x) == y);
        let n = hay.len() - needle.len();
        let found = if backward.as_bool() { (0..=n).rev().find(|&i| hit(i)) } else { (0..=n).find(|&i| hit(i)) };
        let Some(i) = found else { return Err(windows::core::Error::empty()) };
        Ok(self.range(a + i, a + i + needle.len()))
    }

    fn GetAttributeValue(&self, _id: UIA_TEXTATTRIBUTE_ID) -> Result<VARIANT> {
        let unk = unsafe { UiaGetReservedNotSupportedValue()? };
        Ok(VARIANT::from(unk))
    }

    fn GetBoundingRectangles(&self) -> Result<*mut SAFEARRAY> {
        Ok(unsafe { SafeArrayCreateVector(VT_R8, 0, 0) })
    }

    fn GetEnclosingElement(&self) -> Result<IRawElementProviderSimple> {
        Ok(self.root.clone())
    }

    fn GetText(&self, max: i32) -> Result<BSTR> {
        let (a, b) = self.get();
        let d = lock(&self.shared);
        let end = if max >= 0 { b.min(a + max as usize) } else { b };
        Ok(BSTR::from_wide(&d.text[a..end]))
    }

    fn Move(&self, unit: TextUnit, count: i32) -> Result<i32> {
        let (a, b) = self.get();
        let d = lock(&self.shared);
        let degenerate = a == b;
        let s = unit_start(&d.text, a, unit);
        let (s, moved) = step(&d.text, s, unit, count, false);
        let e = if degenerate { s } else { end_of_unit(&d.text, s, unit) };
        drop(d);
        self.set(s, e);
        Ok(moved)
    }

    fn MoveEndpointByUnit(&self, endpoint: TextPatternRangeEndpoint, unit: TextUnit, count: i32) -> Result<i32> {
        let (a, b) = self.get();
        let d = lock(&self.shared);
        let p = if endpoint == TextPatternRangeEndpoint_Start { a } else { b };
        let (p, moved) = step(&d.text, p, unit, count, true);
        drop(d);
        if endpoint == TextPatternRangeEndpoint_Start {
            self.set(p, b.max(p));
        } else {
            self.set(a.min(p), p);
        }
        Ok(moved)
    }

    fn MoveEndpointByRange(&self, endpoint: TextPatternRangeEndpoint, target: Ref<ITextRangeProvider>, target_endpoint: TextPatternRangeEndpoint) -> Result<()> {
        let (a, b) = self.get();
        let (c, d) = Self::other(target)?;
        let p = if target_endpoint == TextPatternRangeEndpoint_Start { c } else { d };
        if endpoint == TextPatternRangeEndpoint_Start {
            self.set(p, b.max(p));
        } else {
            self.set(a.min(p), p);
        }
        Ok(())
    }

    fn Select(&self) -> Result<()> {
        let (a, b) = self.get();
        lock(&self.shared).select = Some((a, b));
        unsafe {
            let _ = PostMessageW(Some(HWND(self.hwnd as _)), WM_UIA_SELECT, WPARAM(0), LPARAM(0));
        }
        Ok(())
    }

    fn AddToSelection(&self) -> Result<()> {
        Err(windows::Win32::Foundation::E_NOTIMPL.into())
    }

    fn RemoveFromSelection(&self) -> Result<()> {
        Err(windows::Win32::Foundation::E_NOTIMPL.into())
    }

    fn ScrollIntoView(&self, _top: BOOL) -> Result<()> {
        self.Select()
    }

    fn GetChildren(&self) -> Result<*mut SAFEARRAY> {
        Ok(unsafe { unknown_array(&[]) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn units_step_by_words_and_lines() {
        let t: Vec<u16> = "one two, three\nfour five\n\nsix".encode_utf16().collect();
        assert_eq!(unit_start(&t, 5, TextUnit_Word), 4);
        assert_eq!(next_start(&t, 4, TextUnit_Word), Some(7));
        assert_eq!(step(&t, 0, TextUnit_Line, 1, false), (15, 1));
        assert_eq!(step(&t, 0, TextUnit_Line, 3, false), (26, 3));
        assert_eq!(step(&t, 26, TextUnit_Line, 5, false), (26, 0));
        assert_eq!(step(&t, 26, TextUnit_Line, -2, false), (15, -2));
        assert_eq!(end_of_unit(&t, 15, TextUnit_Line), 25);
        assert_eq!(step(&t, 26, TextUnit_Line, 1, true), (29, 1));
    }
}
