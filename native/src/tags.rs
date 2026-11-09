//! Favourites and colour tags for files and folders, as Transient keeps them: in Looker's own data folder
//! (`tags.txt`), never written into the files. A line per tagged path: `<fav 0|1> <colour 0-7> <path>`,
//! colour 0 meaning none and 1-7 the colours in `explorer::COLOURS`. Moving or renaming through Looker
//! carries a path's tags along (`moved`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::explorer::{self, Collection};

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Tag {
    pub favourite: bool,
    /// An index into `explorer::COLOURS`.
    pub colour: Option<u8>,
}

impl Tag {
    fn is_empty(&self) -> bool {
        !self.favourite && self.colour.is_none()
    }
}

#[derive(Default)]
pub struct Tags {
    /// Keyed by `explorer::norm`; the path as it was tagged alongside.
    map: HashMap<String, (PathBuf, Tag)>,
    /// Bumped on every change, so lists showing a collection know to list again.
    pub version: u64,
}

fn file() -> Option<PathBuf> {
    crate::settings::data_dir().map(|d| d.join("tags.txt"))
}

impl Tags {
    pub fn load() -> Tags {
        let text = file().and_then(|f| std::fs::read_to_string(f).ok()).unwrap_or_default();
        Tags::parse(&text)
    }

    fn parse(text: &str) -> Tags {
        let mut t = Tags::default();
        for line in text.lines() {
            let mut it = line.splitn(3, ' ');
            let (Some(f), Some(c), Some(p)) = (it.next(), it.next(), it.next()) else { continue };
            let colour = c.parse::<u8>().ok().filter(|&c| (1..=explorer::COLOURS.len() as u8).contains(&c)).map(|c| c - 1);
            let tag = Tag { favourite: f == "1", colour };
            if !tag.is_empty() && !p.is_empty() {
                let path = PathBuf::from(p);
                t.map.insert(explorer::norm(&path), (path, tag));
            }
        }
        t
    }

    fn format(&self) -> String {
        let mut lines: Vec<String> = self
            .map
            .values()
            .map(|(p, t)| format!("{} {} {}", t.favourite as u8, t.colour.map_or(0, |c| c + 1), p.display()))
            .collect();
        lines.sort();
        lines.join("\n")
    }

    fn save(&mut self) {
        self.version += 1;
        if let Some(f) = file() {
            let _ = std::fs::write(f, self.format());
        }
    }

    pub fn get(&self, path: &Path) -> Tag {
        self.map.get(&explorer::norm(path)).map(|(_, t)| *t).unwrap_or_default()
    }

    fn set(&mut self, path: &Path, tag: Tag) {
        let key = explorer::norm(path);
        if tag.is_empty() {
            self.map.remove(&key);
        } else {
            self.map.insert(key, (path.to_path_buf(), tag));
        }
    }

    /// Stars or unstars all of `paths`: stars them unless every one is starred already (Ctrl+D on a mixed
    /// selection stars the rest). Returns whether they're starred now.
    pub fn toggle_favourite(&mut self, paths: &[PathBuf]) -> bool {
        let on = !paths.iter().all(|p| self.get(p).favourite);
        for p in paths {
            let t = self.get(p);
            self.set(p, Tag { favourite: on, ..t });
        }
        self.save();
        on
    }

    /// Gives `paths` colour `c` (None clears it); a colour they all have already is taken off instead.
    pub fn set_colour(&mut self, paths: &[PathBuf], c: Option<u8>) {
        let c = if c.is_some() && paths.iter().all(|p| self.get(p).colour == c) { None } else { c };
        for p in paths {
            let t = self.get(p);
            self.set(p, Tag { colour: c, ..t });
        }
        self.save();
    }

    /// The paths in a collection, in no particular order (the list sorts them).
    pub fn in_collection(&self, c: Collection) -> Vec<PathBuf> {
        self.map
            .values()
            .filter(|(_, t)| match c {
                Collection::Favourites => t.favourite,
                Collection::Colour(i) => t.colour == Some(i),
            })
            .map(|(p, _)| p.clone())
            .collect()
    }

    /// Which colours have anything tagged (the sidebar shows those).
    pub fn colours_in_use(&self) -> Vec<u8> {
        let mut v: Vec<u8> = self.map.values().filter_map(|(_, t)| t.colour).collect();
        v.sort_unstable();
        v.dedup();
        v
    }

    /// `from` (a file, or a folder and everything in it) is now at `to`: its tags follow.
    pub fn moved(&mut self, from: &Path, to: &Path) {
        let hits: Vec<(String, PathBuf, Tag)> = self
            .map
            .iter()
            .filter_map(|(k, (p, t))| explorer::rebase(p, from, to).map(|n| (k.clone(), n, *t)))
            .collect();
        if hits.is_empty() {
            return;
        }
        for (k, n, t) in hits {
            self.map.remove(&k);
            self.map.insert(explorer::norm(&n), (n, t));
        }
        self.save();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_follows_moves() {
        let mut t = Tags::parse("1 0 C:\\a\\one.jpg\n0 3 C:\\a\\sub\nbogus\n0 0 C:\\none.jpg");
        assert!(t.get(Path::new(r"c:\A\ONE.jpg")).favourite);
        assert_eq!(t.get(Path::new(r"C:\a\sub")).colour, Some(2));
        assert_eq!(t.get(Path::new(r"C:\none.jpg")), Tag::default());
        let again = Tags::parse(&t.format());
        assert_eq!(again.map.len(), 2);
        // Moving the folder carries what's in it (the save writes nowhere in tests without a data dir mismatch).
        t.map.insert(explorer::norm(Path::new(r"C:\a\sub\x.png")), (PathBuf::from(r"C:\a\sub\x.png"), Tag { favourite: true, colour: None }));
        let hits: Vec<_> = t.map.values().filter_map(|(p, _)| explorer::rebase(p, Path::new(r"C:\a\sub"), Path::new(r"D:\b"))).collect();
        assert!(hits.contains(&PathBuf::from(r"D:\b\x.png")));
        assert_eq!(t.colours_in_use(), vec![2]);
        assert_eq!(t.in_collection(Collection::Favourites).len(), 2);
    }
}
