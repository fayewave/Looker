//! A playing video's place in Windows (Faye, 2026-11-10): its progress on the taskbar button (`ITaskbarList3`,
//! as video players show it), and Windows' media controls (System Media Transport Controls: the play/pause keys
//! on a keyboard, the media flyout by the volume, the lock screen), which Looker's window joins through
//! `ISystemMediaTransportControlsInterop`.
//!
//! The window feeds `Media::update` every frame with what's playing (or nothing); it calls Windows only when
//! something changed: the play state at once, the taskbar's progress when it moved a visible step, the flyout's
//! timeline once a second. The controls' buttons come back as `WM_MEDIA_BUTTON` posted to the window (Windows
//! raises them on a thread of its own).

use std::time::{Duration, Instant};

use windows::Foundation::{TimeSpan, TypedEventHandler};
use windows::Media::{
    MediaPlaybackStatus, MediaPlaybackType, SystemMediaTransportControls, SystemMediaTransportControlsButton,
    SystemMediaTransportControlsButtonPressedEventArgs, SystemMediaTransportControlsTimelineProperties,
};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::System::WinRT::ISystemMediaTransportControlsInterop;
use windows::Win32::UI::Shell::{ITaskbarList3, TBPF_NOPROGRESS, TBPF_NORMAL, TBPF_PAUSED, TaskbarList};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};
use windows::core::HSTRING;

/// Posted to the window when a media control is pressed: `wParam` is a `BUTTON_*`.
pub const WM_MEDIA_BUTTON: u32 = WM_APP + 23;
pub const BUTTON_PLAY: usize = 1;
pub const BUTTON_PAUSE: usize = 2;
pub const BUTTON_STOP: usize = 3;

/// What's playing, for Windows.
#[derive(Clone, PartialEq)]
pub struct Playing {
    pub title: String,
    pub playing: bool,
    pub position_us: i64,
    pub duration_us: i64,
}

#[derive(Default)]
pub struct Media {
    taskbar: Option<ITaskbarList3>,
    taskbar_failed: bool,
    smtc: Option<SystemMediaTransportControls>,
    smtc_failed: bool,
    /// What Windows was last told: the title and play state, the taskbar's progress (thousandths), and when the
    /// flyout's timeline was last updated.
    title: Option<String>,
    playing: Option<bool>,
    progress: Option<u64>,
    timeline_at: Option<Instant>,
}

impl Media {
    /// Tells Windows what's playing now (`None`: nothing, the progress and the controls go).
    pub fn update(&mut self, hwnd: HWND, now: Option<&Playing>) {
        let Some(p) = now else {
            if self.title.is_some() || self.progress.is_some() {
                if let Some(t) = &self.taskbar {
                    unsafe {
                        let _ = t.SetProgressState(hwnd, TBPF_NOPROGRESS);
                    }
                }
                if let Some(s) = &self.smtc {
                    let _ = s.SetPlaybackStatus(MediaPlaybackStatus::Closed);
                    let _ = s.DisplayUpdater().and_then(|d| d.ClearAll());
                    let _ = s.SetIsEnabled(false);
                }
                (self.title, self.playing, self.progress, self.timeline_at) = (None, None, None, None);
            }
            return;
        };
        self.taskbar(hwnd, p);
        self.controls(hwnd, p);
    }

    fn taskbar(&mut self, hwnd: HWND, p: &Playing) {
        if self.taskbar.is_none() && !self.taskbar_failed {
            let t: Option<ITaskbarList3> = unsafe { CoCreateInstance(&TaskbarList, None, CLSCTX_INPROC_SERVER).ok() };
            match t.filter(|t| unsafe { t.HrInit() }.is_ok()) {
                Some(t) => self.taskbar = Some(t),
                None => self.taskbar_failed = true,
            }
        }
        let Some(t) = &self.taskbar else { return };
        let progress = if p.duration_us > 0 { (p.position_us.clamp(0, p.duration_us) as f64 / p.duration_us as f64 * 1000.0).round() as u64 } else { 0 };
        let state_changed = self.playing != Some(p.playing) || self.progress.is_none();
        unsafe {
            if state_changed {
                // Paused: Windows' yellow, as players show it.
                let _ = t.SetProgressState(hwnd, if p.playing { TBPF_NORMAL } else { TBPF_PAUSED });
            }
            if state_changed || self.progress != Some(progress) {
                let _ = t.SetProgressValue(hwnd, progress, 1000);
            }
        }
        self.progress = Some(progress);
    }

