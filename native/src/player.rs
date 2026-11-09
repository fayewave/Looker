//! Playing a video: a decode thread runs the FFmpeg decoder (imaging/video.rs) a few pictures and about a second
//! of audio ahead; an audio thread feeds WASAPI from that; the UI thread asks, once per vsync, for the picture due
//! now (`Player::due`) and draws it (app/video.rs). The clock is the audio device's when the sound is playing (at
//! 0.25x to 2x: other speeds go through FFmpeg's atempo, same pitch), else the wall clock.
//!
//! Every seek bumps an epoch: what the decode thread had queued is thrown away, and the first picture of the new
//! epoch is shown at once, playing or paused (scrubbing and frame steps show where they landed).

use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use crate::imaging::video::{AUDIO_RATE, Decoder, Info, Item, Picture, Tempo};

/// Pictures decoded ahead, normally; up to `HARD_PICTURES` while the audio is behind, and past that the decoder
/// reads ahead for the sound alone (`Decoder::next_audio`). A GPU decoder holds one surface per picture queued
/// (looker_video.c asks for 10 extra; one more is on screen).
const MAX_PICTURES: usize = 6;
const HARD_PICTURES: usize = 9;
/// Audio decoded ahead, in frames (one frame = a left and a right sample).
const MAX_AUDIO: usize = AUDIO_RATE as usize * 3 / 2;

/// A block of audio in the ring: its first sample's time, interleaved stereo, and how much is already played.
struct Chunk {
    pts: i64,
    samples: Vec<f32>,
    read: usize,
}

struct State {
    pictures: VecDeque<Picture>,
    audio: VecDeque<Chunk>,
    audio_frames: usize,
    /// Bumped by each seek; the decode thread drops what it decoded for an older one.
    epoch: u32,
    /// The decoder reached the end in this epoch.
    end: bool,
    /// For the decode thread: where to go next.
    seek: Option<(i64, bool)>,
    quit: bool,
    error: Option<String>,

    playing: bool,
    speed: f64,
    volume: f32,
    muted: bool,
    /// Media time at `anchor_at` (the wall clock runs from there while playing; the time itself when paused).
    anchor: i64,
    anchor_at: Instant,
    /// The audio device's clock: the time after the last sample written, how much of that was still queued in
    /// the device (in media time: at 2x a second queued is two), and when that was read. None until sound plays
    /// (again) after a pause or seek.
    audio_clock: Option<(i64, i64, Instant)>,
    /// The audio thread should drop what the device holds (a seek).
    audio_reset: bool,
    /// No device or no stream: the wall clock it is.
    audio_dead: bool,
    /// The sound ran out before the pictures (this epoch): the wall clock takes over from where it stopped.
    audio_ended: bool,
    /// atempo couldn't be set up: speeds other than 1x play silent.
    tempo_failed: bool,
    /// The decoder can't reach more sound without decoding more pictures (it read as far ahead as it may), or
    /// the sound is over: when the device runs dry now, the sound has ended.
    audio_exhausted: bool,
}

impl State {
    fn clock(&self, now: Instant) -> i64 {
        if !self.playing {
            return self.anchor;
        }
        if let Some((end, queued, at)) = self.audio_clock.filter(|_| self.audio_live()) {
            let t = end - queued + (now.saturating_duration_since(at).as_secs_f64() * 1e6 * self.speed) as i64;
            return t.min(end);
        }
        self.anchor + (now.saturating_duration_since(self.anchor_at).as_secs_f64() * 1e6 * self.speed) as i64
    }

    /// Whether the sound is what's playing (0.25x to 2x; faster plays silent).
    fn audio_live(&self) -> bool {
        !self.audio_dead && !self.audio_ended && (self.speed == 1.0 || (!self.tempo_failed && (0.25..=2.0).contains(&self.speed)))
    }

    /// Re-anchors the wall clock at `t` (keeps the audio clock out until sound plays from there).
    fn set_anchor(&mut self, t: i64, now: Instant) {
        self.anchor = t;
        self.anchor_at = now;
        self.audio_clock = None;
    }

