//! Playing an audio file (the audio view, app/audio.rs). A decode thread reads the file (imaging/audio.rs) about a
//! second ahead into a queue of blocks; a render thread feeds WASAPI (shared mode) from it at the file's own rate,
//! Windows converting to the device's (AUTOCONVERTPCM). More than two channels are folded to stereo here.
//!
//! The playhead is exact: the render thread keeps, for what it wrote, where each run of device frames came from in
//! the file (`State::segments`), and the device's own position (`IAudioClock`, stamped with the QPC time it was
//! read) says which device frame is being heard; between readings it runs on at the file's rate.
//!
//! Loops are seamless: the decode thread cuts the block at the loop's end and goes back to its start itself, so the
//! device never runs dry and the playhead follows through the segment map. A seek drops what's queued and what the
//! device holds, and fades the first 3 ms in.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use crate::imaging::audio::{Block, Facts, Interrupter, Reader, Tags};
use crate::imaging::video::Info;

/// Decoded ahead of what the device holds.
const AHEAD_SECS: f64 = 1.0;
/// The fade-in after a seek or a start, in seconds.
const FADE_SECS: f64 = 0.003;

/// What opening found: the facts, the tags and the cover's encoded bytes.
pub struct Opened {
    pub facts: Facts,
    pub tags: Tags,
    pub cover: Option<Vec<u8>>,
}

struct Chunk {
    /// The file frame its first frame is.
    pos: i64,
    /// Interleaved, `out_channels` a frame.
    samples: Vec<f32>,
    /// Frames already written.
    read: usize,
}

struct State {
    chunks: VecDeque<Chunk>,
    /// Frames queued.
    queued: usize,
    epoch: u32,
    seek: Option<i64>,
    /// The decoder reached the end of the file (not looping).
    end: bool,
    quit: bool,
    playing: bool,
    volume: f32,
    muted: bool,
    /// What loops, in file frames (`i64::MAX` as the end: the whole file), or nothing.
    looping: Option<(i64, i64)>,
    /// The position when nothing is heard yet (paused before any sound, right after a seek).
    anchor: i64,
    /// The render thread drops what the device holds (a seek).
    reset: bool,
    /// Runs of device frames as written: (first device frame, the file frame it is, length).
    segments: VecDeque<(i64, i64, i64)>,
    /// Device frames written since the last reset.
    written: i64,
    /// Device frames heard, and when (from `IAudioClock`).
    heard: Option<(f64, Instant)>,
    /// Played to the end and stopped there.
    finished: bool,
    error: Option<String>,
}

impl State {
    /// The file frame being heard at `now`.
    fn position(&self, now: Instant, rate: f64) -> i64 {
        let Some((played, at)) = self.heard else { return self.anchor };
        if self.segments.is_empty() {
            return self.anchor;
        }
        let mut dev = played;
        if self.playing {
            dev += now.saturating_duration_since(at).as_secs_f64() * rate;
        }
        let dev = (dev as i64).min(self.written);
        let first = self.segments[0];
        if dev < first.0 {
            return first.1;
        }
        for &(d, s, n) in &self.segments {
            if dev < d + n {
                return s + (dev - d);
            }
        }
        let last = self.segments[self.segments.len() - 1];
        last.1 + last.2
    }

    fn clear(&mut self) {
        self.chunks.clear();
        self.queued = 0;
        self.end = false;
        self.finished = false;
        self.reset = true;
        self.heard = None;
        self.segments.clear();
        self.written = 0;
    }
}

