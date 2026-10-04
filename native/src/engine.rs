//! The decode bookkeeping that needs no window or GPU: cache keys and size buckets, a byte-budgeted LRU,
//! and which neighbours to preload (ports of `ImageCache.cs`, `PreloadPlan` and `MainViewModel.BuildPreloadWindow`).

use std::collections::HashMap;
use std::hash::Hash;
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};

/// The low-res placeholder tier: decoded into a `LOW_DIM` box, first frame only. Its own bucket value so it can
/// never collide with a sharp one.
pub const LOW: i32 = -1;
/// Full resolution (zoomed past 70% of native).
pub const FULL: i32 = i32::MAX;
/// Placeholder box edge, device pixels.
pub const LOW_DIM: u32 = 1280;

/// Quantizes a target edge to 256 px steps, so small window resizes reuse the same decode.
pub fn bucket_for(max_dim: u32) -> i32 {
    (max_dim.div_ceil(256).max(1) * 256) as i32
}

/// The decode box for a bucket: `(0, 0)` means full resolution.
pub fn box_for(bucket: i32) -> (u32, u32) {
    match bucket {
        LOW => (LOW_DIM, LOW_DIM),
        FULL => (0, 0),
        b => (b as u32, b as u32),
    }
}

/// A file's last-write time: part of every cache key, so an edited file never shows its stale decode.
pub fn stamp(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.last_write_time()).unwrap_or(0)
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Key {
    pub path: PathBuf,
    pub stamp: u64,
    pub bucket: i32,
}

impl Key {
    pub fn new(path: &Path, stamp: u64, bucket: i32) -> Key {
        Key { path: path.to_path_buf(), stamp, bucket }
    }
}

/// Least-recently-used map with a byte budget. Small (tens of entries), so eviction just scans.
pub struct Lru<K, V> {
    map: HashMap<K, (V, usize, u64)>,
    bytes: usize,
    pub budget: usize,
    tick: u64,
}

impl<K: Eq + Hash + Clone, V> Lru<K, V> {
    pub fn new(budget: usize) -> Self {
        Lru { map: HashMap::new(), bytes: 0, budget, tick: 0 }
    }

    pub fn get(&mut self, k: &K) -> Option<&V> {
        self.tick += 1;
        let t = self.tick;
        self.map.get_mut(k).map(|e| {
            e.2 = t;
            &e.0
        })
    }

    pub fn contains(&self, k: &K) -> bool {
        self.map.contains_key(k)
    }

    #[cfg(test)]
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// Inserts (replacing), then evicts least-recently-used entries until within budget, never the new one.
    pub fn insert(&mut self, k: K, v: V, bytes: usize) {
        self.tick += 1;
        if let Some((_, old, _)) = self.map.insert(k.clone(), (v, bytes, self.tick)) {
            self.bytes -= old;
        }
        self.bytes += bytes;
        while self.bytes > self.budget && self.map.len() > 1 {
            let Some(oldest) = self.map.iter().filter(|(key, _)| **key != k).min_by_key(|(_, e)| e.2).map(|(key, _)| key.clone()) else {
                break;
            };
            self.remove(&oldest);
        }
    }

    pub fn remove(&mut self, k: &K) -> Option<V> {
        self.map.remove(k).map(|(v, b, _)| {
            self.bytes -= b;
            v
        })
    }

    pub fn keys(&self) -> impl Iterator<Item = &K> {
        self.map.keys()
    }

    pub fn values(&self) -> impl Iterator<Item = &V> {
        self.map.values().map(|e| &e.0)
    }