    fn clear(&mut self) {
        self.pictures.clear();
        self.audio.clear();
        self.audio_frames = 0;
        self.end = false;
        self.audio_reset = true;
        self.audio_clock = None;
        self.audio_ended = false;
        self.audio_exhausted = false;
    }
}

struct Shared {
    state: Mutex<State>,
    /// Wakes the decode thread (room in the queue, a seek, quit).
    wake: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// What the UI thread draws from: the picture due now, if it changed.
pub struct Due {
    pub picture: Picture,
}

pub struct Player {
    shared: Arc<Shared>,
    pub info: Option<Info>,
    /// The epoch and time of the picture on screen.
    shown: Option<(u32, i64, i64)>,
    /// Loops instead of stopping at the end (short clips).
    pub looping: bool,
    /// A frame step waiting for its picture.
    step: bool,
    /// Pictures shown and skipped (late), for the trace.
    shown_count: u64,
    dropped: u64,
    opened: Arc<Mutex<Option<Result<Info, String>>>>,
}

impl Player {
    /// Starts decoding `path` on its own thread (`d3d11`: the window's device to decode on, or null), playing at
    /// once when `play`.
    pub fn open(path: &Path, d3d11: crate::gfx::Sendable<*mut std::ffi::c_void>, play: bool, volume: f32, muted: bool) -> Player {
        let now = Instant::now();
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                pictures: VecDeque::new(),
                audio: VecDeque::new(),
                audio_frames: 0,
                epoch: 0,
                end: false,
                seek: None,
                quit: false,
                error: None,
                playing: play,
                speed: 1.0,
                volume,
                muted,
                anchor: 0,
                anchor_at: now,
                audio_clock: None,
                audio_reset: false,
                audio_dead: false,
                audio_ended: false,
                tempo_failed: false,
                audio_exhausted: false,
            }),
            wake: Condvar::new(),
        });
        let opened = Arc::new(Mutex::new(None));
        let (s, p, o) = (shared.clone(), path.to_path_buf(), opened.clone());
        let device = d3d11;
        std::thread::Builder::new()
            .name("video-decode".into())
            .spawn(move || {
                let device = device;
                decode_thread(s, &p, device.0, o)
            })
            .ok();
        Player { shared, info: None, shown: None, looping: false, step: false, shown_count: 0, dropped: 0, opened }
    }

    /// The stream's facts once the decoder is open (`Err` when it couldn't be).
    pub fn poll_open(&mut self) -> Option<Result<(), String>> {
        if self.info.is_some() {
            return Some(Ok(()));
        }
        let r = self.opened.lock().ok()?.take()?;
        Some(r.map(|i| {
            // Short clips loop, like an animated image.
            self.looping = i.duration_us > 0 && i.duration_us < 10_000_000;
            if i.audio_channels > 0 {
                let s = self.shared.clone();
                std::thread::Builder::new().name("video-audio".into()).spawn(move || audio_thread(s)).ok();
            } else {
                self.shared.lock().audio_dead = true;
            }
            self.info = Some(i);
        }))
    }

    /// The picture to draw now, when it changed since the last call; also stops (or loops) at the end.
    pub fn due(&mut self, now: Instant) -> Option<Due> {
        let mut st = self.shared.lock();
        let epoch = st.epoch;
        let fresh = self.shown.is_none_or(|(e, _, _)| e != epoch);
        let mut pick: Option<Picture> = None;
        if self.step && !fresh {
            // A frame step: the next picture, and the clock moves to it.
            if let Some(p) = st.pictures.pop_front() {
                self.step = false;
                st.set_anchor(p.pts_us, now);
                pick = Some(p);
            }
        } else if fresh {
            // The first picture after a seek (or the very first): shown at once, and the clock starts from it.
            if let Some(p) = st.pictures.pop_front() {
                st.set_anchor(p.pts_us, now);
                pick = Some(p);
            }
        } else {
            let t = st.clock(now);
            while st.pictures.front().is_some_and(|p| p.pts_us <= t) {
                if pick.is_some() {
                    self.dropped += 1;
                }
                pick = st.pictures.pop_front();
            }
            // At the end: loop, or stop on the last picture.
            if pick.is_none() && st.end && st.pictures.is_empty() && st.playing {
                let last_end = self.shown.map_or(0, |(_, pts, dur)| pts + dur);
                if t >= last_end {
                    if self.looping {
                        drop(st);
                        self.seek(0, false);
                        return None;
                    }
                    st.playing = false;
                    st.set_anchor(last_end, now);
                }
            }
        }
        if pick.is_some() {
            self.shared.wake.notify_all();
        }
        drop(st);
        let p = pick?;
        self.shown_count += 1;
        self.shown = Some((epoch, p.pts_us, p.duration_us));
        Some(Due { picture: p })
    }

    /// Whether frames are moving (the UI keeps drawing every vsync), or the first picture is still to come.
    pub fn animating(&self) -> bool {
        let st = self.shared.lock();
        st.playing || self.shown.is_none_or(|(e, _, _)| e != st.epoch)
    }

    pub fn playing(&self) -> bool {
        self.shared.lock().playing
    }

    pub fn set_playing(&mut self, play: bool) {
        let now = Instant::now();
        let mut st = self.shared.lock();
        if st.playing == play {
            return;
        }
        // Played to the end: playing again starts over.
        if play && st.end && st.pictures.is_empty() {
            drop(st);
            self.seek(0, false);
            self.shared.lock().playing = true;
            return;
        }
        let t = st.clock(now);
        st.playing = play;
        st.set_anchor(t, now);
        self.shared.wake.notify_all();
    }

    /// The time of the picture on screen (or where a seek is going).
    pub fn position(&self) -> i64 {
        self.shown.map_or(0, |(_, pts, _)| pts)
    }

    pub fn duration(&self) -> i64 {
        self.info.as_ref().map_or(0, |i| i.duration_us)
    }

    /// Jumps to `us` (clamped to the video): to the keyframe before it, or `exact`ly.
    pub fn seek(&mut self, us: i64, exact: bool) {
        let d = self.duration();
        let us = if d > 0 { us.clamp(0, d) } else { us.max(0) };
        let now = Instant::now();
        let mut st = self.shared.lock();
        st.clear();
        st.epoch = st.epoch.wrapping_add(1);
        st.seek = Some((us, exact));
        st.set_anchor(us, now);
        self.shared.wake.notify_all();
    }

    /// Pauses and shows the next picture (at the next `due`).
    pub fn step_forward(&mut self) {
        self.set_playing(false);
        self.step = true;
    }

    /// Paused: the picture before the one on screen. Aims at the middle of it: a picture's time is rounded to
    /// the microsecond, so "just before this one" can fall on the boundary and land on this one again.
    pub fn step_back(&mut self) {
        self.set_playing(false);
        let Some((_, pts, dur)) = self.shown else { return };
        let t = pts - dur.max(2) / 2;
        if t >= 0 {
            self.seek(t, true);
        }
    }

    pub fn speed(&self) -> f64 {
        self.shared.lock().speed
    }

    /// The picture for the last seek is on screen (scrubbing waits for it before seeking again).
    pub fn settled(&self) -> bool {
        let epoch = self.shared.lock().epoch;
        self.shown.is_some_and(|(e, _, _)| e == epoch)
    }

    /// Whether it's at its end and stopped there.
    pub fn at_end(&self) -> bool {
        let st = self.shared.lock();
        !st.playing && st.end && st.pictures.is_empty() && self.settled_locked(&st)
    }

    fn settled_locked(&self, st: &State) -> bool {
        self.shown.is_some_and(|(e, _, _)| e == st.epoch)
    }

    pub fn set_speed(&mut self, speed: f64) {
        let now = Instant::now();
        let mut st = self.shared.lock();
        let t = st.clock(now);
        st.speed = speed;
        st.set_anchor(t, now);
        st.audio_reset = true;
    }

    pub fn set_looping(&mut self, on: bool) {
        self.looping = on;
    }

    pub fn set_volume(&mut self, volume: f32, muted: bool) {
        let mut st = self.shared.lock();
        st.volume = volume;
        st.muted = muted;
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        let clock = {
            let st = self.shared.lock();
            if st.audio_dead { "wall (no sound)" } else if st.audio_ended { "wall (sound ended)" } else if st.audio_clock.is_some() { "audio" } else { "wall" }
        };
        crate::trace::mark(format!("video: closed after {} pictures shown, {} skipped late; clock {clock}", self.shown_count, self.dropped));
        // The threads see this and end on their own (never joined: a decode may be mid-picture).
        self.shared.lock().quit = true;
        self.shared.wake.notify_all();
    }
}