struct Shared {
    state: Mutex<State>,
    wake: Condvar,
    interrupt: Mutex<Option<Interrupter>>,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

type OpenSlot = Arc<Mutex<Option<Result<Opened, String>>>>;

pub struct AudioPlayer {
    shared: Arc<Shared>,
    /// The facts in a video's shape, for the controls bar.
    pub info: Option<Info>,
    pub facts: Option<Facts>,
    pub tags: Tags,
    /// The cover's bytes, until the view takes them.
    pub cover: Option<Vec<u8>>,
    /// The whole file loops (the loop button / setting); a region loops whatever this says.
    pub looping: bool,
    region: Option<(i64, i64)>,
    /// Frames in the file, once the scan has counted them (more exact than the header's duration).
    total: Option<i64>,
    opened: OpenSlot,
}

impl AudioPlayer {
    /// Starts reading `path`'s audio stream `stream` (None: the best one), playing at once when `play`.
    pub fn open(path: &Path, stream: Option<i32>, play: bool, volume: f32, muted: bool) -> AudioPlayer {
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                chunks: VecDeque::new(),
                queued: 0,
                epoch: 0,
                seek: None,
                end: false,
                quit: false,
                playing: play,
                volume,
                muted,
                looping: None,
                anchor: 0,
                reset: false,
                segments: VecDeque::new(),
                written: 0,
                heard: None,
                finished: false,
                error: None,
            }),
            wake: Condvar::new(),
            interrupt: Mutex::new(None),
        });
        let opened: OpenSlot = Arc::new(Mutex::new(None));
        let (s, p, o) = (shared.clone(), path.to_path_buf(), opened.clone());
        std::thread::Builder::new().name("audio-decode".into()).spawn(move || decode_thread(s, &p, stream, o)).ok();
        AudioPlayer { shared, info: None, facts: None, tags: Tags::default(), cover: None, looping: false, region: None, total: None, opened }
    }

    /// The facts once the file is open (`Err` when it couldn't be); starts the sound then.
    pub fn poll_open(&mut self) -> Option<Result<(), String>> {
        if self.info.is_some() {
            return Some(Ok(()));
        }
        let r = self.opened.lock().ok()?.take()?;
        Some(r.map(|o| {
            let f = &o.facts;
            self.info = Some(Info::audio_only(f.duration_us, f.channels, f.rate, f.bit_rate, &f.codec()));
            let (rate, channels) = (f.rate as u32, if f.channels == 1 { 1 } else { 2 });
            let s = self.shared.clone();
            std::thread::Builder::new().name("audio-render".into()).spawn(move || render_thread(s, rate, channels)).ok();
            self.facts = Some(o.facts);
            self.tags = o.tags;
            self.cover = o.cover;
            self.apply_loop();
        }))
    }

    pub fn rate(&self) -> f64 {
        self.facts.as_ref().map_or(48000.0, |f| f.rate.max(1) as f64)
    }

    /// Frames in the file: counted by the scan when it's done, else from the header.
    pub fn total_frames(&self) -> i64 {
        self.total.or_else(|| self.facts.as_ref().map(|f| f.frames)).unwrap_or(0)
    }

    /// The scan counted the frames: the length follows.
    pub fn set_total_frames(&mut self, frames: i64) {
        self.total = Some(frames);
        let us = (frames as f64 * 1e6 / self.rate()) as i64;
        if let Some(i) = &mut self.info {
            i.duration_us = us;
        }
    }

    pub fn animating(&self) -> bool {
        self.info.is_none() || self.shared.lock().playing
    }

    pub fn playing(&self) -> bool {
        self.shared.lock().playing
    }

    pub fn set_playing(&mut self, play: bool) {
        let mut st = self.shared.lock();
        if st.playing == play {
            return;
        }
        if play && st.finished {
            let start = self.region.map_or(0, |r| r.0);
            drop(st);
            self.seek_frame(start);
            self.shared.lock().playing = true;
            return;
        }
        st.playing = play;
        self.shared.wake.notify_all();
    }

    /// The file frame being heard now.
    pub fn position_frame(&self) -> i64 {
        let rate = self.rate();
        self.shared.lock().position(Instant::now(), rate)
    }

    /// The time being heard now.
    pub fn position(&self) -> i64 {
        (self.position_frame() as f64 * 1e6 / self.rate()) as i64
    }

    pub fn duration(&self) -> i64 {
        self.info.as_ref().map_or(0, |i| i.duration_us)
    }

    pub fn seek(&mut self, us: i64, _exact: bool) {
        let f = (us.max(0) as f64 * self.rate() / 1e6).round() as i64;
        self.seek_frame(f);
    }

    /// Jumps to file frame `frame` (clamped to the file).
    pub fn seek_frame(&mut self, frame: i64) {
        let total = self.total_frames();
        let frame = if total > 0 { frame.clamp(0, total) } else { frame.max(0) };
        let mut st = self.shared.lock();
        st.clear();
        st.epoch = st.epoch.wrapping_add(1);
        st.seek = Some(frame);
        st.anchor = frame;
        drop(st);
        self.shared.wake.notify_all();
        if let Some(i) = self.shared.interrupt.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            i.interrupt();
        }
    }

    pub fn at_end(&self) -> bool {
        self.shared.lock().finished
    }

    pub fn set_looping(&mut self, on: bool) {
        self.looping = on;
        self.apply_loop();
    }

    /// The loop region in file frames (None: the whole file, as the loop button says).
    pub fn region(&self) -> Option<(i64, i64)> {
        self.region
    }

    /// Loops `r` (file frames), or stops looping a region. Playing from outside a new region jumps into it; from
    /// inside, the queue is read again from where it's heard (what was queued may run past the region's end).
    pub fn set_region(&mut self, r: Option<(i64, i64)>) {
        let r = r.filter(|(a, b)| b > a);
        if self.region == r {
            return;
        }
        self.region = r;
        self.apply_loop();
        let at = self.position_frame();
        match r {
            Some((a, b)) if at < a || at >= b => self.seek_frame(a),
            _ => {
                let playing = self.playing();
                if playing || r.is_some() {
                    self.seek_frame(at);
                }
            }
        }
    }

    fn apply_loop(&mut self) {
        let l = self.region.or(self.looping.then_some((0, i64::MAX)));
        self.shared.lock().looping = l;
        self.shared.wake.notify_all();
    }

    pub fn set_volume(&mut self, volume: f32, muted: bool) {
        let mut st = self.shared.lock();
        st.volume = volume;
        st.muted = muted;
    }
}