    pub fn clear(&mut self) {
        self.map.clear();
        self.bytes = 0;
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tier {
    /// Decoded at the viewport's bucket: shows sharp the moment you arrive.
    Sharp,
    /// The cheap placeholder, a wider ring running ahead of a held key.
    Low,
}

const SHARP_FORWARD: [isize; 3] = [1, 2, -1];
const SHARP_BACKWARD: [isize; 3] = [-1, -2, 1];
const LOW_FORWARD: [isize; 8] = [1, 2, 3, 4, 5, -1, -2, -3];
const LOW_BACKWARD: [isize; 8] = [-1, -2, -3, -4, -5, 1, 2, 3];

/// Neighbours of `current` among `n` images to preload, in priority order: the next image sharp, its
/// placeholder, the other sharp neighbours, then the wide placeholder ring, biased in the direction of travel.
/// Wraps around the folder ends (as navigation does); never lists the current image or a duplicate per tier.
pub fn preload_window(n: usize, current: usize, forward: bool) -> Vec<(usize, Tier)> {
    if n <= 1 || current >= n {
        return Vec::new();
    }
    let (sharp, low) = if forward { (&SHARP_FORWARD, &LOW_FORWARD[..]) } else { (&SHARP_BACKWARD, &LOW_BACKWARD[..]) };
    let wrap = |d: isize| (((current as isize + d) % n as isize + n as isize) % n as isize) as usize;
    let mut out: Vec<(usize, Tier)> = Vec::new();
    let mut add = |d: isize, tier: Tier| {
        let i = wrap(d);
        if i != current && !out.contains(&(i, tier)) {
            out.push((i, tier));
        }
    };
    add(sharp[0], Tier::Sharp);
    add(low[0], Tier::Low);
    for &d in &sharp[1..] {
        add(d, Tier::Sharp);
    }
    for &d in &low[1..] {
        add(d, Tier::Low);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buckets_quantize_up_to_256() {
        assert_eq!(bucket_for(1), 256);
        assert_eq!(bucket_for(256), 256);
        assert_eq!(bucket_for(257), 512);
        assert_eq!(bucket_for(1200), 1280);
        assert_eq!(box_for(LOW), (1280, 1280));
        assert_eq!(box_for(FULL), (0, 0));
    }

    #[test]
    fn lru_evicts_oldest_but_never_the_newest() {
        let mut c: Lru<&str, u8> = Lru::new(100);
        c.insert("a", 1, 40);
        c.insert("b", 2, 40);
        c.get(&"a"); // a is now more recent than b
        c.insert("c", 3, 40);
        assert!(c.contains(&"a") && c.contains(&"c") && !c.contains(&"b"));
        assert_eq!(c.bytes(), 80);
        c.insert("huge", 4, 500); // over budget alone: kept, everything else goes
        assert!(c.contains(&"huge") && !c.contains(&"a"));
        assert_eq!(c.bytes(), 500);
    }

    #[test]
    fn lru_replace_updates_size() {
        let mut c: Lru<&str, u8> = Lru::new(100);
        c.insert("a", 1, 40);
        c.insert("a", 2, 10);
        assert_eq!(c.bytes(), 10);
        assert_eq!(c.get(&"a"), Some(&2));
    }

    #[test]
    fn preload_order_matches_the_csharp_window() {
        let w = preload_window(100, 50, true);
        assert_eq!(w[0], (51, Tier::Sharp));
        assert_eq!(w[1], (51, Tier::Low));
        assert_eq!(w[2], (52, Tier::Sharp));
        assert_eq!(w[3], (49, Tier::Sharp));
        let lows: Vec<usize> = w.iter().filter(|e| e.1 == Tier::Low).map(|e| e.0).collect();
        assert_eq!(lows, [51, 52, 53, 54, 55, 49, 48, 47]);
        let back = preload_window(100, 50, false);
        assert_eq!(back[0], (49, Tier::Sharp));
    }

    #[test]
    fn preload_wraps_and_skips_current_and_duplicates() {
        let w = preload_window(3, 0, true);
        assert!(w.iter().all(|e| e.0 != 0));
        assert!(w.contains(&(2, Tier::Sharp))); // -1 wraps to the end
        let sharps = w.iter().filter(|e| e.1 == Tier::Sharp).count();
        assert_eq!(sharps, 2); // only two other images exist
        assert!(preload_window(1, 0, true).is_empty());
    }
}