fn decode_thread(shared: Arc<Shared>, path: &Path, d3d11: *mut std::ffi::c_void, opened: Arc<Mutex<Option<Result<Info, String>>>>) {
    let t = Instant::now();
    let mut dec = match Decoder::open(path, d3d11, false) {
        Ok((d, info)) => {
            crate::trace::mark(format!(
                "video: opened {} ({}) in {:.0} ms",
                crate::imaging::video::codec_label(&info.codec(), &info.profile()),
                if d.hardware() { "GPU" } else { "CPU" },
                t.elapsed().as_secs_f64() * 1000.0
            ));
            if let Ok(mut o) = opened.lock() {
                *o = Some(Ok(info));
            }
            d
        }
        Err(e) => {
            crate::trace::mark(format!("video: {e}"));
            shared.lock().error = Some(e.clone());
            if let Ok(mut o) = opened.lock() {
                *o = Some(Err(e));
            }
            return;
        }
    };
    loop {
        // A seek or quit first; then wait while the queues are full (or at the end).
        let (pending, sound_only) = {
            let mut st = shared.lock();
            let mut sound_only = false;
            loop {
                if st.quit {
                    return;
                }
                if st.seek.is_some() {
                    break;
                }
                let n = st.pictures.len();
                let audio_full = st.audio_frames >= MAX_AUDIO;
                // The device drains the audio on its own; with little of it queued, the decoder goes on (up to the
                // hard cap), then reads ahead for the sound alone, so it doesn't run dry behind the pictures (some
                // files store their sound a second or more after the pictures it goes with).
                let starving = !st.audio_dead && !st.audio_ended && !st.audio_exhausted && st.audio_frames < AUDIO_RATE as usize / 2;
                if !st.end && !(audio_full && n > 0) {
                    if n < MAX_PICTURES || (starving && n < HARD_PICTURES) {
                        break;
                    }
                    if starving {
                        sound_only = true;
                        break;
                    }
                }
                st = shared.wake.wait_timeout(st, Duration::from_millis(100)).map(|(g, _)| g).unwrap_or_else(|e| e.into_inner().0);
            }
            (st.seek.take().map(|s| (s, st.epoch)), sound_only)
        };
        if let Some(((us, exact), _)) = pending {
            if let Err(e) = dec.seek(us, exact) {
                crate::trace::mark(format!("video: seek: {e}"));
            }
            continue;
        }
        if sound_only {
            let item = dec.next_audio();
            let mut st = shared.lock();
            if st.quit {
                return;
            }
            if st.seek.is_some() {
                continue;
            }
            match item {
                Ok(Some(Item::Audio(pts, samples))) if !samples.is_empty() => {
                    st.audio_frames += samples.len() / 2;
                    st.audio.push_back(Chunk { pts, samples, read: 0 });
                }
                Ok(Some(_)) => {}
                Ok(None) | Err(_) => st.audio_exhausted = true,
            }
            continue;
        }
        let item = dec.next();
        let mut st = shared.lock();
        if st.quit {
            return;
        }
        if st.seek.is_some() {
            continue; // decoded for the old position: dropped
        }
        match item {
            Ok(Item::Picture(p)) => {
                st.pictures.push_back(p);
                st.audio_exhausted = false; // a picture further on: the sound may be in reach again
            }
            Ok(Item::Audio(pts, samples)) => {
                if !st.audio_dead && !samples.is_empty() {
                    st.audio_frames += samples.len() / 2;
                    st.audio.push_back(Chunk { pts, samples, read: 0 });
                }
            }
            Ok(Item::End) => st.end = true,
            Err(e) => {
                crate::trace::mark(format!("video: decode: {e}"));
                st.error = Some(e);
                st.end = true;
            }
        }
    }
}