impl Drop for AudioPlayer {
    fn drop(&mut self) {
        self.shared.lock().quit = true;
        self.shared.wake.notify_all();
        if let Some(i) = self.shared.interrupt.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            i.interrupt();
        }
    }
}

/// Folds `channels`-channel frames to stereo (FFmpeg's default orders: L R C LFE then surrounds), or copies mono
/// and stereo.
fn to_output(s: &[f32], channels: usize) -> Vec<f32> {
    if channels <= 2 {
        return s.to_vec();
    }
    const C: f32 = std::f32::consts::FRAC_1_SQRT_2;
    let mut out = Vec::with_capacity(s.len() / channels * 2);
    for f in s.chunks_exact(channels) {
        let (mut l, mut r) = (f[0], f[1]);
        if channels >= 3 {
            l += C * f[2];
            r += C * f[2];
        }
        // f[3] is the LFE (left out, as downmixes do); then surround pairs, left then right.
        for (i, v) in f.iter().enumerate().skip(4) {
            if i % 2 == 0 {
                l += C * v;
            } else {
                r += C * v;
            }
        }
        let k = 1.0 / (1.0 + C + C * ((channels.saturating_sub(4)) as f32 / 2.0));
        out.push(l * k);
        out.push(r * k);
    }
    out
}

