//! Adapts GIO folder-monitor events into ordered navigation mutations and presentation outcomes.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, SystemTime};

use gtk4::gio;
use gtk4::glib;
use gtk4::glib::clone;
use gtk4::prelude::*;

use crate::config;
use crate::folder::{
    Destination, FileSnapshot, Navigation, NavigationSetId, RemovalOutcome, RenameOutcome,
    SnapshotKind,
};

use super::{App, Arrival};

#[derive(Debug, Clone, PartialEq, Eq)]
enum FsPresentation {
    Unchanged,
    Show(Destination),
    Empty,
}

#[derive(Debug)]
enum FsChange {
    Insert(FileSnapshot),
    Remove(PathBuf),
    Rename {
        old: PathBuf,
        new: PathBuf,
        snapshot: Option<FileSnapshot>,
    },
}

#[derive(Debug)]
struct PendingFsQuery {
    version: u64,
    cancellable: gio::Cancellable,
}

/// Per-path versions keep asynchronous metadata queries ordered without
/// retaining history after the last query completes.
#[derive(Debug, Default)]
pub(super) struct FsQueryVersions {
    next: u64,
    paths: HashMap<PathBuf, PendingFsQuery>,
}

impl FsQueryVersions {
    fn start(&mut self, paths: &[PathBuf]) -> (u64, gio::Cancellable) {
        let version = self.next.wrapping_add(1);
        self.next = version;
        let cancellable = gio::Cancellable::new();
        for_each_unique_path(paths, |path| {
            if let Some(stale) = self.paths.insert(
                path.to_path_buf(),
                PendingFsQuery {
                    version,
                    cancellable: cancellable.clone(),
                },
            ) {
                stale.cancellable.cancel();
            }
        });
        (version, cancellable)
    }

    pub(super) fn supersede(&mut self, paths: &[PathBuf]) {
        for_each_unique_path(paths, |path| {
            if let Some(stale) = self.paths.remove(path) {
                stale.cancellable.cancel();
            }
        });
    }

    fn finish(&mut self, paths: &[PathBuf], version: u64) -> bool {
        let current = paths.iter().all(|path| {
            self.paths
                .get(path)
                .is_some_and(|pending| pending.version == version)
        });
        for_each_unique_path(paths, |path| {
            if self
                .paths
                .get(path)
                .is_some_and(|pending| pending.version == version)
            {
                self.paths.remove(path);
            }
        });
        current
    }

    pub(super) fn cancel_all(&mut self) {
        for pending in self.paths.values() {
            pending.cancellable.cancel();
        }
        self.paths.clear();
    }
}

fn for_each_unique_path(paths: &[PathBuf], mut f: impl FnMut(&Path)) {
    for (index, path) in paths.iter().enumerate() {
        if !paths[..index].contains(path) {
            f(path);
        }
    }
}

impl App {
    // ----- filesystem events (FR-3.5) ----------------------------------

    pub(super) fn on_fs_event(
        self: &Rc<Self>,
        set: NavigationSetId,
        file: &gio::File,
        other: Option<&gio::File>,
        event: gio::FileMonitorEvent,
    ) {
        if self.navigation.borrow().set_id() != Some(set) || self.shutting_down.get() {
            return;
        }
        use gio::FileMonitorEvent as E;
        match event {
            E::Created | E::MovedIn => {
                let Some(path) = file.path().filter(|path| config::is_supported(path)) else {
                    return;
                };
                self.query_fs_snapshot(set, file.clone(), vec![path], move |app, snapshot| {
                    if let Some(snapshot) = snapshot {
                        app.apply_fs_change(set, FsChange::Insert(snapshot), event);
                    }
                });
            }
            E::Deleted | E::MovedOut => {
                let Some(path) = file.path() else {
                    return;
                };
                self.fs_queries
                    .borrow_mut()
                    .supersede(std::slice::from_ref(&path));
                self.apply_fs_change(set, FsChange::Remove(path), event);
            }
            E::Renamed => {
                let Some((old, new_file, new)) = file.path().and_then(|old| {
                    let new_file = other?.clone();
                    let new = new_file.path()?;
                    Some((old, new_file, new))
                }) else {
                    return;
                };
                let paths = vec![old.clone(), new.clone()];
                if config::is_supported(&new) {
                    self.query_fs_snapshot(set, new_file, paths, move |app, snapshot| {
                        app.apply_fs_change(set, FsChange::Rename { old, new, snapshot }, event);
                    });
                } else {
                    self.fs_queries.borrow_mut().supersede(&paths);
                    self.apply_fs_change(
                        set,
                        FsChange::Rename {
                            old,
                            new,
                            snapshot: None,
                        },
                        event,
                    );
                }
            }
            _ => {}
        }
    }

