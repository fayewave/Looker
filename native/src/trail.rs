//! The files shown, in order, for the mouse's Back and Forward buttons: a browser's history, of files (Faye,
//! 2026-11-11). Showing a file drops the Forward entries; going back or forward moves along without recording.

use std::path::{Path, PathBuf};

/// Entries kept; the oldest go first.
const CAPACITY: usize = 500;

#[derive(Default)]
pub struct Trail {
    files: Vec<PathBuf>,
    at: Option<usize>,
}

fn same(a: &Path, b: &Path) -> bool {
    a.as_os_str().eq_ignore_ascii_case(b.as_os_str())
}

impl Trail {
    /// A file was shown. Showing the current one again (a reload, or the end of a Back) is not a move.
    pub fn record(&mut self, path: &Path) {
        if let Some(i) = self.at {
            if same(&self.files[i], path) {
                return;
            }
            self.files.truncate(i + 1);
        }
        self.files.push(path.to_path_buf());
        if self.files.len() > CAPACITY {
            self.files.remove(0);
        }
        self.at = Some(self.files.len() - 1);
    }

    /// One step back (`-1`) or forward (`1`), skipping files that are gone; the file to show.
    pub fn step(&mut self, dir: isize) -> Option<PathBuf> {
        let mut i = self.at? as isize;
        loop {
            i += dir;
            let f = self.files.get(usize::try_from(i).ok()?)?;
            if f.exists() {
                self.at = Some(i as usize);
                return Some(f.clone());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn back_and_forward_like_a_browser() {
        let dir = std::env::temp_dir().join("looker-trail-test");
        std::fs::create_dir_all(&dir).unwrap();
        let [a, b, c, d] = ["a", "b", "c", "d"].map(|n| {
            let p = dir.join(n);
            std::fs::write(&p, b"x").unwrap();
            p
        });
        let mut t = Trail::default();
        for p in [&a, &b, &c] {
            t.record(p);
        }
        assert_eq!(t.step(-1).as_ref(), Some(&b));
        t.record(&b); // the shown file reports itself: no move
        assert_eq!(t.step(-1).as_ref(), Some(&a));
        assert_eq!(t.step(-1), None);
        assert_eq!(t.step(1).as_ref(), Some(&b));
        t.record(&d); // a new file drops c
        assert_eq!(t.step(1), None);
        assert_eq!(t.step(-1).as_ref(), Some(&b));
        // A deleted file is stepped over.
        std::fs::remove_file(&b).unwrap();
        t.step(1);
        assert_eq!(t.step(-1).as_ref(), Some(&a));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
