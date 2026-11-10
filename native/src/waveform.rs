//! An audio file's picture, worked out on its own thread with its own reader (imaging/audio.rs) while the player
//! plays: per channel, the min, max and RMS of every 256 frames (Transient's peaks, filling in as it reads), the
//! samples themselves for files small enough to keep (exact when zoomed in), a spectrogram of the channels mixed,
//! and the loudness (ITU-R BS.1770: integrated, with its gates, and the sample peak).

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use crate::imaging::audio::{Block, Reader};

/// Frames per peak bucket.
pub const BUCKET: usize = 256;
/// Channels drawn as lanes; the rest still count in the spectrogram and loudness.
pub const MAX_LANES: usize = 8;
/// Samples kept for exact zoom while they fit in this much memory.
const KEEP_BYTES: usize = 256 << 20;
/// The spectrogram's FFT, and how many columns it aims at over the file (the hop follows the length).
pub const FFT: usize = 2048;
pub const BINS: usize = FFT / 2;
const COLUMNS: usize = 8192;
/// Its range: this many dB below full scale is black.
const FLOOR_DB: f32 = 100.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Loudness {
    /// Integrated loudness, LUFS (None for silence).
    pub integrated: Option<f32>,
    /// The highest sample, dBFS.
    pub peak: f32,
}

pub struct Data {
    pub rate: u32,
    pub channels: usize,
    /// `lanes` = min(channels, MAX_LANES) values per bucket, bucket-major.
    pub lanes: usize,
    pub min: Vec<f32>,
    pub max: Vec<f32>,
    pub rms: Vec<f32>,
    /// Frames read so far (all of them once `done`).
    pub frames: i64,
    /// The file's frames from its header, until `done`.
    pub estimate: i64,
    pub done: bool,
    /// Interleaved, all channels: kept while the file fits in `KEEP_BYTES`.
    pub samples: Option<Vec<f32>>,
    /// The spectrogram: `BINS` bytes a column (0 = `FLOOR_DB` down, 255 = full scale), column `c` the FFT of
    /// frames `c * hop .. c * hop + FFT`.
    pub spec: Vec<u8>,
    pub hop: usize,
    pub loudness: Option<Loudness>,
    pub error: Option<String>,
    /// Bumped whenever more is in (the view rebuilds what it drew from it).
    pub generation: u64,
}

impl Data {
    /// Buckets filled.
    pub fn buckets(&self) -> usize {
        self.min.len() / self.lanes.max(1)
    }

    /// The frames the file has (counted once done, else the larger of the header's and what's read).
    pub fn total(&self) -> i64 {
        if self.done { self.frames } else { self.estimate.max(self.frames) }
    }

    /// Min, max and RMS of lane `lane` over frames `f0..f1`, from the samples when they're kept and the range is
    /// short, else from the buckets. None past what's read.
    pub fn range(&self, lane: usize, f0: f64, f1: f64) -> Option<(f32, f32, f32)> {
        if f0 >= self.frames as f64 || f1 <= 0.0 {
            return None;
        }
        if let Some(s) = self.samples.as_ref().filter(|_| f1 - f0 < (BUCKET * 4) as f64) {
            let ch = self.channels;
            // One sample either side so neighbouring columns join into a line when zoomed far in.
            let a = (f0.floor().max(0.0) as usize).min(self.frames as usize - 1);
            let b = (f1.ceil() as usize).clamp(a + 1, self.frames as usize);
            let (mut lo, mut hi, mut sq) = (f32::MAX, f32::MIN, 0.0f32);
            for i in a..=b.min(self.frames as usize - 1) {
                let v = s[i * ch + lane];
                lo = lo.min(v);
                hi = hi.max(v);
                sq += v * v;
            }
            let n = (b.min(self.frames as usize - 1) + 1 - a) as f32;
            return Some((lo, hi, (sq / n).sqrt()));
        }
        let nb = self.buckets();
        if nb == 0 {
            return None;
        }
        let b0 = ((f0 / BUCKET as f64).floor().max(0.0) as usize).min(nb - 1);
        let b1 = ((f1 / BUCKET as f64).ceil() as usize).clamp(b0 + 1, nb);
        let (mut lo, mut hi, mut sq) = (f32::MAX, f32::MIN, 0.0f32);
        for b in b0..b1 {
            let i = b * self.lanes + lane;
            lo = lo.min(self.min[i]);
            hi = hi.max(self.max[i]);
            sq += self.rms[i] * self.rms[i];
        }
        Some((lo, hi, (sq / (b1 - b0) as f32).sqrt()))
    }