    fn query_fs_snapshot(
        self: &Rc<Self>,
        set: NavigationSetId,
        file: gio::File,
        paths: Vec<PathBuf>,
        apply: impl FnOnce(&Rc<Self>, Option<FileSnapshot>) + 'static,
    ) {
        let Some(snapshot_path) = file.path() else {
            return;
        };
        let (version, cancellable) = self.fs_queries.borrow_mut().start(&paths);
        file.query_info_async(
            "standard::type,time::modified,time::modified-nsec",
            gio::FileQueryInfoFlags::NONE,
            glib::Priority::DEFAULT,
            Some(&cancellable),
            clone!(
                #[strong(rename_to = app)]
                self,
                move |result| {
                    let snapshot = result
                        .ok()
                        .and_then(|info| file_snapshot_from_info(snapshot_path, &info));
                    let current = app.fs_queries.borrow_mut().finish(&paths, version);
                    let same_set = app.navigation.borrow().set_id() == Some(set);
                    if current && same_set && !app.shutting_down.get() {
                        apply(&app, snapshot);
                    }
                }
            ),
        );
    }

    fn apply_fs_change(
        self: &Rc<Self>,
        set: NavigationSetId,
        change: FsChange,
        event: gio::FileMonitorEvent,
    ) {
        let (path, removal) = match &change {
            FsChange::Insert(snapshot) => (snapshot.path(), false),
            FsChange::Remove(path) => (path.as_path(), true),
            FsChange::Rename { old, .. } => (old.as_path(), false),
        };
        let path = path.to_path_buf();
        let (before_generation, presentation) = {
            let mut navigation = self.navigation.borrow_mut();
            let before = navigation.generation();
            let Some(presentation) = apply_fs_change_for_set(&mut navigation, set, change) else {
                return;
            };
            (before, presentation)
        };
        let current_changed = !matches!(presentation, FsPresentation::Unchanged);
        crate::applog!(
            "fs event: {event:?} {}{}",
            path.display(),
            if current_changed {
                " (current destination changed)"
            } else {
                ""
            }
        );
        match presentation {
            FsPresentation::Show(destination) => {
                self.show_destination(destination, Arrival::Direct)
            }
            FsPresentation::Empty => self.empty_state("No media left in this folder"),
            FsPresentation::Unchanged => {}
        }
        self.update_pos_label();
        if removal {
            let after_generation = self.navigation.borrow().generation();
            self.operations.borrow_mut().observe_removal(
                set,
                &path,
                before_generation,
                after_generation,
                current_changed,
            );
        }
    }
}

fn file_snapshot_from_info(path: PathBuf, info: &gio::FileInfo) -> Option<FileSnapshot> {
    let kind = if info.file_type() == gio::FileType::Regular {
        SnapshotKind::Regular
    } else {
        SnapshotKind::Other
    };
    let timestamp = Duration::from_secs(info.attribute_uint64("time::modified")).saturating_add(
        Duration::from_nanos(u64::from(info.attribute_uint32("time::modified-nsec"))),
    );
    let modified = SystemTime::UNIX_EPOCH
        .checked_add(timestamp)
        .unwrap_or(SystemTime::UNIX_EPOCH);
    Some(FileSnapshot::new(path, modified, kind))
}

/// Apply one already-prepared filesystem mutation to the plain-Rust owner and
/// return only the presentation work the window adapter must perform.
fn apply_fs_change(navigation: &mut Navigation, change: FsChange) -> FsPresentation {
    match change {
        FsChange::Insert(snapshot) => {
            navigation.insert(snapshot);
            FsPresentation::Unchanged
        }
        FsChange::Remove(path) => match navigation.remove(&path) {
            RemovalOutcome::CurrentRemoved(Some(destination)) => FsPresentation::Show(destination),
            RemovalOutcome::CurrentRemoved(None) => FsPresentation::Empty,
            RemovalOutcome::NotFound | RemovalOutcome::CurrentPreserved => {
                FsPresentation::Unchanged
            }
        },
        FsChange::Rename { old, new, snapshot } => match navigation.rename(&old, &new, snapshot) {
            RenameOutcome::Renamed(destination) | RenameOutcome::Removed(Some(destination)) => {
                FsPresentation::Show(destination)
            }
            RenameOutcome::Removed(None) => FsPresentation::Empty,
            RenameOutcome::Preserved => FsPresentation::Unchanged,
        },
    }
}