// --- The seek bar's previews ---

/// A preview: the time asked for and the picture found there (the keyframe at or before it) as premultiplied BGRA.
pub struct Still {
    pub asked: i64,
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

#[derive(Default)]
struct PreviewState {
    want: Option<i64>,
    got: Option<Still>,
    quit: bool,
    failed: bool,
}

struct PreviewShared {
    state: Mutex<PreviewState>,
    wake: Condvar,
}

/// Pictures for the seek bar's hover: a second decoder on its own thread, on the CPU and without the sound, that
/// jumps to the keyframe before the time asked for and hands that picture back small. Only the latest wish counts.
pub struct Preview {
    shared: Arc<PreviewShared>,
}

impl Preview {
    /// Opens `path` for pictures fitted into `box_w × box_h`, turned `rotation` degrees.
    pub fn open(path: &Path, rotation: i32, box_w: u32, box_h: u32) -> Preview {
        let shared = Arc::new(PreviewShared { state: Mutex::new(PreviewState::default()), wake: Condvar::new() });
        let (s, p) = (shared.clone(), path.to_path_buf());
        std::thread::Builder::new().name("video-preview".into()).spawn(move || preview_thread(&s, &p, rotation, box_w, box_h)).ok();
        Preview { shared }
    }

    fn lock(&self) -> MutexGuard<'_, PreviewState> {
        self.shared.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Asks for the picture at `us` (replacing any earlier wish).
    pub fn want(&self, us: i64) {
        let mut st = self.lock();
        if st.want != Some(us) {
            st.want = Some(us);
            self.shared.wake.notify_all();
        }
    }

