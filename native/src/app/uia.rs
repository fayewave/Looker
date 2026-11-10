//! Screen readers in the text editor (Narrator, NVDA, JAWS): UI Automation's text pattern, as Notepad offers it.
//! While a text file is up the window answers WM_GETOBJECT with a Document element whose text pattern reads the
//! document by character, word, line and the whole; follows the caret and selection (TextSelectionChanged as
//! they move, TextChanged on edits); and lets the reader select text. Everything else of Looker is drawn, not
//! controls, and isn't exposed.
//!
//! The provider reads a copy of the text and selection (`Doc`) that the window refreshes as they change, only
//! while some client is listening (`UiaClientsAreListening`): the providers run on UI Automation's threads.

#![allow(non_upper_case_globals)]

use std::sync::{Arc, Mutex};

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::SAFEARRAY;
use windows::Win32::System::Ole::{SafeArrayCreateVector, SafeArrayPutElement};
use windows::Win32::System::Variant::{VARIANT, VT_R8, VT_UNKNOWN};
use windows::Win32::UI::Accessibility::*;
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};
use windows::core::{AsImpl, BOOL, BSTR, IUnknown, IUnknownImpl, Interface, Ref, Result, implement};

/// Posted when a reader selects text: the window picks the range up from `Doc::select`.
pub(super) const WM_UIA_SELECT: u32 = WM_APP + 31;

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

/// The window's end: the element, and what it reads.
pub(super) struct Uia {
    pub shared: Shared,
    pub root: IRawElementProviderSimple,
    /// What was last handed over: the document's id and version, and the selection.
    pub synced: (u64, u64, (usize, usize), bool),
}

impl Uia {
    pub(super) fn new(hwnd: HWND) -> Uia {
        let shared: Shared = Arc::new(Mutex::new(Doc::default()));
        let root: IRawElementProviderSimple = Root { hwnd: hwnd.0 as isize, shared: shared.clone() }.into();
        Uia { shared, root, synced: (0, u64::MAX, (usize::MAX, 0), false) }
    }

    /// Answers WM_GETOBJECT for the window's root.
    pub(super) fn answer(&self, hwnd: HWND, wp: WPARAM, lp: LPARAM) -> windows::Win32::Foundation::LRESULT {
        unsafe { UiaReturnRawElementProvider(hwnd, wp, lp, &self.root) }
    }

    pub(super) fn listening() -> bool {
        unsafe { UiaClientsAreListening().as_bool() }
    }

    pub(super) fn raise(&self, id: UIA_EVENT_ID) {
        unsafe {
            let _ = UiaRaiseAutomationEvent(&self.root, id);
        }
    }
}

#[implement(IRawElementProviderSimple, ITextProvider)]
struct Root {
    hwnd: isize,
    shared: Shared,
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

impl Root_Impl {
    fn range(&self, a: usize, b: usize) -> ITextRangeProvider {
        Range { shared: self.shared.clone(), hwnd: self.hwnd, root: self.to_interface(), span: Mutex::new((a, b)) }.into()
    }
}

impl IRawElementProviderSimple_Impl for Root_Impl {
    fn ProviderOptions(&self) -> Result<ProviderOptions> {
        Ok(ProviderOptions_ServerSideProvider | ProviderOptions_UseComThreading)
    }

    fn GetPatternProvider(&self, id: UIA_PATTERN_ID) -> Result<IUnknown> {
        if id == UIA_TextPatternId {
            let p: ITextProvider = self.to_interface();
            return p.cast();
        }
        Err(windows::core::Error::empty())
    }

    fn GetPropertyValue(&self, id: UIA_PROPERTY_ID) -> Result<VARIANT> {
        let d = self.shared.lock().unwrap();
        Ok(match id {
            UIA_ControlTypePropertyId => VARIANT::from(UIA_DocumentControlTypeId.0),
            UIA_NamePropertyId => VARIANT::from(BSTR::from(d.name.as_str())),
            UIA_AutomationIdPropertyId => VARIANT::from(BSTR::from("TextEditor")),
            UIA_IsKeyboardFocusablePropertyId | UIA_IsTextPatternAvailablePropertyId | UIA_IsEnabledPropertyId => VARIANT::from(true),
            UIA_HasKeyboardFocusPropertyId => VARIANT::from(d.focused),
            _ => VARIANT::default(),
        })
    }

    fn HostRawElementProvider(&self) -> Result<IRawElementProviderSimple> {
        unsafe { UiaHostProviderFromHwnd(HWND(self.hwnd as _)) }
    }
}

impl ITextProvider_Impl for Root_Impl {
    fn GetSelection(&self) -> Result<*mut SAFEARRAY> {
        let (a, b) = self.shared.lock().unwrap().sel;
        let r: IUnknown = self.range(a, b).cast()?;
        Ok(unsafe { unknown_array(&[r]) })
    }

    fn GetVisibleRanges(&self) -> Result<*mut SAFEARRAY> {
        let n = self.shared.lock().unwrap().text.len();
        let r: IUnknown = self.range(0, n).cast()?;
        Ok(unsafe { unknown_array(&[r]) })
    }

    fn RangeFromChild(&self, _child: Ref<IRawElementProviderSimple>) -> Result<ITextRangeProvider> {
        Err(windows::core::Error::empty())
    }

    fn RangeFromPoint(&self, _point: &UiaPoint) -> Result<ITextRangeProvider> {
        let (a, _) = self.shared.lock().unwrap().sel;
        Ok(self.range(a, a))
    }

    fn DocumentRange(&self) -> Result<ITextRangeProvider> {
        let n = self.shared.lock().unwrap().text.len();
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
        let n = self.shared.lock().unwrap().text.len();
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
        let d = self.shared.lock().unwrap();
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
        let d = self.shared.lock().unwrap();
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
        let d = self.shared.lock().unwrap();
        let end = if max >= 0 { b.min(a + max as usize) } else { b };
        Ok(BSTR::from_wide(&d.text[a..end]))
    }

    fn Move(&self, unit: TextUnit, count: i32) -> Result<i32> {
        let (a, b) = self.get();
        let d = self.shared.lock().unwrap();
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
        let d = self.shared.lock().unwrap();
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
        self.shared.lock().unwrap().select = Some((a, b));
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