fn apply_fs_change_for_set(
    navigation: &mut Navigation,
    set: NavigationSetId,
    change: FsChange,
) -> Option<FsPresentation> {
    (navigation.set_id() == Some(set)).then(|| apply_fs_change(navigation, change))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Sort, SortOrder};
    use crate::folder::{Folder, Navigation};
    use gtk4::gio::prelude::CancellableExt;

    fn by_name() -> Sort {
        Sort {
            order: SortOrder::Name,
            reverse: false,
        }
    }

    fn folder_of(name: &str, files: &[&str]) -> (PathBuf, Folder) {
        let dir =
            std::env::temp_dir().join(format!("open-mpv-window-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for file in files {
            std::fs::File::create(dir.join(file)).unwrap();
        }
        let folder = Folder::scan(&dir, by_name()).unwrap();
        (dir, folder)
    }

    fn navigation_of(name: &str, files: &[&str]) -> (PathBuf, Navigation) {
        let (dir, folder) = folder_of(name, files);
        let mut navigation = Navigation::default();
        navigation.install(folder);
        (dir, navigation)
    }

    #[test]
    fn external_removal_events_present_the_model_outcome() {
        for (name, selected, removed, expected_index, expected_name) in [
            ("delete-middle", 1, "b.jpg", 1, "c.jpg"),
            ("move-out-last", 2, "c.jpg", 1, "b.jpg"),
        ] {
            let (dir, mut navigation) = navigation_of(name, &["a.jpg", "b.jpg", "c.jpg"]);
            navigation.select(selected).unwrap();
            let presentation =
                apply_fs_change(&mut navigation, FsChange::Remove(dir.join(removed)));
            let FsPresentation::Show(destination) = presentation else {
                panic!("expected a replacement destination, got {presentation:?}");
            };
            assert_eq!(destination.index, expected_index);
            assert_eq!(destination.path.file_name().unwrap(), expected_name);
            std::fs::remove_dir_all(&dir).unwrap();
        }
    }

    #[test]
    fn external_rename_to_unsupported_presents_the_nearest_item() {
        let (dir, mut navigation) =
            navigation_of("rename-unsupported-event", &["a.jpg", "b.jpg", "c.jpg"]);
        navigation.select(1).unwrap();
        let old = dir.join("b.jpg");
        let new = dir.join("b.txt");
        std::fs::rename(&old, &new).unwrap();

        let presentation = apply_fs_change(
            &mut navigation,
            FsChange::Rename {
                old,
                new,
                snapshot: None,
            },
        );
        let FsPresentation::Show(destination) = presentation else {
            panic!("expected a replacement destination, got {presentation:?}");
        };
        assert_eq!(destination.index, 1);
        assert_eq!(destination.path, dir.join("c.jpg"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn external_removal_of_the_only_item_presents_empty_state() {
        let (dir, mut navigation) = navigation_of("delete-only", &["only.jpg"]);
        navigation.select(0).unwrap();
        assert_eq!(
            apply_fs_change(&mut navigation, FsChange::Remove(dir.join("only.jpg"))),
            FsPresentation::Empty
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn stale_monitor_event_cannot_mutate_a_reopened_folder() {
        let (dir, mut navigation) = navigation_of("stale-monitor-set", &["a.jpg", "b.jpg"]);
        navigation.select(0).unwrap();
        let old_set = navigation.set_id().unwrap();

        navigation.install(Folder::scan(&dir, by_name()).unwrap());
        navigation.select(0).unwrap();
        assert_eq!(
            apply_fs_change_for_set(
                &mut navigation,
                old_set,
                FsChange::Remove(dir.join("a.jpg")),
            ),
            None
        );
        assert_eq!(navigation.len(), 2);
        assert_eq!(navigation.current_path(), Some(dir.join("a.jpg").as_path()));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn stale_filesystem_queries_are_rejected_and_released() {
        let path = PathBuf::from("a.jpg");
        let other = PathBuf::from("b.jpg");
        let mut versions = FsQueryVersions::default();

        let (stale, stale_query) = versions.start(std::slice::from_ref(&path));
        let (current, _) = versions.start(std::slice::from_ref(&path));
        let (unrelated, _) = versions.start(std::slice::from_ref(&other));
        assert!(stale_query.is_cancelled());
        assert!(!versions.finish(std::slice::from_ref(&path), stale));
        assert!(versions.finish(std::slice::from_ref(&path), current));
        assert!(versions.finish(std::slice::from_ref(&other), unrelated));
        assert!(versions.paths.is_empty());

        let (stale, stale_query) = versions.start(std::slice::from_ref(&path));
        versions.supersede(std::slice::from_ref(&path));
        assert!(stale_query.is_cancelled());
        assert!(!versions.finish(std::slice::from_ref(&path), stale));
        assert!(versions.paths.is_empty());

        let (stale, stale_query) = versions.start(std::slice::from_ref(&path));
        versions.cancel_all();
        assert!(stale_query.is_cancelled());
        assert!(!versions.finish(std::slice::from_ref(&path), stale));
        assert!(versions.paths.is_empty());
    }

    #[test]
    fn gio_metadata_becomes_a_regular_file_snapshot() {
        let path = PathBuf::from("a.jpg");
        let info = gtk4::gio::FileInfo::new();
        info.set_file_type(gtk4::gio::FileType::Regular);
        info.set_attribute_uint64("time::modified", 42);
        info.set_attribute_uint32("time::modified-nsec", 123);
        assert_eq!(
            file_snapshot_from_info(path.clone(), &info),
            Some(FileSnapshot::new(
                path,
                std::time::SystemTime::UNIX_EPOCH + std::time::Duration::new(42, 123),
                SnapshotKind::Regular,
            ))
        );

        info.set_file_type(gtk4::gio::FileType::Directory);
        assert_eq!(
            file_snapshot_from_info(PathBuf::from("dir.jpg"), &info),
            Some(FileSnapshot::new(
                PathBuf::from("dir.jpg"),
                std::time::SystemTime::UNIX_EPOCH + std::time::Duration::new(42, 123),
                SnapshotKind::Other,
            ))
        );
    }
}
