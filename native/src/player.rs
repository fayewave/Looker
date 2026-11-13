//! Playing a video: a decode thread runs the FFmpeg decoder (imaging/video.rs) a few pictures and about a second
//! of audio ahead, and puts CPU pictures on the GPU as they come (yuv::Uploader); an audio thread feeds WASAPI from
//! that; the UI thread asks, once per vsync, for the picture due when that frame reaches the screen
//! (`Player::due`) and draws it (app/video.rs).
//!
//! The clock is the wall clock, steered by the sound while it plays (at 0.25x to 2x: other speeds go through
//! FFmpeg's atempo, same pitch): the audio device's own position (`IAudioClock`, stamped with the QPC time it was
//! read) pulls it gently towards the sound, so the pictures keep an even pace and stay in sync. Read directly,
//! the device clock moves in steps of its period (10 ms), which at 165 Hz showed some pictures a vsync early.
//!
//! Every seek bumps an epoch: what the decode thread had queued is thrown away, and the first picture of the new
//! epoch is shown at once, playing or paused (scrubbing and frame steps show where they landed).

use std::collections::{BTreeMap, VecDeque};
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use crate::imaging::video::{AUDIO_RATE, Chapter, Decoder, Info, Interrupter, Item, LvFrame, Pass, Picture, TEXTURE, Tempo, Track};
use crate::yuv::{Uploaded, Uploader};

/// A picture in the queue: the decoder's frame, and its planes on the GPU when the decode thread uploaded them
/// (its CPU planes are let go then), or a copy from the scrub cache (`keep` is the texture it points at).
pub struct Frame {
    pub picture: Picture,
    pub uploaded: Option<Uploaded>,
    /// Held, not read: the texture a cached picture points at.
    _keep: Option<Texture>,
}

impl Frame {
    fn decoded(picture: Picture, uploaded: Option<Uploaded>) -> Frame {
        Frame { picture, uploaded, _keep: None }
    }
}

/// A texture held across threads.
#[derive(Clone)]
struct Texture(windows::Win32::Graphics::Direct3D11::ID3D11Texture2D);
// SAFETY: D3D11 resources are free-threaded; the device is multithread-protected.
unsafe impl Send for Texture {}

// --- Scrubbing back: copies of what the GPU decoded ---

/// GPU memory the scrub cache may hold: ~60 pictures of 2.7K, ~30 of 4K 10-bit.
const CACHE_BYTES: usize = 384 << 20;

/// The facts of a cached picture (an LvFrame's, without its pointers).
#[derive(Clone, Copy)]
struct Head {
    pts_us: i64,
    duration_us: i64,
    width: i32,
    height: i32,
    bit_depth: i32,
    shift: i32,
    chroma_w_shift: i32,
    chroma_h_shift: i32,
    matrix: i32,
    full_range: i32,
    transfer: i32,
    primaries: i32,
}

struct Cached {
    tex: Texture,
    head: Head,
    bytes: usize,
}

/// Copies of pictures the GPU decoded while paused (and those an exact seek decoded on its way to its target), by
/// time. H.264/HEVC only decode forwards from a keyframe, so each step back on the seek bar cost a decode from the
/// keyframe (~300 ms on 2.7K drone footage with keyframes 2.4 s apart); from here, a step back into what was
/// decoded is a copy on screen. Within `CACHE_BYTES`, the ones furthest from where the seek bar is go first.
#[derive(Default)]
struct Cache {
    frames: BTreeMap<i64, Cached>,
    bytes: usize,
    /// Where the seek bar was last: what's furthest from it is let go first.
    focus: i64,
    spare: Vec<Texture>,
}

impl Cache {
    /// The cached picture showing at `us`, as a frame for the queue.
    fn frame_at(&mut self, us: i64) -> Option<Frame> {
        let (_, c) = self.frames.range(..=us).next_back()?;
        if us >= c.head.pts_us + c.head.duration_us.max(1) {
            return None;
        }
        self.focus = us;
        Some(texture_frame(c.tex.clone(), &c.head))
    }
}

impl Head {
    fn of(p: &LvFrame) -> Head {
        Head {
            pts_us: p.pts_us,
            duration_us: p.duration_us,
            width: p.width,
            height: p.height,
            bit_depth: p.bit_depth,
            shift: p.shift,
            chroma_w_shift: p.chroma_w_shift,
            chroma_h_shift: p.chroma_h_shift,
            matrix: p.matrix,
            full_range: p.full_range,
            transfer: p.transfer,
            primaries: p.primaries,
        }
    }
}

/// A queue frame showing our own copy `tex` of a GPU picture with the facts `h`.
fn texture_frame(tex: Texture, h: &Head) -> Frame {
    {
        let h = *h;
        let mut f: LvFrame = unsafe { std::mem::zeroed() };
        f.kind = crate::imaging::video::VIDEO;
        f.layout = TEXTURE;
        f.pts_us = h.pts_us;
        f.duration_us = h.duration_us;
        f.width = h.width;
        f.height = h.height;
        f.bit_depth = h.bit_depth;
        f.shift = h.shift;
        f.chroma_w_shift = h.chroma_w_shift;
        f.chroma_h_shift = h.chroma_h_shift;
        f.nplanes = 2;
        f.matrix = h.matrix;
        f.full_range = h.full_range;
        f.transfer = h.transfer;
        f.primaries = h.primaries;
        f.texture = windows::core::Interface::as_raw(&tex.0);
        f.texture_index = 0;
        Frame { picture: Picture::detached(f), uploaded: None, _keep: Some(tex) }
    }
}

impl Cache {
    /// Keeps a copy of GPU picture `p` (when it isn't kept already). `focus`: where the seek bar is (an exact
    /// seek's target while it decodes towards it), else `p` itself: what's furthest from it goes first.
    fn insert(&mut self, up: &Uploader, p: &LvFrame, focus: Option<i64>) {
        self.focus = focus.unwrap_or(p.pts_us);
        if p.layout != TEXTURE || self.frames.contains_key(&p.pts_us) {
            return;
        }
        let bytes = Uploader::surface_bytes(p);
        if bytes == 0 || bytes > CACHE_BYTES / 4 {
            return;
        }
        while self.bytes + bytes > CACHE_BYTES && !self.frames.is_empty() {
            let (first, last) = (*self.frames.keys().next().unwrap(), *self.frames.keys().next_back().unwrap());
            let gone = if (self.focus - first).abs() >= (last - self.focus).abs() { first } else { last };
            if let Some(c) = self.frames.remove(&gone) {
                self.bytes -= c.bytes;
                if self.spare.len() < 4 {
                    self.spare.push(c.tex);
                }
            }
        }
        let Ok(tex) = up.copy_surface(p, self.spare.pop().map(|t| t.0)) else { return };
        let head = Head::of(p);
        self.bytes += bytes;
        self.frames.insert(p.pts_us, Cached { tex: Texture(tex), head, bytes });
    }
}

impl std::ops::Deref for Frame {
    type Target = LvFrame;
    fn deref(&self) -> &LvFrame {
        &self.picture
    }
}

/// How much of the gap between the sound and the wall clock is closed at each of the sound's updates (about
/// 100 a second), and the most that may move the clock each time: the pictures speed up or slow down by 10 % at
/// most while they catch up, which can't be seen.
const STEER: f64 = 0.05;
const STEER_MAX_US: i64 = 1_000;
/// Further apart than this (the sound has just started, or stalled), the clock jumps to the sound instead.
const SNAP_US: i64 = 150_000;
/// The first reading after the clock was set (a seek, play) jumps back to the sound when it's further behind than
/// this (the picture on screen stays a moment longer). Ahead, it's eased into like any other: a jump forward
/// would skip pictures.
const FIRST_SNAP_US: i64 = 20_000;

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
    pictures: VecDeque<Frame>,
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
    /// The sound has steered the clock since it was last set.
    steered: bool,
    /// The first picture of this epoch is on screen: the sound waits for it, so both start together.
    picture_up: bool,
    /// The picture on screen came from the scrub cache: the decoder is still where it was, and goes to this time
    /// (exactly) only when more pictures are wanted (playing, a frame step).
    hold: Option<i64>,
    /// A frame step wants the next picture (the decoder leaves its hold for it).
    need_next: bool,
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
    /// The queued audio's shape: f32s per frame and frames a second (2 and 48 kHz; passthrough: the IEC stream's
    /// channels and rate, each 16-bit sample in an f32). `pass`: passthrough is on (the audio thread opens the
    /// device exclusively for it).
    fpf: usize,
    rate: u32,
    pass: Option<Pass>,

    /// Playing backwards (J): the clock runs down, silent, and the queue holds pictures newest first, decoded a
    /// stretch at a time (`reverse_chunk`). `rev_until`: the decode thread's next stretch ends before this time;
    /// `rev_last`: how many pictures the last stretch had (the next is decoded once fewer are queued). `end` means
    /// the start was reached.
    reverse: bool,
    rev_until: Option<i64>,
    rev_last: usize,
}