    fn controls(&mut self, hwnd: HWND, p: &Playing) {
        if self.smtc.is_none() && !self.smtc_failed {
            match open_controls(hwnd) {
                Some(s) => self.smtc = Some(s),
                None => self.smtc_failed = true,
            }
        }
        let Some(s) = &self.smtc else { return };
        if self.title.as_deref() != Some(p.title.as_str()) {
            let _ = s.SetIsEnabled(true);
            if let Ok(d) = s.DisplayUpdater() {
                let _ = d.SetType(MediaPlaybackType::Video);
                if let Ok(v) = d.VideoProperties() {
                    let _ = v.SetTitle(&HSTRING::from(p.title.as_str()));
                }
                let _ = d.Update();
            }
            self.title = Some(p.title.clone());
            self.timeline_at = None;
        }
        if self.playing != Some(p.playing) {
            let _ = s.SetPlaybackStatus(if p.playing { MediaPlaybackStatus::Playing } else { MediaPlaybackStatus::Paused });
            self.playing = Some(p.playing);
            self.timeline_at = None;
        }
        // The flyout's timeline: once a second while playing (Microsoft asks for no more), at once on a change.
        if self.timeline_at.is_none_or(|t| t.elapsed() >= Duration::from_secs(1)) && p.duration_us > 0 {
            if let Ok(tl) = SystemMediaTransportControlsTimelineProperties::new() {
                let span = |us: i64| TimeSpan { Duration: us.max(0) * 10 };
                let _ = tl.SetStartTime(span(0));
                let _ = tl.SetMinSeekTime(span(0));
                let _ = tl.SetEndTime(span(p.duration_us));
                let _ = tl.SetMaxSeekTime(span(p.duration_us));
                let _ = tl.SetPosition(span(p.position_us.min(p.duration_us)));
                let _ = s.UpdateTimelineProperties(&tl);
            }
            self.timeline_at = Some(Instant::now());
        }
    }
}

/// The window's media controls, play and pause on, their presses posted back to it.
fn open_controls(hwnd: HWND) -> Option<SystemMediaTransportControls> {
    let fail = |step: &str, e: windows::core::Error| crate::trace::mark(format!("media controls: {step}: {e}"));
    let interop = windows::core::factory::<SystemMediaTransportControls, ISystemMediaTransportControlsInterop>().map_err(|e| fail("factory", e)).ok()?;
    let s: SystemMediaTransportControls = unsafe { interop.GetForWindow(hwnd) }.map_err(|e| fail("GetForWindow", e)).ok()?;
    s.SetIsPlayEnabled(true).map_err(|e| fail("play", e)).ok()?;
    s.SetIsPauseEnabled(true).ok()?;
    crate::trace::mark("media controls: open");
    let _ = s.SetIsStopEnabled(true);
    let target = hwnd.0 as isize; // HWND isn't Send; the handler runs on Windows' thread
    let handler = TypedEventHandler::<SystemMediaTransportControls, SystemMediaTransportControlsButtonPressedEventArgs>::new(move |_, args| {
        let Some(a) = args.as_ref() else { return Ok(()) };
        let button = match a.Button()? {
            SystemMediaTransportControlsButton::Play => BUTTON_PLAY,
            SystemMediaTransportControlsButton::Pause => BUTTON_PAUSE,
            SystemMediaTransportControlsButton::Stop => BUTTON_STOP,
            _ => return Ok(()),
        };
        unsafe {
            let _ = PostMessageW(Some(HWND(target as _)), WM_MEDIA_BUTTON, WPARAM(button), LPARAM(0));
        }
        Ok(())
    });
    s.ButtonPressed(&handler).ok()?;
    Some(s)
}
