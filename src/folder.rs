//! Folder model (FR-3): the sorted list of images in one directory and
//! navigation over it. Pure logic — no GTK types — so the future
//! explorer iteration can reuse it (NFR-6.1). Filesystem events from
//! the GIO monitor are fed in via [`Folder::insert`] / [`Folder::remove`].

use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::config::{Sort, SortOrder, is_supported};

#[derive(Debug, Clone)]
struct Entry {
    path: PathBuf,
    /// Only meaningful under `SortOrder::Date`; reading it costs a stat
    /// per file, so name sorting leaves it at the epoch rather than pay
    /// for a value nothing compares (see `mtime_of`).
    mtime: SystemTime,
}

#[derive(Debug)]
pub struct Folder {
    entries: Vec<Entry>,
    sort: Sort,
}

impl Folder {
    /// Scan `dir` for supported images. Unreadable entries are skipped.
    pub fn scan(dir: &Path, sort: Sort) -> std::io::Result<Folder> {
        let mut entries = Vec::new();
        for res in std::fs::read_dir(dir)? {
            let Ok(de) = res else { continue };
            let path = de.path();
            // Extension first: it is pure string work, and it rejects most
            // of a mixed directory before anything touches the filesystem.
            // `Path::is_file` was doing that work for every entry, stat
            // included, only to discard the answer a moment later.
            if !is_supported(&path) || !is_regular_file(&de, &path) {
                continue;
            }
            entries.push(Entry {
                mtime: mtime_of(&de, sort),
                path,
            });
        }
        let mut folder = Folder { entries, sort };
        folder.entries.sort_by(|a, b| folder_cmp(a, b, sort));
        Ok(folder)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn get(&self, index: usize) -> Option<&Path> {
        self.entries.get(index).map(|e| e.path.as_path())
    }

    pub fn index_of(&self, path: &Path) -> Option<usize> {
        self.entries.iter().position(|e| e.path == path)
    }

    /// Index to show after `index`, honoring `wrap` (FR-3.3).
    pub fn next(&self, index: usize, wrap: bool) -> Option<usize> {
        if index + 1 < self.entries.len() {
            Some(index + 1)
        } else if wrap && !self.entries.is_empty() {
            Some(0)
        } else {
            None
        }
    }

    pub fn prev(&self, index: usize, wrap: bool) -> Option<usize> {
        if index > 0 {
            Some(index - 1)
        } else if wrap && !self.entries.is_empty() {
            Some(self.entries.len() - 1)
        } else {
            None
        }
    }

    /// Insert a newly appeared file at its sorted position (FR-3.5).
    /// Returns its index, or None if unsupported or already present
    /// (guards against monitor/undo double-insertion).
    pub fn insert(&mut self, path: &Path) -> Option<usize> {
        if !is_supported(path) || self.index_of(path).is_some() {
            return None;
        }
        let mtime = match self.sort.order {
            SortOrder::Date => std::fs::metadata(path)
                .and_then(|m| m.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH),
            SortOrder::Name => SystemTime::UNIX_EPOCH,
        };
        let entry = Entry {
            path: path.to_path_buf(),
            mtime,
        };
        let sort = self.sort;
        let pos = self
            .entries
            .partition_point(|e| folder_cmp(e, &entry, sort) == Ordering::Less);
        self.entries.insert(pos, entry);
        Some(pos)
    }

    /// Remove a vanished file. Returns its former index.
    pub fn remove(&mut self, path: &Path) -> Option<usize> {
        let pos = self.index_of(path)?;
        self.entries.remove(pos);
        Some(pos)
    }
}

/// Whether a directory entry is a regular file. `DirEntry::file_type`
/// comes from readdir's `d_type` on Linux, so it usually costs no syscall
/// at all — but it describes the *link*, so a symlink has to be followed
/// the slow way or symlinked images would vanish from the folder.
fn is_regular_file(de: &std::fs::DirEntry, path: &Path) -> bool {
    match de.file_type() {
        Ok(t) if t.is_symlink() => path.is_file(),
        Ok(t) => t.is_file(),
        Err(_) => path.is_file(),
    }
}

/// Modification time, read only when something will actually order by it
/// — it is a second stat per file, and name sorting never looks at it.
fn mtime_of(de: &std::fs::DirEntry, sort: Sort) -> SystemTime {
    match sort.order {
        SortOrder::Date => de
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH),
        SortOrder::Name => SystemTime::UNIX_EPOCH,
    }
}