    /// The newest picture found, once.
    pub fn take(&self) -> Option<Still> {
        self.lock().got.take()
    }

    /// A wish is still being worked on.
    pub fn busy(&self) -> bool {
        let st = self.lock();
        !st.failed && st.want.is_some()
    }
}

impl Drop for Preview {
    fn drop(&mut self) {
        self.lock().quit = true;
        self.shared.wake.notify_all();
    }
}

fn preview_thread(shared: &PreviewShared, path: &Path, rotation: i32, box_w: u32, box_h: u32) {
    let lock = || shared.state.lock().unwrap_or_else(|e| e.into_inner());
    let mut dec = match Decoder::open(path, std::ptr::null_mut(), true) {
        Ok((d, _)) => d,
        Err(e) => {
            crate::trace::mark(format!("video preview: {e}"));
            lock().failed = true;
            return;
        }
    };
    loop {
        let us = {
            let mut st = lock();
            loop {
                if st.quit {
                    return;
                }
                if let Some(us) = st.want {
                    break us;
                }
                st = shared.wake.wait(st).unwrap_or_else(|e| e.into_inner());
            }
        };
        let mut found = None;
        if dec.seek(us, false).is_ok() {
            // The first picture after a keyframe seek is the keyframe (a few tries past stray audio-less items).
            for _ in 0..64 {
                match dec.next() {
                    Ok(Item::Picture(p)) => {
                        found = Some(p);
                        break;
                    }
                    Ok(Item::Audio(..)) => {}
                    _ => break,
                }
            }
        }
        let still = found.and_then(|p| {
            let (w, h, mut px) = p.rgba(rotation, box_w, box_h).ok()?;
            for c in px.chunks_exact_mut(4) {
                let a = c[3] as u32;
                let (r, g, b) = (c[0] as u32, c[1] as u32, c[2] as u32);
                c[0] = ((b * a + 127) / 255) as u8;
                c[1] = ((g * a + 127) / 255) as u8;
                c[2] = ((r * a + 127) / 255) as u8;
            }
            Some(Still { asked: us, width: w, height: h, bgra: px })
        });
        let mut st = lock();
        if st.want == Some(us) {
            st.want = None;
        }
        if still.is_some() {
            st.got = still;
        }
    }
}

// --- Audio (WASAPI, shared mode) ---

fn audio_thread(shared: Arc<Shared>) {
    if let Err(e) = unsafe { run_audio(&shared) } {
        crate::trace::mark(format!("video: no sound: {e}"));
        let mut st = shared.lock();
        st.audio_dead = true;
        st.audio.clear();
        st.audio_frames = 0;
    }
}

unsafe fn run_audio(shared: &Shared) -> windows::core::Result<()> {
    use windows::Win32::Media::Audio::*;
    use windows::Win32::Media::KernelStreaming::{SPEAKER_FRONT_LEFT, SPEAKER_FRONT_RIGHT, WAVE_FORMAT_EXTENSIBLE};
    // KSDATAFORMAT_SUBTYPE_IEEE_FLOAT
    const IEEE_FLOAT: windows::core::GUID = windows::core::GUID::from_u128(0x00000003_0000_0010_8000_00aa00389b71);
    use windows::Win32::System::Com::*;
    use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let devices: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let device = devices.GetDefaultAudioEndpoint(eRender, eConsole)?;
        let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
        let format = WAVEFORMATEXTENSIBLE {
            Format: WAVEFORMATEX {
                wFormatTag: WAVE_FORMAT_EXTENSIBLE as u16,
                nChannels: 2,
                nSamplesPerSec: AUDIO_RATE,
                nAvgBytesPerSec: AUDIO_RATE * 8,
                nBlockAlign: 8,
                wBitsPerSample: 32,
                cbSize: (std::mem::size_of::<WAVEFORMATEXTENSIBLE>() - std::mem::size_of::<WAVEFORMATEX>()) as u16,
            },
            Samples: WAVEFORMATEXTENSIBLE_0 { wValidBitsPerSample: 32 },
            dwChannelMask: SPEAKER_FRONT_LEFT | SPEAKER_FRONT_RIGHT,
            SubFormat: IEEE_FLOAT,
        };
        // 100 ms of buffer; Windows converts our 48 kHz float stereo to the device's own format.
        client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_EVENTCALLBACK | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
            1_000_000,
            0,
            &format.Format,
            None,
        )?;
        let event = CreateEventW(None, false, false, None)?;
        client.SetEventHandle(event)?;
        let size = client.GetBufferSize()? as usize;
        let render: IAudioRenderClient = client.GetService()?;
        let mut started = false;
        // Other speeds: the sound goes through atempo. `t0` is the time of the first sample fed to it since it was
        // (re)made, `out[out_read..]` what it gave that isn't written yet, `written` the frames that have been.
        let mut tempo: Option<(f64, Tempo)> = None;
        let mut out: Vec<f32> = Vec::new();
        let mut out_read = 0usize;
        let mut t0: Option<i64> = None;
        let mut written = 0i64;
        loop {
            WaitForSingleObject(event, 20);
            let now = Instant::now();
            let mut st = shared.lock();
            if st.quit {
                let _ = client.Stop();
                return Ok(());
            }
            if st.audio_reset {
                st.audio_reset = false;
                let _ = client.Stop();
                let _ = client.Reset();
                started = false;
                st.audio_clock = None;
                tempo = None;
                out.clear();
                out_read = 0;
            }
            if !st.playing || !st.audio_live() {
                if started {
                    let _ = client.Stop();
                    started = false;
                }
                // Faster than 1x plays silent: what's decoded is let go as the pictures pass it.
                if st.playing {
                    let t = st.clock(now);
                    drop_before(&mut st, t);
                }
                continue;
            }
            // Starting (again): from where the pictures are.
            if st.audio_clock.is_none() {
                let t = st.clock(now);
                drop_before(&mut st, t);
            }
            let queued = client.GetCurrentPadding()? as usize;
            let room = size.saturating_sub(queued);
            let speed = st.speed;
            if speed != 1.0 {
                if tempo.as_ref().is_none_or(|(s, _)| *s != speed) {
                    match Tempo::new(speed) {
                        Ok(t) => tempo = Some((speed, t)),
                        Err(e) => {
                            crate::trace::mark(format!("video: no sound at {speed}x: {e}"));
                            st.tempo_failed = true;
                            continue;
                        }
                    }
                    out.clear();
                    out_read = 0;
                    t0 = None;
                    written = 0;
                }
                let (_, tp) = tempo.as_mut().unwrap();
                while (out.len() - out_read) / 2 < room {
                    let Some(c) = st.audio.front_mut() else { break };
                    let take = ((c.samples.len() - c.read) / 2).min(4096);
                    if t0.is_none() {
                        t0 = Some(c.pts + (c.read / 2) as i64 * 1_000_000 / AUDIO_RATE as i64);
                    }
                    let r = tp.process(&c.samples[c.read..c.read + take * 2], &mut out);
                    c.read += take * 2;
                    let done = c.read >= c.samples.len();
                    st.audio_frames -= take;
                    if done {
                        st.audio.pop_front();
                    }
                    if r.is_err() {
                        break;
                    }
                }
                let pending = (out.len() - out_read) / 2;
                let n = room.min(pending);
                let gain = if st.muted { 0.0 } else { st.volume * st.volume };
                let media = |frames: f64| (frames * speed * 1e6 / AUDIO_RATE as f64) as i64;
                if n > 0 {
                    let buf = render.GetBuffer(n as u32)? as *mut f32;
                    let o = std::slice::from_raw_parts_mut(buf, n * 2);
                    for (d, v) in o.iter_mut().zip(&out[out_read..out_read + n * 2]) {
                        *d = v * gain;
                    }
                    out_read += n * 2;
                    if out_read >= 1 << 16 {
                        out.drain(..out_read);
                        out_read = 0;
                    }
                    render.ReleaseBuffer(n as u32, 0)?;
                    written += n as i64;
                    let end = t0.unwrap_or(0) + media(written as f64);
                    st.audio_clock = Some((end, media((queued + n) as f64), now));
                    shared.wake.notify_all();
                    if !started {
                        client.Start()?;
                        started = true;
                    }
                } else if let Some((end, _, _)) = st.audio_clock {
                    st.audio_clock = Some((end, media(queued as f64), now));
                    if queued == 0 && pending == 0 && st.audio_frames == 0 && (st.end || st.audio_exhausted) {
                        let t = st.clock(now);
                        st.audio_ended = true;
                        st.set_anchor(t, now);
                        if started {
                            let _ = client.Stop();
                            started = false;
                        }
                    }
                }
                continue;
            }
            if tempo.take().is_some() {
                out.clear();
                out_read = 0;
            }
            let have = st.audio_frames;
            let n = room.min(have);
            if n > 0 {
                let buf = render.GetBuffer(n as u32)? as *mut f32;
                let out = std::slice::from_raw_parts_mut(buf, n * 2);
                let gain = if st.muted { 0.0 } else { st.volume * st.volume };
                let mut written = 0;
                let mut end = st.audio_clock.map_or(0, |(e, _, _)| e);
                while written < n {
                    let Some(c) = st.audio.front_mut() else { break };
                    let take = ((c.samples.len() - c.read) / 2).min(n - written);
                    for i in 0..take * 2 {
                        out[written * 2 + i] = c.samples[c.read + i] * gain;
                    }
                    c.read += take * 2;
                    written += take;
                    end = c.pts + (c.read / 2) as i64 * 1_000_000 / AUDIO_RATE as i64;
                    if c.read >= c.samples.len() {
                        st.audio.pop_front();
                    }
                }
                st.audio_frames -= written;
                render.ReleaseBuffer(written as u32, 0)?;
                let in_device = queued + written;
                st.audio_clock = Some((end, in_device as i64 * 1_000_000 / AUDIO_RATE as i64, now));
                shared.wake.notify_all();
                if !started {
                    client.Start()?;
                    started = true;
                }
            } else if let Some((end, _, _)) = st.audio_clock {
                st.audio_clock = Some((end, queued as i64 * 1_000_000 / AUDIO_RATE as i64, now));
                // Nothing left to play and none coming (the decoder is at the end, or read as far ahead as it may
                // without finding more): the sound is over, the wall clock carries on from here.
                if queued == 0 && have == 0 && (st.end || st.audio_exhausted) {
                    let t = st.clock(now);
                    st.audio_ended = true;
                    st.set_anchor(t, now);
                    if started {
                        let _ = client.Stop();
                        started = false;
                    }
                }
            }
        }
    }
}

