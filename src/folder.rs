//! Navigation set model (FR-3): selected-folder identity, sorted local media,
//! logical Viewer destination, and mutation outcomes. Pure logic — no GTK or
//! GIO types — so the future Explorer can reuse it (NFR-6.1). The window
//! adapter translates filesystem monitor events into these transitions.

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
    directory: PathBuf,
    entries: Vec<Entry>,
    sort: Sort,
}

/// One selected folder and the logical Viewer destination within it.
///
/// GTK and GIO feed changes into this model, but presentation and monitoring
/// stay in `window` (NFR-6.1). A generation belongs to each destination so
/// asynchronous media work can prove that it still targets the active item.
#[derive(Debug, Default)]
pub struct Navigation {
    folder: Option<Folder>,
    current: Option<usize>,
    generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Destination {
    pub index: usize,
    pub path: PathBuf,
    pub generation: u64,
}

/// Filesystem facts gathered by the asynchronous window adapter before a
/// supported regular file enters the Navigation set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSnapshot {
    path: PathBuf,
    modified: SystemTime,
}

impl FileSnapshot {
    pub fn new(path: PathBuf, modified: SystemTime) -> Self {
        Self { path, modified }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemovalOutcome {
    NotFound,
    CurrentPreserved,
    CurrentRemoved(Option<Destination>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenameOutcome {
    Preserved,
    Renamed(Destination),
    Removed(Option<Destination>),
}

impl Folder {
    /// Scan `dir` for supported media. Unreadable entries are skipped.
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
        let mut folder = Folder {
            directory: dir.to_path_buf(),
            entries,
            sort,
        };
        folder.entries.sort_by(|a, b| folder_cmp(a, b, sort));
        Ok(folder)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn get(&self, index: usize) -> Option<&Path> {
        self.entries.get(index).map(|e| e.path.as_path())
    }

    fn index_of(&self, path: &Path) -> Option<usize> {
        self.entries.iter().position(|e| e.path == path)
    }

    /// Index to show after `index`, honoring `wrap` (FR-3.3).
    fn next(&self, index: usize, wrap: bool) -> Option<usize> {
        if index + 1 < self.entries.len() {
            Some(index + 1)
        } else if wrap && !self.entries.is_empty() {
            Some(0)
        } else {
            None
        }
    }

    fn prev(&self, index: usize, wrap: bool) -> Option<usize> {
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
    fn insert(&mut self, snapshot: FileSnapshot) -> Option<usize> {
        if snapshot.path.parent() != Some(self.directory.as_path())
            || !is_supported(&snapshot.path)
            || self.index_of(&snapshot.path).is_some()
        {
            return None;
        }
        let mtime = match self.sort.order {
            SortOrder::Date => snapshot.modified,
            SortOrder::Name => SystemTime::UNIX_EPOCH,
        };
        let entry = Entry {
            path: snapshot.path,
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
    fn remove(&mut self, path: &Path) -> Option<usize> {
        let pos = self.index_of(path)?;
        self.entries.remove(pos);
        Some(pos)
    }
}

impl Navigation {
    pub fn install(&mut self, folder: Folder) {
        self.folder = Some(folder);
        self.current = None;
        self.bump_generation();
    }

    pub fn directory(&self) -> Option<&Path> {
        self.folder
            .as_ref()
            .map(|folder| folder.directory.as_path())
    }

    pub fn len(&self) -> usize {
        self.folder.as_ref().map_or(0, Folder::len)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn get(&self, index: usize) -> Option<&Path> {
        self.folder.as_ref()?.get(index)
    }

    pub fn index_of(&self, path: &Path) -> Option<usize> {
        self.folder.as_ref()?.index_of(path)
    }

    pub fn current_index(&self) -> Option<usize> {
        self.current
    }

    pub fn current_path(&self) -> Option<&Path> {
        self.get(self.current?)
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn is_current_generation(&self, generation: u64) -> bool {
        self.generation == generation
    }

    pub fn select(&mut self, index: usize) -> Option<Destination> {
        self.get(index)?;
        self.current = Some(index);
        self.bump_generation();
        self.destination()
    }

    pub fn clear_current(&mut self) {
        self.current = None;
        self.bump_generation();
    }

    /// Invalidate work for the current destination without losing its
    /// logical position, as when its decode resolves to an error state.
    pub fn supersede(&mut self) {
        self.bump_generation();
    }

    pub fn next(&self, wrap: bool) -> Option<usize> {
        let folder = self.folder.as_ref()?;
        match self.current {
            Some(index) => folder.next(index, wrap),
            None if !folder.is_empty() => Some(0),
            None => None,
        }
    }

    pub fn prev(&self, wrap: bool) -> Option<usize> {
        let folder = self.folder.as_ref()?;
        match self.current {
            Some(index) => folder.prev(index, wrap),
            None if !folder.is_empty() => Some(folder.len() - 1),
            None => None,
        }
    }

    pub fn next_from(&self, index: usize, wrap: bool) -> Option<usize> {
        self.folder.as_ref()?.next(index, wrap)
    }

    pub fn prev_from(&self, index: usize, wrap: bool) -> Option<usize> {
        self.folder.as_ref()?.prev(index, wrap)
    }

    pub fn insert(&mut self, snapshot: FileSnapshot) -> Option<usize> {
        self.insert_entry(snapshot)
    }

    pub fn remove(&mut self, path: &Path) -> RemovalOutcome {
        let was_current = self.current_path() == Some(path);
        let Some(index) = self.remove_entry(path) else {
            return RemovalOutcome::NotFound;
        };
        if was_current {
            self.current = (!self.is_empty()).then(|| index.min(self.len() - 1));
            self.bump_generation();
            RemovalOutcome::CurrentRemoved(self.destination())
        } else {
            RemovalOutcome::CurrentPreserved
        }
    }

    pub fn rename(
        &mut self,
        old: &Path,
        new: &Path,
        snapshot: Option<FileSnapshot>,
    ) -> RenameOutcome {
        let current_affected = self
            .current_path()
            .is_some_and(|current| current == old || current == new);

        self.remove_entry(old);
        if new != old {
            // A replace-style rename can overwrite an existing destination.
            // Remove that stale entry so insertion refreshes its metadata and
            // leaves exactly one copy of the destination path.
            self.remove_entry(new);
        }
        let inserted = snapshot.and_then(|snapshot| self.insert_entry(snapshot));

        if current_affected {
            if let Some(index) = inserted {
                self.current = Some(index);
                self.bump_generation();
                self.destination()
                    .map_or(RenameOutcome::Preserved, RenameOutcome::Renamed)
            } else {
                self.current = self
                    .current
                    .filter(|_| !self.is_empty())
                    .map(|index| index.min(self.len() - 1));
                self.bump_generation();
                RenameOutcome::Removed(self.destination())
            }
        } else {
            RenameOutcome::Preserved
        }
    }

    fn insert_entry(&mut self, snapshot: FileSnapshot) -> Option<usize> {
        let index = self.folder.as_mut()?.insert(snapshot)?;
        if self.current.is_some_and(|current| index <= current) {
            self.current = self.current.map(|current| current + 1);
        }
        Some(index)
    }

    fn remove_entry(&mut self, path: &Path) -> Option<usize> {
        let index = self.folder.as_mut()?.remove(path)?;
        if self.current.is_some_and(|current| index < current) {
            self.current = self.current.map(|current| current - 1);
        }
        Some(index)
    }

    fn destination(&self) -> Option<Destination> {
        let index = self.current?;
        Some(Destination {
            index,
            path: self.get(index)?.to_path_buf(),
            generation: self.generation,
        })
    }

    fn bump_generation(&mut self) {
        self.generation = self.generation.wrapping_add(1);
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

    fn snapshot(path: &Path) -> FileSnapshot {
        let modified = std::fs::metadata(path)
            .and_then(|metadata| metadata.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        FileSnapshot::new(path.to_path_buf(), modified)
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
        assert_eq!(folder.insert(snapshot(&c)), Some(0));
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
        assert_eq!(folder.insert(snapshot(&b)), Some(1)); // sorted position
        assert_eq!(folder.insert(snapshot(&b)), None); // double-insertion guarded
        assert_eq!(
            folder.insert(FileSnapshot::new(dir.join("x.txt"), SystemTime::UNIX_EPOCH,)),
            None
        ); // unsupported
        assert_eq!(folder.remove(&b), Some(1));
        assert_eq!(folder.remove(&b), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn insertion_stays_within_the_selected_folder() {
        let dir = tempdir("insert-scope");
        let other = tempdir("insert-scope-other");
        File::create(dir.join("a.jpg")).unwrap();
        File::create(other.join("b.jpg")).unwrap();
        let mut folder = Folder::scan(&dir, by_name()).unwrap();

        assert_eq!(folder.insert(snapshot(&other.join("b.jpg"))), None);
        assert_eq!(names(&folder), ["a.jpg"]);
        std::fs::remove_dir_all(&dir).unwrap();
        std::fs::remove_dir_all(&other).unwrap();
    }

    #[test]
    fn insertion_uses_prevalidated_metadata_without_filesystem_io() {
        let dir = tempdir("prevalidated-insert");
        File::create(dir.join("a.jpg")).unwrap();
        let mut navigation = Navigation::default();
        navigation.install(Folder::scan(&dir, by_name()).unwrap());
        let appeared = dir.join("b.jpg");
        let snapshot = FileSnapshot::new(appeared.clone(), SystemTime::UNIX_EPOCH);

        // The adapter owns filesystem validation. Removing the file proves
        // model insertion consumes only the captured value and cannot stat.
        File::create(&appeared).unwrap();
        std::fs::remove_file(&appeared).unwrap();

        assert_eq!(navigation.insert(snapshot), Some(1));
        assert_eq!(navigation.get(1), Some(appeared.as_path()));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn navigation_of(name: &str, files: &[&str]) -> (PathBuf, Navigation) {
        let dir = tempdir(name);
        for file in files {
            File::create(dir.join(file)).unwrap();
        }
        let mut navigation = Navigation::default();
        navigation.install(Folder::scan(&dir, by_name()).unwrap());
        (dir, navigation)
    }

    #[test]
    fn current_removal_keeps_the_nearest_position() {
        for (name, selected, removed, expected) in [
            ("first", 0, "a.jpg", Some((0, "b.jpg"))),
            ("middle", 1, "b.jpg", Some((1, "c.jpg"))),
            ("last", 2, "c.jpg", Some((1, "b.jpg"))),
        ] {
            let (dir, mut navigation) =
                navigation_of(&format!("remove-{name}"), &["a.jpg", "b.jpg", "c.jpg"]);
            navigation.select(selected).unwrap();

            let outcome = navigation.remove(&dir.join(removed));
            let destination = match outcome {
                RemovalOutcome::CurrentRemoved(destination) => destination,
                other => panic!("expected current removal, got {other:?}"),
            };
            let (index, filename) = expected.unwrap();
            let destination = destination.unwrap();
            assert_eq!(destination.index, index);
            assert_eq!(destination.path.file_name().unwrap(), filename);
            assert_eq!(navigation.current_index(), Some(index));
            std::fs::remove_dir_all(&dir).unwrap();
        }
    }

    #[test]
    fn removing_the_only_item_clears_the_destination() {
        let (dir, mut navigation) = navigation_of("remove-only", &["only.jpg"]);
        navigation.select(0).unwrap();
        assert_eq!(
            navigation.remove(&dir.join("only.jpg")),
            RemovalOutcome::CurrentRemoved(None)
        );
        assert!(navigation.is_empty());
        assert_eq!(navigation.current_path(), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn removal_before_current_preserves_the_logical_item() {
        let (dir, mut navigation) =
            navigation_of("remove-before-current", &["a.jpg", "b.jpg", "c.jpg"]);
        navigation.select(2).unwrap();
        assert_eq!(
            navigation.remove(&dir.join("a.jpg")),
            RemovalOutcome::CurrentPreserved
        );
        assert_eq!(navigation.current_index(), Some(1));
        assert_eq!(navigation.current_path(), Some(dir.join("c.jpg").as_path()));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rename_returns_an_explicit_current_transition() {
        let (dir, mut navigation) = navigation_of("rename-current", &["a.jpg", "b.jpg", "c.jpg"]);
        navigation.select(1).unwrap();
        let renamed = dir.join("z.jpg");
        std::fs::rename(dir.join("b.jpg"), &renamed).unwrap();

        let outcome = navigation.rename(&dir.join("b.jpg"), &renamed, Some(snapshot(&renamed)));
        let RenameOutcome::Renamed(destination) = outcome else {
            panic!("expected a current rename, got {outcome:?}");
        };
        assert_eq!(destination.path, renamed);
        assert_eq!(destination.index, 2);
        assert_eq!(navigation.current_path(), Some(destination.path.as_path()));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rename_to_unsupported_uses_the_current_removal_policy() {
        let (dir, mut navigation) =
            navigation_of("rename-unsupported", &["a.jpg", "b.jpg", "c.jpg"]);
        navigation.select(1).unwrap();
        let renamed = dir.join("b.txt");
        std::fs::rename(dir.join("b.jpg"), &renamed).unwrap();

        let outcome = navigation.rename(&dir.join("b.jpg"), &renamed, None);
        let RenameOutcome::Removed(Some(destination)) = outcome else {
            panic!("expected removal after unsupported rename, got {outcome:?}");
        };
        assert_eq!(destination.index, 1);
        assert_eq!(destination.path, dir.join("c.jpg"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn overwrite_rename_refreshes_either_affected_current_item() {
        for (name, selected) in [("source-current", 0), ("destination-current", 2)] {
            let (dir, mut navigation) =
                navigation_of(&format!("overwrite-{name}"), &["a.jpg", "b.jpg", "c.jpg"]);
            let stale = navigation.select(selected).unwrap().generation;
            let old = dir.join("a.jpg");
            let new = dir.join("c.jpg");
            std::fs::rename(&old, &new).unwrap();

            let RenameOutcome::Renamed(destination) =
                navigation.rename(&old, &new, Some(snapshot(&new)))
            else {
                panic!("an affected current item must be re-presented");
            };
            assert_eq!(navigation.len(), 2);
            assert_eq!(destination.index, 1);
            assert_eq!(destination.path, new);
            assert_eq!(navigation.current_path(), Some(destination.path.as_path()));
            assert!(!navigation.is_current_generation(stale));
            assert!(navigation.is_current_generation(destination.generation));
            std::fs::remove_dir_all(&dir).unwrap();
        }
    }

    #[test]
    fn destination_generation_rejects_stale_work() {
        let (dir, mut navigation) = navigation_of("stale", &["a.jpg", "b.jpg"]);
        let first = navigation.select(0).unwrap();
        assert!(navigation.is_current_generation(first.generation));

        let second = navigation.select(1).unwrap();
        assert!(!navigation.is_current_generation(first.generation));
        assert!(navigation.is_current_generation(second.generation));

        let replacement = tempdir("stale-replacement");
        File::create(replacement.join("c.jpg")).unwrap();
        navigation.install(Folder::scan(&replacement, by_name()).unwrap());
        assert!(!navigation.is_current_generation(second.generation));
        std::fs::remove_dir_all(&dir).unwrap();
        std::fs::remove_dir_all(&replacement).unwrap();
    }

    #[test]
    fn selected_folder_and_viewer_destination_are_plain_model_state() {
        let (dir, mut navigation) = navigation_of("identity", &["a.jpg"]);
        assert_eq!(navigation.directory(), Some(dir.as_path()));
        let destination = navigation.select(0).unwrap();
        assert_eq!(destination.path, dir.join("a.jpg"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