fn decode_thread(shared: Arc<Shared>, path: &Path, stream: Option<i32>, opened: OpenSlot) {
    let t = Instant::now();
    let mut r = match Reader::open(path, stream) {
        Ok(r) => r,
        Err(e) => {
            crate::trace::mark(format!("audio: {e}"));
            shared.lock().error = Some(e.clone());
            if let Ok(mut o) = opened.lock() {
                *o = Some(Err(e));
            }
            return;
        }
    };
    let facts = r.facts.clone();
    crate::trace::mark(format!("audio: opened {} in {:.0} ms", crate::imaging::audio::format_line(&facts), t.elapsed().as_secs_f64() * 1000.0));
    let o = Opened { facts: facts.clone(), tags: r.tags(), cover: r.cover() };
    if let Ok(mut slot) = opened.lock() {
        *slot = Some(Ok(o));
    }
    *shared.interrupt.lock().unwrap_or_else(|e| e.into_inner()) = Some(r.interrupter());
    let ch = facts.channels.max(1) as usize;
    let ahead = (facts.rate.max(1) as f64 * AHEAD_SECS) as usize;
    loop {
        let (seek, epoch, looping) = {
            let mut st = shared.lock();
            loop {
                if st.quit {
                    *shared.interrupt.lock().unwrap_or_else(|e| e.into_inner()) = None;
                    return;
                }
                if st.seek.is_some() || (!st.end && st.queued < ahead) {
                    break;
                }
                st = shared.wake.wait_timeout(st, Duration::from_millis(100)).map(|(g, _)| g).unwrap_or_else(|e| e.into_inner().0);
            }
            (st.seek.take(), st.epoch, st.looping)
        };
        if let Some(f) = seek {
            if let Err(e) = r.seek(f) {
                crate::trace::mark(format!("audio: seek: {e}"));
            }
            continue;
        }
        let back_to = match r.read() {
            Ok(Block::Samples(pos, s)) => {
                let n = s.len() / ch;
                let (mut from, mut take, mut wrap) = (0usize, n, None);
                if let Some((a, b)) = looping {
                    // A region set while elsewhere starts at its start (the seek that comes with it does that), but
                    // what was read before it is never queued.
                    if pos < a && pos + (n as i64) > a {
                        from = (a - pos) as usize;
                    }
                    if b != i64::MAX && pos + n as i64 >= b {
                        take = (b - pos).clamp(0, n as i64) as usize;
                        wrap = Some(a);
                    }
                }
                let samples = if take > from { to_output(&s[from * ch..take * ch], ch) } else { Vec::new() };
                let mut st = shared.lock();
                if st.seek.is_some() || st.epoch != epoch {
                    continue;
                }
                if !samples.is_empty() {
                    st.queued += take - from;
                    st.chunks.push_back(Chunk { pos: pos + from as i64, samples, read: 0 });
                }
                wrap
            }
            Ok(Block::End) => match looping {
                Some((a, _)) => Some(a),
                None => {
                    let mut st = shared.lock();
                    if st.epoch == epoch && st.seek.is_none() {
                        st.end = true;
                    }
                    None
                }
            },
            Ok(Block::Interrupted) => {
                // For a seek (taken at the top); else it came too late: read on.
                let st = shared.lock();
                if st.seek.is_none() && !st.quit {
                    let at = st.chunks.back().map(|c| c.pos + (c.samples.len() / if ch == 1 { 1 } else { 2 }) as i64);
                    drop(st);
                    let _ = r.seek(at.unwrap_or(0));
                }
                None
            }
            Err(e) => {
                crate::trace::mark(format!("audio: decode: {e}"));
                let mut st = shared.lock();
                st.error = Some(e);
                st.end = true;
                None
            }
        };
        if let Some(a) = back_to {
            // Seamless: the next block read is the loop's start, queued straight after.
            if shared.lock().seek.is_none() {
                if let Err(e) = r.seek(a) {
                    crate::trace::mark(format!("audio: loop: {e}"));
                }
            }
        }
    }
}

fn render_thread(shared: Arc<Shared>, rate: u32, channels: u16) {
    if let Err(e) = unsafe { run(&shared, rate, channels) } {
        crate::trace::mark(format!("audio: no sound: {e}"));
        let mut st = shared.lock();
        st.error = Some(format!("No sound: {}", e.message()));
        st.playing = false;
    }
}