    pub fn columns(&self) -> usize {
        self.spec.len() / BINS
    }
}

/// The scan of one file: its data fills in while its thread reads.
pub struct Scan {
    data: Arc<Mutex<Data>>,
    quit: Arc<AtomicBool>,
}

impl Scan {
    pub fn start(path: &Path, stream: Option<i32>) -> Scan {
        let data = Arc::new(Mutex::new(Data {
            rate: 0,
            channels: 0,
            lanes: 0,
            min: Vec::new(),
            max: Vec::new(),
            rms: Vec::new(),
            frames: 0,
            estimate: 0,
            done: false,
            samples: None,
            spec: Vec::new(),
            hop: 0,
            loudness: None,
            error: None,
            generation: 0,
        }));
        let quit = Arc::new(AtomicBool::new(false));
        let (d, q, p) = (data.clone(), quit.clone(), path.to_path_buf());
        std::thread::Builder::new()
            .name("audio-scan".into())
            .spawn(move || {
                if let Err(e) = scan(&p, stream, &d, &q) {
                    crate::trace::mark(format!("audio scan: {e}"));
                    let mut d = d.lock().unwrap_or_else(|e| e.into_inner());
                    d.error = Some(e);
                    d.done = true;
                    d.generation += 1;
                }
            })
            .ok();
        Scan { data, quit }
    }