impl State {
    /// Media time at `now` (which may be a little ahead: the vsync a frame will be shown at).
    fn clock(&self, now: Instant) -> i64 {
        if !self.playing {
            return self.anchor;
        }
        self.wall(now)
    }

    fn wall(&self, at: Instant) -> i64 {
        let dt = if at >= self.anchor_at { (at - self.anchor_at).as_secs_f64() } else { -(self.anchor_at - at).as_secs_f64() };
        let speed = if self.reverse { -self.speed } else { self.speed };
        self.anchor + (dt * 1e6 * speed) as i64
    }

    /// The sound was at `heard` at `at`: the clock moves towards it (see the module doc).
    fn steer(&mut self, heard: i64, at: Instant) {
        if !self.playing {
            return;
        }
        let err = heard - self.wall(at);
        if err.abs() > SNAP_US || (!self.steered && err < -FIRST_SNAP_US) {
            self.anchor = heard;
            self.anchor_at = at;
        } else {
            self.anchor += ((err as f64 * STEER) as i64).clamp(-STEER_MAX_US, STEER_MAX_US);
        }
        self.steered = true;
    }

    /// Whether the sound is what's playing (0.25x to 2x; faster plays silent).
    fn audio_live(&self) -> bool {
        !self.audio_dead && !self.audio_ended && !self.reverse && (self.speed == 1.0 || (!self.tempo_failed && (0.25..=2.0).contains(&self.speed)))
    }

    /// Re-anchors the wall clock at `t` (keeps the audio clock out until sound plays from there).
    fn set_anchor(&mut self, t: i64, now: Instant) {
        self.anchor = t;
        self.anchor_at = now;
        self.audio_clock = None;
        self.steered = false;
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
        self.picture_up = false;
    }
}

struct Shared {
    state: Mutex<State>,
    /// Wakes the decode thread (room in the queue, a seek, quit).
    wake: Condvar,
    /// Stops the decoder mid-decode for a seek (an exact one decodes from its keyframe up to its target). Set
    /// while the decoder lives: the decode thread takes it away before the decoder is closed.
    interrupt: Mutex<Option<Interrupter>>,
    cache: Mutex<Cache>,
    /// The decoder is the GPU's (long-GOP codecs: an exact seek back decodes from the keyframe).
    gpu: std::sync::atomic::AtomicBool,
}

impl Shared {
    fn cache(&self) -> MutexGuard<'_, Cache> {
        self.cache.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn interrupt(&self) {
        if let Some(i) = self.interrupt.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            i.interrupt();
        }
    }
}

/// Takes the interrupter away when the decode thread ends (declared after the decoder, so dropped before it).
struct InterruptGone<'a>(&'a Shared);