unsafe fn run(shared: &Shared, rate: u32, channels: u16) -> windows::core::Result<()> {
    use windows::Win32::Media::Audio::*;
    use windows::Win32::Media::KernelStreaming::{SPEAKER_FRONT_CENTER, SPEAKER_FRONT_LEFT, SPEAKER_FRONT_RIGHT, WAVE_FORMAT_EXTENSIBLE};
    // KSDATAFORMAT_SUBTYPE_IEEE_FLOAT
    const IEEE_FLOAT: windows::core::GUID = windows::core::GUID::from_u128(0x00000003_0000_0010_8000_00aa00389b71);
    use windows::Win32::System::Com::*;
    use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
    use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let mut task = 0u32;
        let _ = windows::Win32::System::Threading::AvSetMmThreadCharacteristicsW(windows::core::w!("Audio"), &mut task);
        let devices: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let device = devices.GetDefaultAudioEndpoint(eRender, eConsole)?;
        let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
        let ch = channels as usize;
        let block = 4 * channels;
        let format = WAVEFORMATEXTENSIBLE {
            Format: WAVEFORMATEX {
                wFormatTag: WAVE_FORMAT_EXTENSIBLE as u16,
                nChannels: channels,
                nSamplesPerSec: rate,
                nAvgBytesPerSec: rate * block as u32,
                nBlockAlign: block,
                wBitsPerSample: 32,
                cbSize: (std::mem::size_of::<WAVEFORMATEXTENSIBLE>() - std::mem::size_of::<WAVEFORMATEX>()) as u16,
            },
            Samples: WAVEFORMATEXTENSIBLE_0 { wValidBitsPerSample: 32 },
            dwChannelMask: if channels == 1 { SPEAKER_FRONT_CENTER } else { SPEAKER_FRONT_LEFT | SPEAKER_FRONT_RIGHT },
            SubFormat: IEEE_FLOAT,
        };
        // 100 ms of buffer at the file's rate; Windows converts it to the device's format.
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
        let clock: IAudioClock = client.GetService()?;
        let clock_rate = clock.GetFrequency()?.max(1) as f64;
        let mut qpf = 0i64;
        let _ = QueryPerformanceFrequency(&mut qpf);
        // Device frames heard, and when.
        let heard = || -> Option<(f64, Instant)> {
            let (mut pos, mut qpc) = (0u64, 0u64);
            clock.GetPosition(&mut pos, Some(&mut qpc)).ok()?;
            let mut now_qpc = 0i64;
            let _ = QueryPerformanceCounter(&mut now_qpc);
            let now = Instant::now();
            let now_100ns = (now_qpc as f64 * 1e7 / qpf.max(1) as f64) as u64;
            let at = now.checked_sub(Duration::from_nanos(now_100ns.saturating_sub(qpc) * 100))?;
            Some((pos as f64 * rate as f64 / clock_rate, at))
        };
        let fade = ((rate as f64 * FADE_SECS) as usize).max(1);
        let mut faded = 0usize;
        let mut started = false;
        loop {
            WaitForSingleObject(event, 20);
            let mut st = shared.lock();
            if st.quit {
                let _ = client.Stop();
                return Ok(());
            }
            if st.reset {
                st.reset = false;
                let _ = client.Stop();
                let _ = client.Reset();
                started = false;
                faded = 0;
            }
            if !st.playing {
                if started {
                    let _ = client.Stop();
                    started = false;
                    // Where it stopped: the playhead holds there.
                    if let Some(h) = heard() {
                        st.heard = Some(h);
                    }
                }
                continue;
            }
            let queued = client.GetCurrentPadding()? as usize;
            let room = size.saturating_sub(queued);
            let n = room.min(st.queued);
            if n > 0 {
                let buf = render.GetBuffer(n as u32)? as *mut f32;
                let out = std::slice::from_raw_parts_mut(buf, n * ch);
                let gain = if st.muted { 0.0 } else { st.volume * st.volume };
                let mut done = 0usize;
                while done < n {
                    let Some(c) = st.chunks.front_mut() else { break };
                    let have = c.samples.len() / ch - c.read;
                    let take = have.min(n - done);
                    for i in 0..take {
                        let g = if faded < fade {
                            faded += 1;
                            gain * faded as f32 / fade as f32
                        } else {
                            gain
                        };
                        for k in 0..ch {
                            out[(done + i) * ch + k] = c.samples[(c.read + i) * ch + k] * g;
                        }
                    }
                    let src = c.pos + c.read as i64;
                    c.read += take;
                    let finished_chunk = c.read * ch >= c.samples.len();
                    let dev = st.written + done as i64;
                    match st.segments.back_mut() {
                        Some(last) if last.0 + last.2 == dev && last.1 + last.2 == src => last.2 += take as i64,
                        _ => st.segments.push_back((dev, src, take as i64)),
                    }
                    done += take;
                    if finished_chunk {
                        st.chunks.pop_front();
                    }
                }
                st.queued -= done;
                st.written += done as i64;
                render.ReleaseBuffer(done as u32, 0)?;
                shared.wake.notify_all();
                if !started {
                    client.Start()?;
                    started = true;
                }
            } else if queued == 0 && st.queued == 0 && st.end && started {
                // Played to the end: it stops there.
                let now = Instant::now();
                let at = st.position(now, rate as f64);
                st.playing = false;
                st.finished = true;
                st.anchor = at;
                st.heard = None;
                let _ = client.Stop();
                started = false;
                continue;
            }
            if started {
                if let Some(h) = heard() {
                    st.heard = Some(h);
                    // Runs fully heard are let go (the last one stays: it places the playhead).
                    while st.segments.len() > 1 && (st.segments[0].0 + st.segments[0].2) as f64 <= h.0 {
                        st.segments.pop_front();
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// LOOKER_AUDIO=<file>: plays it (silent) with a half-second loop; the playhead must move and stay in it.
    #[test]
    #[ignore]
    fn loops_a_region() {
        let Some(p) = std::env::var_os("LOOKER_AUDIO") else { return };
        let mut pl = AudioPlayer::open(Path::new(&p), None, true, 0.0, true);
        while pl.poll_open().is_none() {
            std::thread::sleep(Duration::from_millis(10));
        }
        let rate = pl.rate() as i64;
        let (a, b) = (rate * 2, rate * 2 + rate / 2);
        pl.set_region(Some((a, b)));
        let mut seen = Vec::new();
        for _ in 0..40 {
            std::thread::sleep(Duration::from_millis(50));
            seen.push(pl.position_frame());
        }
        println!("{:?}", seen.iter().map(|f| f - a).collect::<Vec<_>>());
        assert!(seen.iter().all(|&f| f >= a && f <= b), "left the loop");
        assert!(seen.windows(2).any(|w| w[1] < w[0]), "never wrapped");
        assert!(seen.windows(2).filter(|w| w[1] > w[0]).count() > 25, "didn't move");
    }

    #[test]
    fn folds_surround_to_stereo() {
        let s = [1.0f32, 0.0, 0.0, 0.0, 0.0, 0.0];
        let o = to_output(&s, 6);
        assert_eq!(o.len(), 2);
        assert!(o[0] > 0.0 && o[1] == 0.0);
        assert_eq!(to_output(&[0.5, -0.5], 2), vec![0.5, -0.5]);
    }

    #[test]
    fn playhead_follows_the_segments() {
        let mut st = State {
            chunks: VecDeque::new(),
            queued: 0,
            epoch: 0,
            seek: None,
            end: false,
            quit: false,
            playing: false,
            volume: 1.0,
            muted: false,
            looping: None,
            anchor: 7,
            reset: false,
            segments: VecDeque::new(),
            written: 0,
            heard: None,
            finished: false,
            error: None,
        };
        let now = Instant::now();
        assert_eq!(st.position(now, 48000.0), 7);
        // A loop: device frames 0..100 are file 900..1000, then 100..200 are file 500..600.
        st.segments.push_back((0, 900, 100));
        st.segments.push_back((100, 500, 100));
        st.written = 200;
        st.heard = Some((50.0, now));
        assert_eq!(st.position(now, 48000.0), 950);
        st.heard = Some((150.0, now));
        assert_eq!(st.position(now, 48000.0), 550);
        st.heard = Some((500.0, now));
        assert_eq!(st.position(now, 48000.0), 600);
    }
}