fn folder_cmp(a: &Entry, b: &Entry, sort: Sort) -> Ordering {
    let ord = folder_cmp_forward(a, b, sort.order);
    if sort.reverse { ord.reverse() } else { ord }
}

fn folder_cmp_forward(a: &Entry, b: &Entry, order: SortOrder) -> Ordering {
    match order {
        SortOrder::Name => natural_cmp(
            &a.path.file_name().unwrap_or_default().to_string_lossy(),
            &b.path.file_name().unwrap_or_default().to_string_lossy(),
        ),
        // Newest first (FR-3.2); name breaks ties for a stable order.
        SortOrder::Date => b.mtime.cmp(&a.mtime).then_with(|| {
            natural_cmp(
                &a.path.file_name().unwrap_or_default().to_string_lossy(),
                &b.path.file_name().unwrap_or_default().to_string_lossy(),
            )
        }),
    }
}

/// Case-insensitive natural ordering: `img2` < `img10` (FR-3.2).
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let mut ca = a.chars().peekable();
    let mut cb = b.chars().peekable();
    loop {
        match (ca.peek().copied(), cb.peek().copied()) {
            (None, None) => return a.cmp(b), // full tie-break for stability
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) => {
                if x.is_ascii_digit() && y.is_ascii_digit() {
                    let na = take_number(&mut ca);
                    let nb = take_number(&mut cb);
                    match na.cmp(&nb) {
                        Ordering::Equal => {}
                        ord => return ord,
                    }
                } else {
                    let (lx, ly) = (
                        x.to_lowercase().next().unwrap_or(x),
                        y.to_lowercase().next().unwrap_or(y),
                    );
                    match lx.cmp(&ly) {
                        Ordering::Equal => {
                            ca.next();
                            cb.next();
                        }
                        ord => return ord,
                    }
                }
            }
        }
    }
}