    pub fn data(&self) -> MutexGuard<'_, Data> {
        self.data.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Drop for Scan {
    fn drop(&mut self) {
        self.quit.store(true, Ordering::Relaxed);
    }
}

/// One biquad, direct form 1 (the K-weighting stages).
#[derive(Clone, Copy, Default)]
struct Biquad {
    b: [f64; 3],
    a: [f64; 3],
    x: [f64; 2],
    y: [f64; 2],
}

impl Biquad {
    fn run(&mut self, v: f64) -> f64 {
        let y = self.b[0] * v + self.b[1] * self.x[0] + self.b[2] * self.x[1] - self.a[1] * self.y[0] - self.a[2] * self.y[1];
        self.x = [v, self.x[0]];
        self.y = [y, self.y[0]];
        y
    }
}

/// BS.1770's K-weighting at `rate` (the shelf and the high-pass, after libebur128's derivation for any rate).
fn k_weighting(rate: f64) -> (Biquad, Biquad) {
    use std::f64::consts::PI;
    let (f0, g, q) = (1681.974450955533, 3.999843853973347, 0.7071752369554196);
    let k = (PI * f0 / rate).tan();
    let vh = 10f64.powf(g / 20.0);
    let vb = vh.powf(0.4996667741545416);
    let a0 = 1.0 + k / q + k * k;
    let shelf = Biquad {
        b: [(vh + vb * k / q + k * k) / a0, 2.0 * (k * k - vh) / a0, (vh - vb * k / q + k * k) / a0],
        a: [1.0, 2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
        ..Default::default()
    };
    let (f0, q) = (38.13547087602444, 0.5003270373238773);
    let k = (PI * f0 / rate).tan();
    let d = 1.0 + k / q + k * k;
    let high = Biquad { b: [1.0, -2.0, 1.0], a: [1.0, 2.0 * (k * k - 1.0) / d, (1.0 - k / q + k * k) / d], ..Default::default() };
    (shelf, high)
}

/// Integrated loudness from 100 ms energies (mean square, channel-weighted): 400 ms blocks overlapping by 75 %,
/// gated at -70 LUFS and then 10 LU below the mean of what passed.
fn integrated(sub: &[f64]) -> Option<f32> {
    let lufs = |e: f64| -0.691 + 10.0 * e.log10();
    let blocks: Vec<f64> = sub.windows(4).map(|w| w.iter().sum::<f64>() / 4.0).filter(|&e| e > 0.0 && lufs(e) > -70.0).collect();
    if blocks.is_empty() {
        return None;
    }
    let mean = blocks.iter().sum::<f64>() / blocks.len() as f64;
    let gate = lufs(mean) - 10.0;
    let kept: Vec<f64> = blocks.into_iter().filter(|&e| lufs(e) > gate).collect();
    (!kept.is_empty()).then(|| lufs(kept.iter().sum::<f64>() / kept.len() as f64) as f32)
}

/// In-place radix-2 FFT of `re`/`im` (length a power of two).
fn fft(re: &mut [f32], im: &mut [f32]) {
    let n = re.len();
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let ang = -2.0 * std::f64::consts::PI / len as f64;
        let (wr, wi) = (ang.cos() as f32, ang.sin() as f32);
        for start in (0..n).step_by(len) {
            let (mut cr, mut ci) = (1.0f32, 0.0f32);
            for k in 0..len / 2 {
                let (a, b) = (start + k, start + k + len / 2);
                let (tr, ti) = (re[b] * cr - im[b] * ci, re[b] * ci + im[b] * cr);
                re[b] = re[a] - tr;
                im[b] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
                let nr = cr * wr - ci * wi;
                ci = cr * wi + ci * wr;
                cr = nr;
            }
        }
        len <<= 1;
    }
}

fn scan(path: &Path, stream: Option<i32>, data: &Mutex<Data>, quit: &AtomicBool) -> Result<(), String> {
    let mut r = Reader::open(path, stream)?;
    let f = r.facts.clone();
    let ch = f.channels.max(1) as usize;
    let lanes = ch.min(MAX_LANES);
    let rate = f.rate.max(1) as usize;
    let estimate = f.frames.max(0);
    // The hop: about COLUMNS over the file (10 minutes when the header doesn't say), never under a quarter FFT.
    let guess = if estimate > 0 { estimate as usize } else { rate * 600 };
    let hop = (guess / COLUMNS).max(FFT / 4).next_multiple_of(64);
    // Samples to keep: those read since the last publish (moved into the data then), while the file fits.
    let mut keeping = estimate as usize * ch * 4 <= KEEP_BYTES;
    let mut kept = 0usize;
    let mut pending: Vec<f32> = Vec::new();
    {
        let mut d = data.lock().unwrap_or_else(|e| e.into_inner());
        d.rate = rate as u32;
        d.channels = ch;
        d.lanes = lanes;
        d.estimate = estimate;
        d.hop = hop;
        d.generation += 1;
    }
    // Peaks: the bucket being filled.
    let mut b_lo = vec![f32::MAX; lanes];
    let mut b_hi = vec![f32::MIN; lanes];
    let mut b_sq = vec![0f32; lanes];
    let mut b_n = 0usize;
    let (mut new_lo, mut new_hi, mut new_rms) = (Vec::new(), Vec::new(), Vec::new());
    // Spectrogram: the mono mix's last FFT frames, in a ring.
    let window: Vec<f32> = (0..FFT).map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / FFT as f32).cos()).collect();
    let wsum: f32 = window.iter().sum();
    let mut ring = vec![0f32; FFT];
    let mut new_cols: Vec<u8> = Vec::new();
    let (mut re, mut im) = (vec![0f32; FFT], vec![0f32; FFT]);
    let mut next_col_end = FFT; // the frame count at which the next column is due
    // Loudness.
    let weights: Vec<f64> = (0..ch).map(|c| if ch > 4 && c == 3 { 0.0 } else if ch > 4 && c >= 4 { 1.41 } else { 1.0 }).collect();
    let mut filters: Vec<(Biquad, Biquad)> = (0..ch).map(|_| k_weighting(rate as f64)).collect();
    let sub_len = rate / 10;
    let mut sub_e = 0f64;
    let mut sub_n = 0usize;
    let mut subs: Vec<f64> = Vec::new();
    let mut peak = 0f32;
    let mut frames = 0i64;
    let mut last_publish = std::time::Instant::now();

    let publish = |frames: i64, lo: &mut Vec<f32>, hi: &mut Vec<f32>, rms: &mut Vec<f32>, cols: &mut Vec<u8>, keeping: bool, pending: &mut Vec<f32>| {
        let mut d = data.lock().unwrap_or_else(|e| e.into_inner());
        d.min.append(lo);
        d.max.append(hi);
        d.rms.append(rms);
        d.spec.append(cols);
        d.frames = frames;
        if keeping {
            let s = d.samples.get_or_insert_with(|| Vec::with_capacity(estimate.max(0) as usize * ch + 4096));
            s.append(pending);
        } else {
            d.samples = None;
            pending.clear();
        }
        d.generation += 1;
    };

    loop {
        if quit.load(Ordering::Relaxed) {
            return Ok(());
        }
        let s = match r.read()? {
            Block::Samples(_, s) => s,
            Block::End => break,
            Block::Interrupted => continue,
        };
        let n = s.len() / ch;
        if keeping {
            if (kept + s.len()) * 4 <= KEEP_BYTES {
                pending.extend_from_slice(s);
                kept += s.len();
            } else {
                keeping = false;
            }
        }
        for i in 0..n {
            let fr = &s[i * ch..i * ch + ch];
            let mut mono = 0f32;
            let mut e = 0f64;
            for (c, &v) in fr.iter().enumerate() {
                if c < lanes {
                    b_lo[c] = b_lo[c].min(v);
                    b_hi[c] = b_hi[c].max(v);
                    b_sq[c] += v * v;
                }
                mono += v;
                peak = peak.max(v.abs());
                if weights[c] > 0.0 {
                    let (a, b) = &mut filters[c];
                    let k = b.run(a.run(v as f64));
                    e += weights[c] * k * k;
                }
            }
            b_n += 1;
            if b_n == BUCKET {
                for c in 0..lanes {
                    new_lo.push(b_lo[c]);
                    new_hi.push(b_hi[c]);
                    new_rms.push((b_sq[c] / BUCKET as f32).sqrt());
                    b_lo[c] = f32::MAX;
                    b_hi[c] = f32::MIN;
                    b_sq[c] = 0.0;
                }
                b_n = 0;
            }
            sub_e += e;
            sub_n += 1;
            if sub_n == sub_len {
                subs.push(sub_e / sub_len as f64);
                sub_e = 0.0;
                sub_n = 0;
            }
            frames += 1;
            ring[(frames as usize - 1) % FFT] = mono / ch as f32;
            if frames as usize == next_col_end {
                let start = frames as usize % FFT; // the oldest sample in the ring
                for k in 0..FFT {
                    re[k] = ring[(start + k) % FFT] * window[k];
                    im[k] = 0.0;
                }
                fft(&mut re, &mut im);
                for k in 0..BINS {
                    let mag = (re[k] * re[k] + im[k] * im[k]).sqrt() * 2.0 / wsum;
                    let db = 20.0 * mag.max(1e-9).log10();
                    new_cols.push((((db + FLOOR_DB) / FLOOR_DB).clamp(0.0, 1.0) * 255.0) as u8);
                }
                next_col_end += hop;
            }
        }
        if last_publish.elapsed().as_millis() >= 60 {
            publish(frames, &mut new_lo, &mut new_hi, &mut new_rms, &mut new_cols, keeping, &mut pending);
            last_publish = std::time::Instant::now();
        }
    }
    // The last, partial bucket.
    if b_n > 0 {
        for c in 0..lanes {
            new_lo.push(b_lo[c]);
            new_hi.push(b_hi[c]);
            new_rms.push((b_sq[c] / b_n as f32).sqrt());
        }
    }
    publish(frames, &mut new_lo, &mut new_hi, &mut new_rms, &mut new_cols, keeping, &mut pending);
    let loud = Loudness { integrated: integrated(&subs), peak: 20.0 * peak.max(1e-9).log10() };
    let mut d = data.lock().unwrap_or_else(|e| e.into_inner());
    d.loudness = Some(loud);
    d.done = true;
    d.generation += 1;
    crate::trace::mark(format!("audio scan: {frames} frames, {} columns, {:?}", d.columns(), loud));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fft_finds_a_tone() {
        let n = 64;
        let mut re: Vec<f32> = (0..n).map(|i| (2.0 * std::f32::consts::PI * 4.0 * i as f32 / n as f32).cos()).collect();
        let mut im = vec![0f32; n];
        fft(&mut re, &mut im);
        let mags: Vec<f32> = (0..n / 2).map(|k| (re[k] * re[k] + im[k] * im[k]).sqrt()).collect();
        let top = mags.iter().enumerate().max_by(|a, b| a.1.partial_cmp(b.1).unwrap()).unwrap().0;
        assert_eq!(top, 4);
    }

    /// BS.1770: a full-scale 1 kHz sine on one channel reads -3.01 LUFS.
    #[test]
    fn loudness_of_a_sine() {
        let rate = 48000.0;
        let (mut a, mut b) = k_weighting(rate);
        let mut subs = Vec::new();
        let (mut e, mut n) = (0f64, 0);
        for i in 0..(rate as usize * 5) {
            let v = (2.0 * std::f64::consts::PI * 1000.0 * i as f64 / rate).sin();
            let k = b.run(a.run(v));
            e += k * k;
            n += 1;
            if n == 4800 {
                subs.push(e / 4800.0);
                e = 0.0;
                n = 0;
            }
        }
        let l = integrated(&subs).unwrap();
        assert!((l - -3.01).abs() < 0.1, "{l}");
    }
}