impl Drop for InterruptGone<'_> {
    fn drop(&mut self) {
        *self.0.interrupt.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// What the UI thread draws from: the picture due now, if it changed.
pub struct Due {
    pub picture: Frame,
}

/// What the decode thread hands over once the file is open: its facts, every picture's time, its audio, video and
/// subtitle streams, and the zero of the clock in the file (`Decoder::start_us`).
type Opened = (Info, Vec<i64>, Vec<Track>, Vec<Track>, Vec<Track>, i64, Vec<Chapter>);

pub struct Player {
    shared: Arc<Shared>,
    pub info: Option<Info>,
    /// The epoch and time of the picture on screen.
    shown: Option<(u32, i64, i64)>,
    /// Where the last seek is going, until its picture is up: the bar shows it at once, and jumps add up.
    target: Option<i64>,
    /// Loops instead of stopping at the end (short clips).
    pub looping: bool,
    /// A frame step waiting for its picture.
    step: bool,
    /// A seek to a picture already queued (paused): shown next, the decoder untouched.
    jump: bool,
    /// Every picture's time from the container's index (`Decoder::frame_times`), empty without one.
    frames: Vec<i64>,
    /// The file's audio streams (`Decoder::audio_tracks`), the one playing marked.
    tracks: Vec<Track>,
    /// Its video streams (`Decoder::video_tracks`), the one showing marked.
    vtracks: Vec<Track>,
    /// Its subtitle streams (`Decoder::sub_tracks`), and the clock's zero in the file (for reading them).
    stracks: Vec<Track>,
    start_us: i64,
    /// Its chapters (`Decoder::chapters`), by start time.
    chapters: Vec<Chapter>,
    /// Pictures shown and skipped (late), and the times the next picture wasn't decoded when it was due (each
    /// such wait counted once), for the trace.
    shown_count: u64,
    dropped: u64,
    starved: u64,
    waiting: bool,
    opened: Arc<Mutex<Option<Result<Opened, String>>>>,
}

impl Player {
    /// Starts decoding `path` on its own thread (`d3d11`: the window's device to decode on, or null), playing at
    /// once when `play`.
    #[cfg(test)]
    pub fn open(path: &Path, d3d11: crate::gfx::Sendable<*mut std::ffi::c_void>, play: bool, volume: f32, muted: bool) -> Player {
        Player::open_track(path, d3d11, play, volume, muted, None, None, false)
    }

    /// `open`, showing the video stream `video` and playing the audio stream `audio` (`Track::stream`s), else the
    /// file's best of each. `passthrough`: AC-3/E-AC-3/DTS/TrueHD sound goes to a receiver undecoded, when the output
    /// device takes it.
    #[allow(clippy::too_many_arguments)]
    pub fn open_track(path: &Path, d3d11: crate::gfx::Sendable<*mut std::ffi::c_void>, play: bool, volume: f32, muted: bool, video: Option<i32>, audio: Option<i32>, passthrough: bool) -> Player {
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
                steered: false,
                picture_up: false,
                hold: None,
                need_next: false,
                audio_reset: false,
                audio_dead: false,
                audio_ended: false,
                tempo_failed: false,
                audio_exhausted: false,
                fpf: 2,
                rate: AUDIO_RATE,
                pass: None,
                reverse: false,
                rev_until: None,
                rev_last: 0,
            }),
            wake: Condvar::new(),
            interrupt: Mutex::new(None),
            cache: Mutex::new(Cache::default()),
            gpu: std::sync::atomic::AtomicBool::new(false),
        });
        let opened = Arc::new(Mutex::new(None));
        let (s, p, o) = (shared.clone(), path.to_path_buf(), opened.clone());
        let device = d3d11;
        std::thread::Builder::new()
            .name("video-decode".into())
            .spawn(move || {
                let device = device;
                decode_thread(s, &p, device.0, o, video, audio, passthrough)
            })
            .ok();
        Player { shared, info: None, shown: None, target: None, jump: false, looping: false, step: false, frames: Vec::new(), tracks: Vec::new(), vtracks: Vec::new(), stracks: Vec::new(), start_us: 0, chapters: Vec::new(), shown_count: 0, dropped: 0, starved: 0, waiting: false, opened }
    }

    /// The stream's facts once the decoder is open (`Err` when it couldn't be).
    pub fn poll_open(&mut self) -> Option<Result<(), String>> {
        if self.info.is_some() {
            return Some(Ok(()));
        }
        let r = self.opened.lock().ok()?.take()?;
        Some(r.map(|(i, frames, tracks, vtracks, stracks, start_us, chapters)| {
            self.frames = frames;
            self.chapters = chapters;
            self.tracks = tracks;
            self.vtracks = vtracks;
            self.stracks = stracks;
            self.start_us = start_us;
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

    /// The picture to show at `now` (the vsync the frame being drawn will reach the screen at), when it changed
    /// since the last call; also stops (or loops) at the end.
    pub fn due(&mut self, now: Instant) -> Option<Due> {
        let mut st = self.shared.lock();
        let epoch = st.epoch;
        let fresh = self.shown.is_none_or(|(e, _, _)| e != epoch);
        let mut pick: Option<Frame> = None;
        if self.jump && !fresh {
            // A seek to a queued picture: the queue was cut down to it.
            if let Some(p) = st.pictures.pop_front() {
                self.jump = false;
                self.target = None;
                st.set_anchor(p.pts_us, now);
                pick = Some(p);
            }
        } else if self.step && !fresh {
            // A frame step: the next picture, and the clock moves to it. Never the one on screen again: from a cached
            // picture the decoder is asked for "at or before its end", and with times rounded to the microsecond
            // (120 fps: 8333.33 µs) that end can fall 1 µs short of the next picture, giving this one back (Faye,
            // 2026-11-10: a step that didn't move, mostly on high frame rates after scrubbing).
            let on_screen = self.shown.map(|(_, pts, _)| pts);
            while st.pictures.front().is_some_and(|p| on_screen.is_some_and(|s| p.pts_us <= s)) {
                st.pictures.pop_front();
            }
            if let Some(p) = st.pictures.pop_front() {
                self.step = false;
                st.set_anchor(p.pts_us, now);
                pick = Some(p);
            }
        } else if fresh {
            // The first picture after a seek (or the very first): shown at once, and the clock starts from it
            // (backwards: from its end, so it has its time too).
            if let Some(p) = st.pictures.pop_front() {
                let at = if st.reverse { p.pts_us + p.duration_us.max(1) - 1 } else { p.pts_us };
                st.set_anchor(at, now);
                st.picture_up = true;
                self.target = None;
                pick = Some(p);
            } else if st.reverse && st.end {
                // Backwards from the very first picture: nothing before it.
                drop(st);
                self.leave_reverse(false);
                return None;
            }
        } else if st.reverse {
            // Backwards: the queue runs newest first; each picture is due once the clock is below its end.
            let t = st.clock(now);
            while st.pictures.front().is_some_and(|p| t < p.pts_us + p.duration_us.max(1)) {
                if pick.is_some() {
                    self.dropped += 1;
                }
                pick = st.pictures.pop_front();
            }
            let overdue = self.shown.is_some_and(|(_, pts, _)| t < pts);
            if pick.is_none() && st.playing && !st.end && st.pictures.is_empty() && overdue {
                if !self.waiting {
                    self.starved += 1;
                    self.waiting = true;
                }
            } else if pick.is_some() {
                self.waiting = false;
            }
            // At the start: stop on the first picture.
            if pick.is_none() && st.end && st.pictures.is_empty() && overdue {
                drop(st);
                self.leave_reverse(false);
                return None;
            }
        } else {
            let t = st.clock(now);
            // The one on screen again (the decoder leaving a cached picture's hold gives it back): not shown twice.
            let on_screen = self.shown.map(|(_, pts, _)| pts);
            while st.pictures.front().is_some_and(|p| on_screen.is_some_and(|s| p.pts_us <= s)) {
                st.pictures.pop_front();
            }
            while st.pictures.front().is_some_and(|p| p.pts_us <= t) {
                if pick.is_some() {
                    self.dropped += 1;
                }
                pick = st.pictures.pop_front();
            }
            // The picture on screen has had its time and the next isn't decoded yet.
            let overdue = self.shown.is_some_and(|(_, pts, dur)| t >= pts + dur.max(1));
            if pick.is_none() && st.playing && !st.end && st.pictures.is_empty() && overdue {
                if !self.waiting {
                    self.starved += 1;
                    self.waiting = true;
                }
            } else if pick.is_some() {
                self.waiting = false;
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
        if st.reverse {
            drop(st);
            self.leave_reverse(play);
            return;
        }
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

    /// Where a seek is going, else the time of the picture on screen.
    pub fn position(&self) -> i64 {
        self.target.unwrap_or_else(|| self.shown.map_or(0, |(_, pts, _)| pts))
    }

    /// The picture at `us` counted from 1, and how many there are, from the container's index: exact whatever
    /// the frame rate does (the nearest listed time). None when the container doesn't list every picture.
    pub fn frame_number(&self, us: i64) -> Option<(usize, usize)> {
        frame_number_in(&self.frames, us).map(|i| (i + 1, self.frames.len()))
    }

    /// The file's audio streams, the one playing marked (empty until it has opened, or without sound).
    pub fn audio_tracks(&self) -> &[Track] {
        &self.tracks
    }

    /// The file's subtitle streams (empty until it has opened).
    pub fn sub_tracks(&self) -> &[Track] {
        &self.stracks
    }

    /// The file's chapters, by start time (empty until it has opened, or without any).
    pub fn chapters(&self) -> &[Chapter] {
        &self.chapters
    }

    /// Where the clock's zero (the video stream's start) is in the file, in µs: subtitle streams read on their own
    /// are put on the clock with it.
    pub fn start_us(&self) -> i64 {
        self.start_us
    }

    /// The file's video streams, the one showing marked (empty until it has opened).
    pub fn video_tracks(&self) -> &[Track] {
        &self.vtracks
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
        // Playing backwards: on backwards from there.
        if st.reverse {
            st.clear();
            st.epoch = st.epoch.wrapping_add(1);
            st.hold = None;
            st.seek = None;
            st.rev_until = Some(us + 1);
            st.rev_last = 0;
            st.set_anchor(us, now);
            drop(st);
            self.jump = false;
            self.target = Some(us);
            self.shared.wake.notify_all();
            self.shared.interrupt();
            return;
        }
        // Paused and exact (dragging the seek bar slowly): the picture on screen or one already decoded may be the
        // one, and then the decoder isn't touched at all.
        if exact && !st.playing && st.seek.is_none() && !self.step {
            if let Some((_, pts, dur)) = self.shown.filter(|s| s.0 == st.epoch) {
                if us >= pts && us < pts + dur.max(1) {
                    self.jump = false;
                    self.target = None;
                    st.set_anchor(pts, now);
                    return;
                }
                if us > pts {
                    if let Some(i) = st.pictures.iter().rposition(|f| f.pts_us <= us) {
                        let f = &st.pictures[i];
                        // The last one queued: its end is only a guess until the next is decoded.
                        if i + 1 < st.pictures.len() || us < f.pts_us + f.duration_us.max(1) {
                            st.pictures.drain(..i);
                            st.set_anchor(us, now);
                            drop(st);
                            self.jump = true;
                            self.target = Some(us);
                            self.shared.wake.notify_all();
                            return;
                        }
                    }
                }
            }
        }
        drop(st);
        self.reposition(us, exact);
    }

    /// The decoder goes to `us` (a new epoch): the seek proper, without `seek`'s shortcuts.
    fn reposition(&mut self, us: i64, exact: bool) {
        let now = Instant::now();
        let mut st = self.shared.lock();
        self.jump = false;
        st.clear();
        st.epoch = st.epoch.wrapping_add(1);
        st.set_anchor(us, now);
        // Paused and exact, and a copy of that picture is cached (scrubbing back): on screen at once, and the
        // decoder stays put until more pictures are wanted.
        let cached = if exact && !st.playing { self.shared.cache().frame_at(us) } else { None };
        if let Some(f) = cached {
            st.hold = Some(f.pts_us + f.duration_us.max(1));
            st.seek = None;
            st.pictures.push_back(f);
        } else {
            st.hold = None;
            st.seek = Some((us, exact));
        }
        drop(st);
        self.target = Some(us);
        self.shared.wake.notify_all();
        // A decode still on its way to an older target gives up.
        self.shared.interrupt();
    }

    /// Pauses and shows the next picture (at the next `due`).
    pub fn step_forward(&mut self) {
        self.set_playing(false);
        self.step = true;
        self.shared.lock().need_next = true;
        self.shared.wake.notify_all();
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

    /// Playing backwards.
    pub fn reverse(&self) -> bool {
        self.shared.lock().reverse
    }

    /// Plays backwards at `speed` from the picture on screen (or where a seek is going), silent; already
    /// backwards, only the speed changes. The decode thread decodes a stretch before it from its keyframe and
    /// hands it over newest first, so the first picture comes after one stretch's decode.
    pub fn play_reverse(&mut self, speed: f64) {
        let now = Instant::now();
        let mut st = self.shared.lock();
        if st.reverse {
            let t = st.clock(now);
            st.speed = speed;
            st.set_anchor(t, now);
            return;
        }
        let Some(from) = self.target.or(self.shown.map(|(_, pts, _)| pts)) else { return };
        st.clear();
        st.epoch = st.epoch.wrapping_add(1);
        st.hold = None;
        st.seek = None;
        st.need_next = false;
        st.reverse = true;
        st.rev_until = Some(if self.target.is_some() { from + 1 } else { from });
        st.rev_last = 0;
        st.playing = true;
        st.speed = speed;
        st.set_anchor(from, now);
        drop(st);
        self.jump = false;
        self.step = false;
        self.target = None;
        self.shared.wake.notify_all();
        self.shared.interrupt();
    }

    /// Stops playing backwards, on the picture on screen: the decoder is put back there (exactly), and plays
    /// forwards from it when `play`.
    fn leave_reverse(&mut self, play: bool) {
        let pos = self.shown.map_or(0, |(_, pts, _)| pts);
        {
            let mut st = self.shared.lock();
            st.reverse = false;
            st.rev_until = None;
            st.rev_last = 0;
            st.playing = false;
            st.speed = 1.0; // backwards speeds don't carry over
        }
        self.reposition(pos, true);
        if play {
            self.shared.lock().playing = true;
        }
    }

    /// Whether an exact seek to `us` is quick: forward of the picture on screen (the decoder carries on), into the
    /// scrub cache, or on a CPU decoder (ProRes and DNx pictures stand alone). Back on the GPU past the cache it
    /// decodes from the keyframe (a few hundred ms on 2.7K drone footage): scrubbing goes to the keyframe first then.
    pub fn exact_is_quick(&self, us: i64) -> bool {
        us >= self.position() || !self.shared.gpu.load(std::sync::atomic::Ordering::Relaxed) || self.shared.cache().frame_at(us).is_some()
    }

    /// The picture for the last seek is on screen (scrubbing waits for it before seeking again).
    pub fn settled(&self) -> bool {
        let epoch = self.shared.lock().epoch;
        !self.jump && self.shown.is_some_and(|(e, _, _)| e == epoch)
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
        crate::trace::mark(format!(
            "video: closed after {} pictures shown, {} skipped late, {} waits for the decoder; clock {clock}",
            self.shown_count, self.dropped, self.starved
        ));
        // The threads see this and end on their own (never joined: a decode may be mid-picture).
        self.shared.lock().quit = true;
        self.shared.wake.notify_all();
    }
}

/// The index in sorted `frames` nearest `us`, or None for an empty list.
fn frame_number_in(frames: &[i64], us: i64) -> Option<usize> {
    if frames.is_empty() {
        return None;
    }
    let i = frames.partition_point(|&t| t < us);
    Some(match (i.checked_sub(1), frames.get(i)) {
        (Some(a), Some(&b)) => if us - frames[a] <= b - us { a } else { i },
        (Some(a), None) => a,
        (None, _) => 0,
    })
}

fn decode_thread(shared: Arc<Shared>, path: &Path, d3d11: *mut std::ffi::c_void, opened: Arc<Mutex<Option<Result<Opened, String>>>>, video: Option<i32>, audio: Option<i32>, passthrough: bool) {
    let t = Instant::now();
    // CPU pictures go on the window's device from here, so the UI thread only draws them.
    let uploader = unsafe { <windows::Win32::Graphics::Direct3D11::ID3D11Device as windows::core::Interface>::from_raw_borrowed(&d3d11) }.and_then(|d| Uploader::new(d).ok());
    let mut upload_failed = false;
    // Passthrough: the codecs the output device takes as IEC 61937 (asked before the file is opened).
    let pass_mask = if passthrough { unsafe { passthrough_codecs() } } else { 0 };
    let mut dec = match Decoder::open_playing(path, d3d11, pass_mask, video, audio) {
        Ok((d, info)) => {
            if let Some(p) = d.passthrough() {
                crate::trace::mark(format!("video: passthrough {:?}", p));
                let mut st = shared.lock();
                st.pass = Some(p);
                st.fpf = p.channels as usize;
                st.rate = p.rate;
            }
            crate::trace::mark(format!(
                "video: opened {} ({}) in {:.0} ms",
                crate::imaging::video::codec_label(&info.codec(), &info.profile()),
                if d.hardware() { "GPU" } else { "CPU" },
                t.elapsed().as_secs_f64() * 1000.0
            ));
            if let Ok(mut o) = opened.lock() {
                *o = Some(Ok((info, d.frame_times(), d.audio_tracks(), d.video_tracks(), d.sub_tracks(), d.start_us(), d.chapters())));
            }
            *shared.interrupt.lock().unwrap_or_else(|e| e.into_inner()) = Some(d.interrupter());
            shared.gpu.store(d.hardware(), std::sync::atomic::Ordering::Relaxed);
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
    let _gone = InterruptGone(&shared);
    // Where the decoder is: the end of the last picture it gave (None right after a seek). Exact seeks on the GPU
    // just ahead of it decode on from here instead of from the keyframe.
    let mut dec_pos: Option<i64> = None;
    let gpu = dec.hardware();
    // Playing backwards: the first picture's time (nothing before it), and how far before a stretch's end its
    // seek aims (grows while stretches come out short: intra-only codecs give one picture from a seek just
    // before).
    let floor = dec.frame_times().first().copied().unwrap_or(0).max(0);
    let mut lookback: i64 = 0;
    loop {
        // A seek or quit first; then wait while the queues are full (or at the end).
        let (pending, sound_only, paused, reverse, epoch_at) = {
            let mut st = shared.lock();
            let mut sound_only = false;
            let mut reverse = None;
            loop {
                if st.quit {
                    return;
                }
                if st.seek.is_some() {
                    break;
                }
                // Backwards: the next stretch once the queue holds less than the last one.
                if st.reverse {
                    if let Some(until) = st.rev_until.filter(|_| st.playing && !st.end && st.pictures.len() < st.rev_last.max(1)) {
                        reverse = Some((until, st.epoch));
                        break;
                    }
                    st = shared.wake.wait_timeout(st, Duration::from_millis(100)).map(|(g, _)| g).unwrap_or_else(|e| e.into_inner().0);
                    continue;
                }
                // Showing a cached picture: the decoder moves only when more pictures are wanted.
                if st.hold.is_some() {
                    if st.playing || st.need_next {
                        break;
                    }
                    st = shared.wake.wait_timeout(st, Duration::from_millis(100)).map(|(g, _)| g).unwrap_or_else(|e| e.into_inner().0);
                    continue;
                }
                let n = st.pictures.len();
                let audio_full = st.audio_frames >= MAX_AUDIO * st.rate as usize / AUDIO_RATE as usize;
                // The device drains the audio on its own; with little of it queued, the decoder goes on (up to the
                // hard cap), then reads ahead for the sound alone, so it doesn't run dry behind the pictures (some
                // files store their sound a second or more after the pictures it goes with).
                let starving = !st.audio_dead && !st.audio_ended && !st.audio_exhausted && st.audio_frames < st.rate as usize / 2;
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
            let held = st.hold.take().map(|h| (h, true));
            if held.is_some() {
                st.need_next = false;
            }
            (st.seek.take().or(held).map(|s| (s, st.epoch)), sound_only, !st.playing, reverse, st.epoch)
        };
        if let (None, Some((until, epoch))) = (&pending, reverse) {
            dec_pos = None;
            let r = reverse_chunk(&mut dec, &shared, until, epoch, floor, &mut lookback, uploader.as_ref());
            let mut st = shared.lock();
            if st.quit {
                return;
            }
            if !st.reverse || st.epoch != epoch || st.seek.is_some() {
                continue;
            }
            match r {
                Rev::Got(frames, earliest, start) => {
                    st.rev_last = frames.len();
                    st.pictures.extend(frames.into_iter().rev());
                    st.rev_until = Some(earliest);
                    st.end = start;
                }
                Rev::Start => st.end = true,
                Rev::Cancelled => {}
            }
            continue;
        }
        if let Some(((us, exact), epoch)) = pending {
            if exact && gpu {
                exact_on_gpu(&mut dec, &shared, us, epoch, &mut dec_pos, uploader.as_ref());
            } else {
                if let Err(e) = dec.seek(us, exact) {
                    crate::trace::mark(format!("video: seek: {e}"));
                }
                dec_pos = None;
            }
            continue;
        }
        if sound_only {
            let item = dec.next_audio();
            let mut st = shared.lock();
            if st.quit {
                return;
            }
            if st.seek.is_some() || st.epoch != epoch_at || st.reverse {
                continue;
            }
            match item {
                Ok(Some(Item::Audio(pts, samples))) if !samples.is_empty() => {
                    st.audio_frames += samples.len() / st.fpf;
                    st.audio.push_back(Chunk { pts, samples, read: 0 });
                }
                Ok(Some(_)) => {}
                Ok(None) | Err(_) => st.audio_exhausted = true,
            }
            continue;
        }
        let mut item = dec.next();
        if let Ok(Item::Picture(p)) = &item {
            dec_pos = Some(p.pts_us + p.duration_us.max(1));
            // Paused: what's decoded ahead is kept for scrubbing back.
            if paused && p.layout == TEXTURE {
                if let Some(u) = &uploader {
                    shared.cache().insert(u, p, None);
                }
            }
        }
        let mut uploaded = None;
        if let (Ok(Item::Picture(p)), Some(u)) = (&mut item, &uploader) {
            if p.layout != TEXTURE && !upload_failed {
                match u.upload(p) {
                    Ok(up) => {
                        p.release();
                        uploaded = Some(up);
                    }
                    Err(e) => {
                        crate::trace::mark(format!("video: upload: {e}"));
                        upload_failed = true; // the UI thread uploads them then
                    }
                }
            }
        }
        let mut st = shared.lock();
        if st.quit {
            return;
        }
        if st.seek.is_some() || st.epoch != epoch_at || st.reverse {
            continue; // decoded for the old position (or before J took it backwards): dropped
        }
        match item {
            Ok(Item::Picture(p)) => {
                st.pictures.push_back(Frame::decoded(p, uploaded));
                st.audio_exhausted = false; // a picture further on: the sound may be in reach again
            }
            Ok(Item::Audio(pts, samples)) => {
                if !st.audio_dead && !samples.is_empty() {
                    st.audio_frames += samples.len() / st.fpf;
                    st.audio.push_back(Chunk { pts, samples, read: 0 });
                }
            }
            Ok(Item::End) => st.end = true,
            Ok(Item::Interrupted) => {
                // A seek or a cached picture is waiting (taken at the top); else the interrupt came too late.
                if st.seek.is_none() && st.hold.is_none() {
                    drop(st);
                    dec.resume();
                }
            }
            Err(e) => {
                crate::trace::mark(format!("video: decode: {e}"));
                st.error = Some(e);
                st.end = true;
            }
        }
    }
}

/// An exact seek on the GPU, done here rather than in the decoder so every picture on the way is cached (stepping
/// back to them is then instant): to the keyframe before `us` (or, when the decoder is just before `us`, on from
/// where it is), then decoding forwards; the last picture at or before `us` is the one shown. Gives up when a newer
/// seek comes (epoch `epoch` is over).
fn exact_on_gpu(dec: &mut Decoder, shared: &Shared, us: i64, epoch: u32, dec_pos: &mut Option<i64>, up: Option<&Uploader>) {
    let near = dec_pos.is_some_and(|d| us >= d && us - d <= 2_000_000);
    if !near {
        if let Err(e) = dec.seek(us, false) {
            crate::trace::mark(format!("video: seek: {e}"));
        }
        *dec_pos = None;
    }
    let mut best: Option<Picture> = None;
    loop {
        {
            let st = shared.lock();
            if st.quit || st.seek.is_some() || st.epoch != epoch {
                return;
            }
        }
        match dec.next() {
            Ok(Item::Picture(p)) => {
                *dec_pos = Some(p.pts_us + p.duration_us.max(1));
                if p.pts_us > us {
                    let Some(b) = best.take() else {
                        // The keyframe landed past the target (an index that's off): the decoder's own exact seek.
                        drop(p);
                        let _ = dec.seek(us, true);
                        *dec_pos = None;
                        return;
                    };
                    let mut st = shared.lock();
                    if st.seek.is_none() && st.epoch == epoch {
                        st.pictures.push_back(Frame::decoded(b, None));
                        st.pictures.push_back(Frame::decoded(p, None));
                        st.audio_exhausted = false;
                    }
                    return;
                }
                if let Some(u) = up {
                    shared.cache().insert(u, &p, Some(us));
                }
                best = Some(p);
            }
            Ok(Item::Audio(pts, samples)) => {
                let mut st = shared.lock();
                let end = pts + (samples.len() / st.fpf) as i64 * 1_000_000 / st.rate as i64;
                if end > us && !st.audio_dead && !samples.is_empty() && st.epoch == epoch {
                    st.audio_frames += samples.len() / st.fpf;
                    st.audio.push_back(Chunk { pts, samples, read: 0 });
                }
            }
            Ok(Item::End) => {
                let mut st = shared.lock();
                if st.epoch == epoch {
                    if let Some(b) = best.take() {
                        st.pictures.push_back(Frame::decoded(b, None));
                    }
                    st.end = true;
                }
                return;
            }
            Ok(Item::Interrupted) => {
                // For a newer seek, the top of the loop leaves; else it came too late to matter.
                let st = shared.lock();
                if !(st.quit || st.seek.is_some() || st.epoch != epoch) {
                    drop(st);
                    dec.resume();
                }
            }
            Err(e) => {
                crate::trace::mark(format!("video: decode: {e}"));
                let mut st = shared.lock();
                st.error = Some(e);
                st.end = true;
                return;
            }
        }
    }
}

// --- Playing backwards ---

/// GPU memory one backwards stretch may hold (two are about at once: the one showing, the next decoding): ~120
/// pictures of 1080p, ~15 of 4K 10-bit. A longer GOP is done in parts, each decoding from the keyframe again.
const REV_BYTES: usize = 384 << 20;

enum Rev {
    /// The pictures of a stretch, oldest first; the first one's time; whether nothing comes before it.
    Got(Vec<Frame>, i64, bool),
    /// Nothing before `until`: the start.
    Start,
    /// A seek, a stop or quit came.
    Cancelled,
}

/// Decodes the stretch of pictures before `until` for playing backwards: from the keyframe before it (aimed
/// `lookback` earlier) forwards, keeping copies of its own (GPU pictures copied off the decoder's surfaces, CPU
/// ones uploaded) up to `REV_BYTES`, the oldest let go past that.
fn reverse_chunk(dec: &mut Decoder, shared: &Shared, until: i64, epoch: u32, floor: i64, lookback: &mut i64, up: Option<&Uploader>) -> Rev {
    let cancelled = || {
        let st = shared.lock();
        st.quit || !st.reverse || st.epoch != epoch || st.seek.is_some()
    };
    if until <= floor {
        return Rev::Start;
    }
    let mut back = *lookback;
    loop {
        let target = (until - 1 - back).max(0);
        if let Err(e) = dec.seek(target, false) {
            crate::trace::mark(format!("video: reverse seek: {e}"));
            return Rev::Start;
        }
        let mut kept: VecDeque<(Frame, usize)> = VecDeque::new();
        let mut bytes = 0;
        let mut evicted = false;
        loop {
            if cancelled() {
                return Rev::Cancelled;
            }
            match dec.next() {
                Ok(Item::Picture(p)) => {
                    if p.pts_us >= until {
                        break;
                    }
                    let (f, b) = own_picture(p, up);
                    kept.push_back((f, b));
                    bytes += b;
                    while bytes > REV_BYTES && kept.len() > 1 {
                        if let Some((_, b0)) = kept.pop_front() {
                            bytes -= b0;
                        }
                        evicted = true;
                    }
                }
                Ok(Item::Audio(..)) => {}
                Ok(Item::End) => break,
                Ok(Item::Interrupted) => {
                    if cancelled() {
                        return Rev::Cancelled;
                    }
                    dec.resume();
                }
                Err(e) => {
                    crate::trace::mark(format!("video: reverse decode: {e}"));
                    return Rev::Start;
                }
            }
        }
        if let Some(earliest) = kept.front().map(|k| k.0.pts_us) {
            *lookback = if evicted {
                0
            } else if kept.len() < 12 {
                (back * 2).clamp(250_000, 4_000_000)
            } else {
                back
            };
            let start = earliest <= floor || (!evicted && target <= floor);
            return Rev::Got(kept.into_iter().map(|k| k.0).collect(), earliest, start);
        }
        // The keyframe landed at or past `until` (an index that's off): aim further back.
        if target <= floor {
            return Rev::Start;
        }
        back = (back * 4).max(1_000_000);
        if back > 60_000_000 {
            return Rev::Start;
        }
    }
}

/// A decoded picture as a queue frame that owns its pixels, and about the bytes it holds: GPU pictures are
/// copied off the decoder's surfaces (held, they'd starve its pool), CPU ones uploaded.
fn own_picture(mut p: Picture, up: Option<&Uploader>) -> (Frame, usize) {
    if p.layout == TEXTURE {
        let b = Uploader::surface_bytes(&p);
        if let Some(Ok(tex)) = up.map(|u| u.copy_surface(&p, None)) {
            let h = Head::of(&p);
            drop(p);
            return (texture_frame(Texture(tex), &h), b);
        }
        // No copy: few of these may be held.
        return (Frame::decoded(p, None), REV_BYTES / 8);
    }
    let per = if p.bit_depth > 8 { 2 } else { 1 };
    let b = p.width.max(0) as usize * p.height.max(0) as usize * per * 3;
    if let Some(Ok(x)) = up.map(|u| u.upload(&p)) {
        p.release();
        return (Frame::decoded(p, Some(x)), b);
    }
    (Frame::decoded(p, None), b)
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
    /// Opens `path` for pictures fitted into `box_w × box_h`, turned `rotation` degrees, decoded on `d3d11`'s GPU
    /// where it can (null: the CPU; a 4K HEVC keyframe took ~150 ms there, on one thread).
    pub fn open(path: &Path, rotation: i32, box_w: u32, box_h: u32, d3d11: crate::gfx::Sendable<*mut std::ffi::c_void>) -> Preview {
        let shared = Arc::new(PreviewShared { state: Mutex::new(PreviewState::default()), wake: Condvar::new() });
        let (s, p) = (shared.clone(), path.to_path_buf());
        std::thread::Builder::new()
            .name("video-preview".into())
            .spawn(move || {
                let d3d11 = d3d11;
                preview_thread(&s, &p, rotation, box_w, box_h, d3d11.0)
            })
            .ok();
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

fn preview_thread(shared: &PreviewShared, path: &Path, rotation: i32, box_w: u32, box_h: u32, d3d11: *mut std::ffi::c_void) {
    let lock = || shared.state.lock().unwrap_or_else(|e| e.into_inner());
    let mut dec = match Decoder::open(path, d3d11, true) {
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
    let pass = shared.lock().pass;
    let r = match pass {
        Some(p) => unsafe { run_passthrough(&shared, p) },
        None => unsafe { run_audio(&shared) },
    };
    if let Err(e) = r {
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
    use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
    use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        // MMCSS, as players do: the sound keeps its timing while a CPU decoder (4K ProRes) has every core busy.
        let mut task = 0u32;
        let _ = windows::Win32::System::Threading::AvSetMmThreadCharacteristicsW(windows::core::w!("Audio"), &mut task);
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
        let audio_clock: IAudioClock = client.GetService()?;
        let clock_rate = audio_clock.GetFrequency()?.max(1) as f64;
        let mut qpf = 0i64;
        let _ = QueryPerformanceFrequency(&mut qpf);
        // Frames written to the device since it was last reset, and the time just after the last one.
        let mut dev_written = 0i64;
        let mut dev_end = 0i64;
        // Where the device is now in media time, from its position (stamped in QPC ticks of 100 ns): what was
        // written less what it hasn't played yet; `per_frame` is the media time a frame stands for.
        let heard = |dev_written: i64, dev_end: i64, per_frame: f64| -> Option<(i64, Instant)> {
            let (mut pos, mut qpc) = (0u64, 0u64);
            audio_clock.GetPosition(&mut pos, Some(&mut qpc)).ok()?;
            let mut now_qpc = 0i64;
            let _ = QueryPerformanceCounter(&mut now_qpc);
            let now = Instant::now();
            let now_100ns = (now_qpc as f64 * 1e7 / qpf.max(1) as f64) as u64;
            let at = now.checked_sub(Duration::from_nanos(now_100ns.saturating_sub(qpc) * 100))?;
            let played = pos as f64 * AUDIO_RATE as f64 / clock_rate;
            Some((dev_end - ((dev_written as f64 - played).max(0.0) * per_frame) as i64, at))
        };
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
                dev_written = 0;
                started = false;
                st.audio_clock = None;
                tempo = None;
                out.clear();
                out_read = 0;
            }
            if !st.playing || !st.audio_live() || !st.picture_up {
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
                    dev_written += n as i64;
                    let end = t0.unwrap_or(0) + media(written as f64);
                    dev_end = end;
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
                if started {
                    if let Some((t, at)) = heard(dev_written, dev_end, speed * 1e6 / AUDIO_RATE as f64) {
                        st.steer(t, at);
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
                dev_written += written as i64;
                dev_end = end;
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
            if started {
                if let Some((t, at)) = heard(dev_written, dev_end, 1e6 / AUDIO_RATE as f64) {
                    st.steer(t, at);
                }
            }
        }
    }
}

// --- Passthrough: IEC 61937 to a receiver (WASAPI, exclusive mode) ---

/// An IEC 61937 stream format (`WAVEFORMATEXTENSIBLE_IEC61937`).
#[repr(C, packed(1))]
struct Iec61937 {
    ext: windows::Win32::Media::Audio::WAVEFORMATEXTENSIBLE,
    encoded_rate: u32,
    encoded_channels: u32,
    average_bytes: u32,
}

/// The format a receiver gets `codec` (`PASS_*`) in: its KSDATAFORMAT_SUBTYPE_IEC61937_*, at the IEC rate.
fn iec_format(codec: i32, rate: u32, channels: u16) -> Option<Iec61937> {
    use crate::imaging::video::{PASS_AC3, PASS_DTS, PASS_EAC3, PASS_TRUEHD};
    use windows::Win32::Media::Audio::{WAVEFORMATEX, WAVEFORMATEXTENSIBLE, WAVEFORMATEXTENSIBLE_0};
    use windows::Win32::Media::KernelStreaming::WAVE_FORMAT_EXTENSIBLE;
    use windows::core::GUID;
    let (sub, encoded_rate, encoded_channels) = match codec {
        PASS_AC3 => (GUID::from_u128(0x00000092_0000_0010_8000_00aa00389b71), rate, 6),
        PASS_EAC3 => (GUID::from_u128(0x0000000a_0cea_0010_8000_00aa00389b71), rate / 4, 8),
        PASS_DTS => (GUID::from_u128(0x00000008_0000_0010_8000_00aa00389b71), rate, 6),
        PASS_TRUEHD => (GUID::from_u128(0x0000000b_0cea_0010_8000_00aa00389b71), 48_000, 8),
        _ => return None,
    };
    let block = channels * 2;
    Some(Iec61937 {
        ext: WAVEFORMATEXTENSIBLE {
            Format: WAVEFORMATEX {
                wFormatTag: WAVE_FORMAT_EXTENSIBLE as u16,
                nChannels: channels,
                nSamplesPerSec: rate,
                nAvgBytesPerSec: rate * block as u32,
                nBlockAlign: block,
                wBitsPerSample: 16,
                cbSize: (std::mem::size_of::<Iec61937>() - std::mem::size_of::<WAVEFORMATEX>()) as u16,
            },
            Samples: WAVEFORMATEXTENSIBLE_0 { wValidBitsPerSample: 16 },
            // Front left and right; 7.1 for TrueHD's eight (HDMI's high bit rate).
            dwChannelMask: if channels == 8 { 0x63f } else { 0x3 },
            SubFormat: sub,
        },
        encoded_rate,
        encoded_channels,
        average_bytes: 0,
    })
}

/// The codecs (`PASS_*`) the default output device takes as IEC 61937 in exclusive mode: a receiver or a TV
/// behind HDMI or S/PDIF says yes; speakers and headphones say no, and their sound is decoded as ever.
unsafe fn passthrough_codecs() -> i32 {
    use crate::imaging::video::{PASS_AC3, PASS_DTS, PASS_EAC3, PASS_TRUEHD};
    use windows::Win32::Media::Audio::*;
    use windows::Win32::System::Com::*;
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let Ok(devices) = CoCreateInstance::<_, IMMDeviceEnumerator>(&MMDeviceEnumerator, None, CLSCTX_ALL) else { return 0 };
        let Ok(device) = devices.GetDefaultAudioEndpoint(eRender, eConsole) else { return 0 };
        let Ok(client) = device.Activate::<IAudioClient>(CLSCTX_ALL, None) else { return 0 };
        let mut mask = 0;
        for (codec, rate, ch) in [(PASS_AC3, 48_000, 2), (PASS_EAC3, 192_000, 2), (PASS_DTS, 48_000, 2), (PASS_TRUEHD, 192_000, 8)] {
            let Some(f) = iec_format(codec, rate, ch) else { continue };
            if client.IsFormatSupported(AUDCLNT_SHAREMODE_EXCLUSIVE, &f.ext.Format, None) == windows::Win32::Foundation::S_OK {
                mask |= codec;
            }
        }
        mask
    }
}

/// The audio thread with passthrough: the device opened exclusively in the IEC format, fed the bursts as they
/// were muxed, and its clock steering the pictures as `run_audio`'s does. No volume (the receiver has it); muted
/// sends silence; at other speeds it plays silent (a receiver can't speed Dolby up).
unsafe fn run_passthrough(shared: &Shared, pass: Pass) -> windows::core::Result<()> {
    use windows::Win32::Media::Audio::*;
    use windows::Win32::System::Com::*;
    use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
    use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let mut task = 0u32;
        let _ = windows::Win32::System::Threading::AvSetMmThreadCharacteristicsW(windows::core::w!("Pro Audio"), &mut task);
        let format = iec_format(pass.codec, pass.rate, pass.channels).ok_or_else(|| windows::core::Error::from(windows::Win32::Foundation::E_INVALIDARG))?;
        let devices: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let device = devices.GetDefaultAudioEndpoint(eRender, eConsole)?;
        let mut client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
        let (mut period, mut min) = (0i64, 0i64);
        client.GetDevicePeriod(Some(&mut period), Some(&mut min))?;
        let period = period.max(100_000); // 10 ms at least: bursts are 32 ms (AC-3) and larger
        // Exclusive event mode wants the period and the buffer equal, and some devices want that aligned: when
        // it isn't, try again at the size the device asks for.
        if let Err(e) = client.Initialize(AUDCLNT_SHAREMODE_EXCLUSIVE, AUDCLNT_STREAMFLAGS_EVENTCALLBACK, period, period, &format.ext.Format, None) {
            if e.code() != AUDCLNT_E_BUFFER_SIZE_NOT_ALIGNED {
                return Err(e);
            }
            let frames = client.GetBufferSize()? as i64;
            let aligned = (10_000_000.0 * frames as f64 / pass.rate as f64 + 0.5) as i64;
            client = device.Activate(CLSCTX_ALL, None)?;
            client.Initialize(AUDCLNT_SHAREMODE_EXCLUSIVE, AUDCLNT_STREAMFLAGS_EVENTCALLBACK, aligned, aligned, &format.ext.Format, None)?;
        }
        let event = CreateEventW(None, false, false, None)?;
        client.SetEventHandle(event)?;
        let size = client.GetBufferSize()? as usize;
        let render: IAudioRenderClient = client.GetService()?;
        let audio_clock: IAudioClock = client.GetService()?;
        let clock_rate = audio_clock.GetFrequency()?.max(1) as f64;
        let mut qpf = 0i64;
        let _ = QueryPerformanceFrequency(&mut qpf);
        let (rate, fpf) = (pass.rate as i64, pass.channels as usize);
        let per_frame = 1e6 / rate as f64;
        let mut dev_written = 0i64;
        let mut dev_end = 0i64;
        let heard = |dev_written: i64, dev_end: i64| -> Option<(i64, Instant)> {
            let (mut pos, mut qpc) = (0u64, 0u64);
            audio_clock.GetPosition(&mut pos, Some(&mut qpc)).ok()?;
            let mut now_qpc = 0i64;
            let _ = QueryPerformanceCounter(&mut now_qpc);
            let now = Instant::now();
            let now_100ns = (now_qpc as f64 * 1e7 / qpf.max(1) as f64) as u64;
            let at = now.checked_sub(Duration::from_nanos(now_100ns.saturating_sub(qpc) * 100))?;
            let played = pos as f64 * rate as f64 / clock_rate;
            Some((dev_end - ((dev_written as f64 - played).max(0.0) * per_frame) as i64, at))
        };
        let mut started = false;
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
                dev_written = 0;
                started = false;
                st.audio_clock = None;
            }
            if !st.playing || !st.audio_live() || !st.picture_up || st.speed != 1.0 {
                if started {
                    let _ = client.Stop();
                    started = false;
                }
                if st.playing {
                    let t = st.clock(now);
                    drop_before(&mut st, t);
                }
                continue;
            }
            if st.audio_clock.is_none() {
                let t = st.clock(now);
                drop_before(&mut st, t);
            }
            let queued = client.GetCurrentPadding()? as usize;
            let room = size.saturating_sub(queued);
            let have = st.audio_frames;
            let n = room.min(have);
            if n > 0 {
                let buf = render.GetBuffer(n as u32)? as *mut i16;
                let out = std::slice::from_raw_parts_mut(buf, n * fpf);
                let muted = st.muted;
                let mut written = 0;
                let mut end = st.audio_clock.map_or(0, |(e, _, _)| e);
                while written < n {
                    let Some(c) = st.audio.front_mut() else { break };
                    let take = ((c.samples.len() - c.read) / fpf).min(n - written);
                    for i in 0..take * fpf {
                        out[written * fpf + i] = if muted { 0 } else { c.samples[c.read + i] as i16 };
                    }
                    c.read += take * fpf;
                    written += take;
                    end = c.pts + (c.read / fpf) as i64 * 1_000_000 / rate;
                    if c.read >= c.samples.len() {
                        st.audio.pop_front();
                    }
                }
                st.audio_frames -= written;
                render.ReleaseBuffer(written as u32, 0)?;
                dev_written += written as i64;
                dev_end = end;
                st.audio_clock = Some((end, (queued + written) as i64 * 1_000_000 / rate, now));
                shared.wake.notify_all();
                if !started {
                    client.Start()?;
                    started = true;
                }
            } else if let Some((end, _, _)) = st.audio_clock {
                st.audio_clock = Some((end, queued as i64 * 1_000_000 / rate, now));
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
            if started {
                if let Some((t, at)) = heard(dev_written, dev_end) {
                    st.steer(t, at);
                }
            }
        }
    }
}

/// Lets go of queued audio that ends before `t`.
fn drop_before(st: &mut State, t: i64) {
    let (fpf, rate) = (st.fpf, st.rate as i64);
    while let Some(c) = st.audio.front() {
        let end = c.pts + (c.samples.len() / fpf) as i64 * 1_000_000 / rate;
        if end > t {
            break;
        }
        st.audio_frames -= (c.samples.len() - c.read) / fpf;
        st.audio.pop_front();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Waits (up to 3 s) for the next picture `due` hands out.
    /// Lists `LOOKER_DECODE`'s audio tracks, and opens it again on the last one.
    /// `LOOKER_CODECS=%LOCALAPPDATA%\Looker\codecs LOOKER_DECODE=x.mp4 cargo test --release -- --ignored --nocapture audio_tracks_list`
    #[test]
    #[ignore]
    fn audio_tracks_list() {
        let path = std::path::PathBuf::from(std::env::var("LOOKER_DECODE").expect("set LOOKER_DECODE"));
        let (d, _) = Decoder::open_audio(&path, std::ptr::null_mut(), false, None).expect("opens");
        for (n, t) in d.video_tracks().iter().enumerate() {
            println!("  video stream {} showing {}: {}", t.stream, t.playing, t.video_label(n + 1));
        }
        let tracks = d.audio_tracks();
        for (n, t) in tracks.iter().enumerate() {
            println!("  stream {} playing {} default {}: {}", t.stream, t.playing, t.is_default, t.label(n + 1));
        }
        if let Some(last) = tracks.last() {
            let (d2, _) = Decoder::open_audio(&path, std::ptr::null_mut(), false, Some(last.stream)).expect("opens on the last");
            assert!(d2.audio_tracks().iter().any(|t| t.stream == last.stream && t.playing != 0));
        }
    }

    #[test]
    fn frame_numbers_are_the_nearest_listed() {
        let f = [0, 8000, 17000, 25000];
        assert_eq!(frame_number_in(&f, 0), Some(0));
        assert_eq!(frame_number_in(&f, 8000), Some(1));
        assert_eq!(frame_number_in(&f, 12000), Some(1));
        assert_eq!(frame_number_in(&f, 13000), Some(2));
        assert_eq!(frame_number_in(&f, 99000), Some(3));
        assert_eq!(frame_number_in(&f, -5), Some(0));
        assert_eq!(frame_number_in(&[], 5), None);
    }

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

    /// The seek bar's previews of `LOOKER_DECODE`: each wish answered, and how fast.
    /// `LOOKER_CODECS=%LOCALAPPDATA%\Looker\codecs LOOKER_DECODE=x.mp4 cargo test --release -- --ignored --nocapture preview_latency`
    #[test]
    #[ignore]
    fn preview_latency() {
        let path = std::path::PathBuf::from(std::env::var("LOOKER_DECODE").expect("set LOOKER_DECODE"));
        let dur = crate::imaging::video::probe(&path).unwrap().duration_us;
        // LOOKER_TEST_HW: on a hardware device, as the app does.
        let device = std::env::var_os("LOOKER_TEST_HW").map(|_| unsafe {
            use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
            use windows::Win32::Graphics::Direct3D11::*;
            use windows::core::Interface;
            let mut d = None;
            D3D11CreateDevice(None, D3D_DRIVER_TYPE_HARDWARE, Default::default(), D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT, None, D3D11_SDK_VERSION, Some(&mut d), None, None).unwrap();
            let d: ID3D11Device = d.unwrap();
            let _ = d.cast::<ID3D11Multithread>().unwrap().SetMultithreadProtected(true);
            d
        });
        let raw = device.as_ref().map_or(std::ptr::null_mut(), |d| windows::core::Interface::as_raw(d));
        let p = Preview::open(&path, 0, 192, 108, crate::gfx::Sendable(raw));
        let mut worst = 0.0f64;
        for i in 0..12 {
            let at = dur * ((i * 7) % 12) / 12;
            let t = Instant::now();
            p.want(at);
            let got = loop {
                if let Some(s) = p.take() {
                    break Some(s);
                }
                if t.elapsed() > Duration::from_secs(3) {
                    break None;
                }
                std::thread::sleep(Duration::from_millis(1));
            };
            let ms = t.elapsed().as_secs_f64() * 1000.0;
            worst = worst.max(ms);
            let s = got.unwrap_or_else(|| panic!("no preview for {at} us"));
            println!("preview at {at} us: {}x{} in {ms:.0} ms", s.width, s.height);
        }
        println!("worst {worst:.0} ms");
    }

    /// How long exact seeks take for the ways the seek bar gets dragged, on the GPU as in the app
    /// (`LOOKER_TEST_HW=1`) or the CPU: slow forward and back (a frame a step), medium (a fifth of a second), jumps.
    /// `LOOKER_CODECS=%LOCALAPPDATA%\Looker\codecs LOOKER_DECODE=x.mp4 cargo test --release -- --ignored --nocapture scrub_latency`
    #[test]
    #[ignore]
    fn scrub_latency() {
        let path = std::path::PathBuf::from(std::env::var("LOOKER_DECODE").expect("set LOOKER_DECODE"));
        let device = std::env::var_os("LOOKER_TEST_HW").map(|_| unsafe {
            use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
            use windows::Win32::Graphics::Direct3D11::*;
            use windows::core::Interface;
            let mut d = None;
            D3D11CreateDevice(None, D3D_DRIVER_TYPE_HARDWARE, Default::default(), D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT, None, D3D11_SDK_VERSION, Some(&mut d), None, None).unwrap();
            let d: ID3D11Device = d.unwrap();
            let _ = d.cast::<ID3D11Multithread>().unwrap().SetMultithreadProtected(true);
            d
        });
        let raw = device.as_ref().map_or(std::ptr::null_mut(), |d| windows::core::Interface::as_raw(d));
        let mut p = Player::open(&path, crate::gfx::Sendable(raw), false, 0.0, true);
        let t = Instant::now();
        while p.poll_open().is_none() {
            assert!(t.elapsed() < Duration::from_secs(3), "never opened");
            std::thread::sleep(Duration::from_millis(2));
        }
        let info = p.info.clone().unwrap();
        let frame = (1e6 * info.fps_den as f64 / info.fps_num as f64) as i64;
        next_due(&mut p).expect("first picture");
        let mid = info.duration_us / 2;
        let run = |name: &str, targets: Vec<i64>, p: &mut Player| {
            let (mut worst, mut total) = (0.0f64, 0.0f64);
            for &target in &targets {
                let t = Instant::now();
                p.seek(target, true);
                let got = next_due(p);
                if std::env::var_os("LOOKER_SCRUB_TRACE").is_some() {
                    let st = p.shared.lock();
                    println!("  {name} {target}: {got:?} in {:.0} ms; queue {} hold {:?} seek {:?} epoch {} shown {:?}", t.elapsed().as_secs_f64() * 1000.0, st.pictures.len(), st.hold, st.seek, st.epoch, p.shown);
                }
                if got.is_none() {
                    continue; // the picture on screen already covers it
                }
                let ms = t.elapsed().as_secs_f64() * 1000.0;
                worst = worst.max(ms);
                total += ms;
            }
            println!("{name:>14}: {:.1} ms average, {worst:.0} worst ({} seeks)", total / targets.len() as f64, targets.len());
        };
        p.seek(mid, true);
        next_due(&mut p);
        run("slow forward", (1..=30).map(|i| mid + i * frame + frame / 2).collect(), &mut p);
        let here = p.position();
        run("slow back", (1..=30).map(|i| here - i * frame + frame / 2).collect(), &mut p);
        run("medium", (1..=20).map(|i| (i * 200_000 + frame / 2).min(info.duration_us - frame)).collect(), &mut p);
        run("jumps", (0..12).map(|i| info.duration_us * ((i * 7) % 12) / 12 + frame / 2).collect(), &mut p);
    }

    /// Frame steps forward from pictures scrubbed back to (shown from the scrub cache) each move on a picture, on
    /// the GPU as in the app. High frame rates found it: a step from a cached picture could show that same one.
    /// `LOOKER_CODECS=%LOCALAPPDATA%\Looker\codecs LOOKER_DECODE=x.mp4 cargo test --release -- --ignored --nocapture steps_after_scrubbing_back`
    #[test]
    #[ignore]
    fn steps_after_scrubbing_back() {
        let path = std::path::PathBuf::from(std::env::var("LOOKER_DECODE").expect("set LOOKER_DECODE"));
        let device = unsafe {
            use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
            use windows::Win32::Graphics::Direct3D11::*;
            use windows::core::Interface;
            let mut d = None;
            D3D11CreateDevice(None, D3D_DRIVER_TYPE_HARDWARE, Default::default(), D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT, None, D3D11_SDK_VERSION, Some(&mut d), None, None).unwrap();
            let d: ID3D11Device = d.unwrap();
            let _ = d.cast::<ID3D11Multithread>().unwrap().SetMultithreadProtected(true);
            d
        };
        let raw = windows::core::Interface::as_raw(&device);
        let mut p = Player::open(&path, crate::gfx::Sendable(raw), false, 0.0, true);
        let t = Instant::now();
        while p.poll_open().is_none() {
            assert!(t.elapsed() < Duration::from_secs(3), "never opened");
            std::thread::sleep(Duration::from_millis(2));
        }
        let info = p.info.clone().unwrap();
        let frame = (1e6 * info.fps_den as f64 / info.fps_num as f64) as i64;
        println!("{:.3} fps, frame {frame} us", info.fps_num as f64 / info.fps_den as f64);
        next_due(&mut p).expect("first picture");
        let mid = info.duration_us / 2;
        let (mut steps, mut stuck, mut miscounted) = (0, 0, 0);
        for round in 0..6 {
            // A slow drag forward fills the cache; then back over it, and steps from there.
            let base = mid + round * 40 * frame;
            for i in 0..30 {
                p.seek(base + i * frame + frame / 2, true);
                next_due(&mut p);
            }
            p.seek(base + (5 + round) * frame + frame / 2, true);
            next_due(&mut p);
            for _ in 0..8 {
                let before = p.position();
                p.step_forward();
                let after = next_due(&mut p).unwrap_or(before);
                steps += 1;
                if after <= before {
                    stuck += 1;
                    println!("  step from {before} stayed at {after}");
                }
                // With the container's list of pictures, each step is exactly one number on.
                if let (Some((a, _)), Some((b, _))) = (p.frame_number(before), p.frame_number(after)) {
                    if b != a + 1 {
                        miscounted += 1;
                        println!("  step {before} -> {after}: frame {a} -> {b}");
                    }
                }
            }
        }
        println!("{stuck} of {steps} steps didn't move, {miscounted} miscounted; {} pictures listed", p.frames.len());
        assert_eq!(stuck, 0);
        assert_eq!(miscounted, 0);
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
        // A slow drag on the seek bar: exact seeks a frame apart, each landing on its picture, quickly (from the
        // queue, or the decoder carrying on: no restart from the keyframe).
        let start = p.position();
        let (mut worst, mut total) = (0.0f64, 0.0f64);
        for i in 1..=40 {
            let target = start + i * frame + frame / 2;
            let t = Instant::now();
            p.seek(target, true);
            let got = next_due(&mut p).expect("dragged picture");
            let ms = t.elapsed().as_secs_f64() * 1000.0;
            worst = worst.max(ms);
            total += ms;
            assert!(got <= target && target - got < 2 * frame, "drag to {target} landed on {got}");
        }
        println!("slow drag: 40 frames, {:.1} ms average, {worst:.0} worst", total / 40.0);

        // An exact seek overtaken by another at once (the first is interrupted mid-decode): the second wins.
        let far = (info.duration_us * 9 / 10).max(frame);
        p.seek(far, true);
        p.seek(400_000, true);
        let got = next_due(&mut p).expect("overtaken seek");
        println!("seek {far} then 400000 -> {got}");
        assert!(got <= 400_000 && 400_000 - got < 2 * frame, "landed on {got}");
        let next = next_due(&mut p);
        assert!(next.is_none() || next.is_some_and(|n| n < far / 2), "a picture of the first seek showed: {next:?}");
        assert!(!p.playing());
    }

    /// Playing `LOOKER_DECODE` backwards (J) on the GPU (`LOOKER_TEST_HW=1`) or the CPU: from 3 s for 2 s at 1x,
    /// every picture shown going down, how many were skipped; then 2x to the start, where it stops; then L plays
    /// forwards from the picture it stopped on.
    /// `LOOKER_CODECS=... LOOKER_DECODE=x.mp4 cargo test --release -- --ignored --nocapture plays_backwards`
    #[test]
    #[ignore]
    fn plays_backwards() {
        let path = std::path::PathBuf::from(std::env::var("LOOKER_DECODE").expect("set LOOKER_DECODE"));
        let device = std::env::var_os("LOOKER_TEST_HW").map(|_| unsafe {
            use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
            use windows::Win32::Graphics::Direct3D11::*;
            use windows::core::Interface;
            let mut d = None;
            D3D11CreateDevice(None, D3D_DRIVER_TYPE_HARDWARE, Default::default(), D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT, None, D3D11_SDK_VERSION, Some(&mut d), None, None).unwrap();
            let d: ID3D11Device = d.unwrap();
            let _ = d.cast::<ID3D11Multithread>().unwrap().SetMultithreadProtected(true);
            d
        });
        let raw = device.as_ref().map_or(std::ptr::null_mut(), |d| windows::core::Interface::as_raw(d));
        let mut p = Player::open(&path, crate::gfx::Sendable(raw), false, 0.0, true);
        let t = Instant::now();
        while p.poll_open().is_none() {
            assert!(t.elapsed() < Duration::from_secs(3), "never opened");
            std::thread::sleep(Duration::from_millis(2));
        }
        let info = p.info.clone().unwrap();
        let frame = (1e6 * info.fps_den as f64 / info.fps_num as f64) as i64;
        next_due(&mut p).expect("first picture");
        p.seek(3_000_000, true);
        let from = next_due(&mut p).expect("seeked");
        let t = Instant::now();
        p.play_reverse(1.0);
        let first = next_due(&mut p).expect("first picture backwards");
        println!("backwards from {from}: first picture {first} after {:.0} ms", t.elapsed().as_secs_f64() * 1000.0);
        assert!(first < from && from - first < 2 * frame, "first backwards {from} -> {first}");
        let (mut last, mut shown) = (first, 1);
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(2) {
            if let Some(d) = p.due(Instant::now()) {
                assert!(d.picture.pts_us < last, "went up: {last} -> {}", d.picture.pts_us);
                last = d.picture.pts_us;
                shown += 1;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        let covered = first - last;
        println!("1x: {shown} pictures over {:.2} s of video in 2 s, {} skipped, {} waits", covered as f64 / 1e6, p.dropped, p.starved);
        assert!(covered > 1_500_000 && covered < 2_300_000, "covered {covered}");
        p.play_reverse(2.0);
        let t = Instant::now();
        while p.reverse() {
            assert!(t.elapsed() < Duration::from_secs(5), "never reached the start (at {})", p.position());
            if let Some(d) = p.due(Instant::now()) {
                assert!(d.picture.pts_us <= last);
                last = d.picture.pts_us;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        println!("2x: stopped at {last} after {:.2} s", t.elapsed().as_secs_f64());
        assert!(last < 2 * frame, "stopped at {last}");
        assert!(!p.playing());
        // Forwards again from there: the decoder was put back.
        p.set_playing(true);
        let mut prev = -1;
        let t = Instant::now();
        while t.elapsed() < Duration::from_millis(500) {
            if let Some(d) = p.due(Instant::now()) {
                assert!(d.picture.pts_us > prev || prev < 0, "forwards went back: {prev} -> {}", d.picture.pts_us);
                prev = d.picture.pts_us;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        println!("forwards: at {prev} after 0.5 s");
        assert!(prev > 300_000 && prev < 700_000, "forwards at {prev}");
    }
}