/// Lets go of queued audio that ends before `t`.
fn drop_before(st: &mut State, t: i64) {
    while let Some(c) = st.audio.front() {
        let end = c.pts + (c.samples.len() / 2) as i64 * 1_000_000 / AUDIO_RATE as i64;
        if end > t {
            break;
        }
        st.audio_frames -= (c.samples.len() - c.read) / 2;
        st.audio.pop_front();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Waits (up to 3 s) for the next picture `due` hands out.
    fn next_due(p: &mut Player) -> Option<i64> {
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(3) {
            if let Some(d) = p.due(Instant::now()) {
                return Some(d.picture.pts_us);
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        None
    }

    /// Opens `LOOKER_DECODE` on the CPU, paused, and checks the first picture, frame steps and exact seeks.
    /// `LOOKER_CODECS=%LOCALAPPDATA%\Looker\codecs LOOKER_DECODE=x.mp4 cargo test --release -- --ignored --nocapture steps_and_seeks`
    #[test]
    #[ignore]
    fn steps_and_seeks() {
        let path = std::path::PathBuf::from(std::env::var("LOOKER_DECODE").expect("set LOOKER_DECODE"));
        let mut p = Player::open(&path, crate::gfx::Sendable(std::ptr::null_mut()), false, 0.0, true);
        let t = Instant::now();
        while p.poll_open().is_none() {
            assert!(t.elapsed() < Duration::from_secs(3), "never opened");
            std::thread::sleep(Duration::from_millis(2));
        }
        let info = p.info.clone().unwrap();
        let frame = (1e6 * info.fps_den as f64 / info.fps_num as f64) as i64;
        let first = next_due(&mut p).expect("first picture");
        println!("first picture at {first} us, frame {frame} us");
        assert!(first.abs() < frame);
        p.step_forward();
        let second = next_due(&mut p).expect("stepped picture");
        // Within two frames: screen recordings have variable frame rates.
        assert!(second > first && second - first < 2 * frame, "step: {first} -> {second}");
        p.step_forward();
        let third = next_due(&mut p).unwrap();
        p.step_back();
        let back = next_due(&mut p).expect("stepped back");
        assert_eq!(back, second, "step back from {third}");
        for target in [1_500_000i64, 400_000, 2_000_000] {
            let t = Instant::now();
            p.seek(target, true);
            let got = next_due(&mut p).expect("seeked picture");
            println!("seek {target} -> {got} in {:.0} ms", t.elapsed().as_secs_f64() * 1000.0);
            assert!(got <= target && target - got < 2 * frame, "seek {target} landed on {got}");
        }
        assert!(!p.playing());
    }
}