/// Consume a run of digits as a number; leading zeros don't change value.
fn take_number(it: &mut std::iter::Peekable<std::str::Chars>) -> u128 {
    let mut n: u128 = 0;
    while let Some(c) = it.peek().copied() {
        if let Some(d) = c.to_digit(10) {
            n = n.saturating_mul(10).saturating_add(u128::from(d));
            it.next();
        } else {
            break;
        }
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;

    fn by_name() -> Sort {
        Sort {
            order: SortOrder::Name,
            reverse: false,
        }
    }

    fn names(f: &Folder) -> Vec<String> {
        (0..f.len())
            .map(|i| {
                f.get(i)
                    .unwrap()
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect()
    }

    fn tempdir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("open-mpv-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn natural_ordering() {
        assert_eq!(natural_cmp("img2.jpg", "img10.jpg"), Ordering::Less);
        assert_eq!(natural_cmp("img10.jpg", "img2.jpg"), Ordering::Greater);
        assert_eq!(natural_cmp("a.png", "B.png"), Ordering::Less); // case-insensitive
        assert_eq!(
            natural_cmp("img007.jpg", "img7.jpg"),
            natural_cmp("img007.jpg", "img7.jpg")
        ); // stable
        assert_eq!(natural_cmp("x.jpg", "x.jpg"), Ordering::Equal);
        assert_eq!(natural_cmp("9.jpg", "10.jpg"), Ordering::Less);
    }

    #[test]
    fn scan_sorts_and_filters() {
        let dir = tempdir("scan");
        for name in ["b10.jpg", "b2.jpg", "a.png", "notes.txt", "z.gif"] {
            File::create(dir.join(name)).unwrap();
        }
        let folder = Folder::scan(&dir, by_name()).unwrap();
        let names: Vec<_> = (0..folder.len())
            .map(|i| {
                folder
                    .get(i)
                    .unwrap()
                    .file_name()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .to_string()
            })
            .collect();
        assert_eq!(names, ["a.png", "b2.jpg", "b10.jpg", "z.gif"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The scan reads `d_type` from readdir rather than stat-ing every
    /// entry, but `d_type` describes the link, not its target — a symlink
    /// has to be followed or symlinked images disappear from the folder.
    #[test]
    fn symlinked_images_are_found_and_broken_links_are_not() {
        let dir = tempdir("symlink");
        File::create(dir.join("real.jpg")).unwrap();
        std::os::unix::fs::symlink(dir.join("real.jpg"), dir.join("link.jpg")).unwrap();
        std::os::unix::fs::symlink(dir.join("gone.jpg"), dir.join("broken.jpg")).unwrap();

        let folder = Folder::scan(&dir, by_name()).unwrap();
        assert_eq!(
            names(&folder),
            ["link.jpg", "real.jpg"],
            "a symlink to an image counts; a dangling one does not"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Guards the stat that name sorting skips: date sorting still has to
    /// read real modification times.
    #[test]
    fn date_sort_orders_by_modification_time() {
        let dir = tempdir("date");
        for name in ["oldest.jpg", "middle.jpg", "newest.jpg"] {
            File::create(dir.join(name)).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let by_date = |reverse| Sort {
            order: SortOrder::Date,
            reverse,
        };
        // Newest first is the default direction for dates (FR-3.2).
        let folder = Folder::scan(&dir, by_date(false)).unwrap();
        assert_eq!(names(&folder), ["newest.jpg", "middle.jpg", "oldest.jpg"]);
        let folder = Folder::scan(&dir, by_date(true)).unwrap();
        assert_eq!(names(&folder), ["oldest.jpg", "middle.jpg", "newest.jpg"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn reverse_flips_the_order_and_insertion_follows_it() {
        let dir = tempdir("reverse");
        for name in ["a.jpg", "b2.jpg", "b10.jpg"] {
            File::create(dir.join(name)).unwrap();
        }
        let sort = Sort {
            order: SortOrder::Name,
            reverse: true,
        };
        let mut folder = Folder::scan(&dir, sort).unwrap();
        // Natural order reversed: b10 before b2, not lexical.
        assert_eq!(names(&folder), ["b10.jpg", "b2.jpg", "a.jpg"]);
        // A file appearing later lands in the reversed position too.
        let c = dir.join("c.jpg");
        File::create(&c).unwrap();
        assert_eq!(folder.insert(&c), Some(0));
        assert_eq!(names(&folder), ["c.jpg", "b10.jpg", "b2.jpg", "a.jpg"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn navigation_and_wrap() {
        let dir = tempdir("nav");
        for name in ["1.jpg", "2.jpg", "3.jpg"] {
            File::create(dir.join(name)).unwrap();
        }
        let folder = Folder::scan(&dir, by_name()).unwrap();
        assert_eq!(folder.next(0, false), Some(1));
        assert_eq!(folder.next(2, false), None); // no wrap at end (FR-3.3)
        assert_eq!(folder.next(2, true), Some(0));
        assert_eq!(folder.prev(0, false), None);
        assert_eq!(folder.prev(0, true), Some(2));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn insert_remove_and_dedup() {
        let dir = tempdir("mut");
        for name in ["a.jpg", "c.jpg"] {
            File::create(dir.join(name)).unwrap();
        }
        let mut folder = Folder::scan(&dir, by_name()).unwrap();
        let b = dir.join("b.jpg");
        File::create(&b).unwrap();
        assert_eq!(folder.insert(&b), Some(1)); // sorted position
        assert_eq!(folder.insert(&b), None); // double-insertion guarded
        assert_eq!(folder.insert(&dir.join("x.txt")), None); // unsupported
        assert_eq!(folder.remove(&b), Some(1));
        assert_eq!(folder.remove(&b), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
