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

use super::{App, Arrival, MediaState, reset_timer};

#[cfg(test)]
thread_local! {
    pub(super) static SNAPSHOT_TEST_GATE: std::cell::RefCell<Option<gio::Cancellable>> = const { std::cell::RefCell::new(None) };
}

const CONTENT_REFRESH_DELAY: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, PartialEq, Eq)]
enum FsPresentation {
    Unchanged,
    Show(Destination),
    Empty,
}

#[derive(Debug)]
enum FsChange {
    PendingRename {
        old: PathBuf,
        new: PathBuf,
    },
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
            E::Changed | E::ChangesDoneHint => {
                if let Some(path) = file.path() {
                    self.on_content_changed(set, &path);
                }
            }
            E::Created | E::MovedIn => {
                let Some(path) = file.path().filter(|path| config::is_supported(path)) else {
                    return;
                };
                self.on_content_changed(set, &path);
                self.query_fs_snapshot(set, file.clone(), event);
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
                self.fs_queries
                    .borrow_mut()
                    .supersede(&[old.clone(), new.clone()]);
                self.apply_fs_change(
                    set,
                    FsChange::PendingRename {
                        old,
                        new: new.clone(),
                    },
                    event,
                );
                if config::is_supported(&new) {
                    self.query_fs_snapshot(set, new_file, event);
                }
            }
            _ => {}
        }
    }

    /// Invalidate immediately, but wait for a short quiet period before
    /// decoding the current image. One timer coalesces Changed/ChangesDoneHint
    /// bursts; there is no per-path history or timer growth for neighbor edits.
    fn on_content_changed(self: &Rc<Self>, set: NavigationSetId, path: &Path) {
        if !self.invalidate_image(path) {
            return;
        }
        let path = path.to_path_buf();
        let weak = Rc::downgrade(self);
        reset_timer(&self.fs_refresh_timer, CONTENT_REFRESH_DELAY, move || {
            let Some(app) = weak.upgrade() else { return };
            let index = {
                let navigation = app.navigation.borrow();
                if app.shutting_down.get()
                    || navigation.set_id() != Some(set)
                    || navigation.current_path() != Some(path.as_path())
                {
                    return;
                }
                navigation.current_index()
            };
            if let Some(index) = index {
                app.show_index(index, Arrival::Direct);
            }
        });
    }

    /// Stop work for old contents without choosing a reload destination.
    /// Rename completion owns that choice; content edits reload the same path.
    fn invalidate_image(self: &Rc<Self>, path: &Path) -> bool {
        if !config::is_supported(path) || config::is_video(path) {
            return false;
        }
        self.cache.invalidate(path);
        self.decodes.borrow_mut().invalidate(path);
        let index = {
            let mut navigation = self.navigation.borrow_mut();
            if navigation.current_path() != Some(path) {
                return false;
            }
            navigation.supersede();
            navigation.current_index()
        };
        let Some(index) = index else { return false };
        // Invalidate animation/SVG presentation as well as first-frame work.
        // Keep the last texture visible while the replacement is prepared.
        self.fs_refresh_timer.cancel();
        self.stop_animation();
        self.svg_timer.cancel();
        if self.view.cancel_markup() {
            self.update_cursor();
        }
        *self.media.borrow_mut() = MediaState::Loading(path.to_path_buf());
        self.update_control_mode();
        self.schedule_decodes(index, None);
        true
    }

    fn query_fs_snapshot(
        self: &Rc<Self>,
        set: NavigationSetId,
        file: gio::File,
        event: gio::FileMonitorEvent,
    ) {
        let Some(snapshot_path) = file.path() else {
            return;
        };
        let paths = vec![snapshot_path.clone()];
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
                    let complete = move || {
                        let current = app.fs_queries.borrow_mut().finish(&paths, version);
                        let same_set = app.navigation.borrow().set_id() == Some(set);
                        let snapshot = match snapshot_from_query(snapshot_path.clone(), result) {
                            Ok(snapshot) => snapshot,
                            Err(error) => {
                                if !error.matches(gio::IOErrorEnum::Cancelled) {
                                    eprintln!(
                                        "open-mpv: cannot query changed file {}: {error}",
                                        snapshot_path.display()
                                    );
                                }
                                if current && same_set && !app.shutting_down.get() {
                                    let loading = match &*app.media.borrow() {
                                        MediaState::Loading(path) if paths.contains(path) => {
                                            Some(path.clone())
                                        }
                                        _ => None,
                                    };
                                    if let Some(path) = loading {
                                        app.show_error(
                                            &path,
                                            &format!("Could not read the changed file: {error}"),
                                        );
                                    }
                                }
                                return;
                            }
                        };
                        if current && same_set && !app.shutting_down.get() {
                            app.apply_fs_change(
                                set,
                                FsChange::Rename {
                                    old: snapshot_path.clone(),
                                    new: snapshot_path,
                                    snapshot,
                                },
                                event,
                            );
                        }
                    };
                    #[cfg(test)]
                    if let Some(gate) = SNAPSHOT_TEST_GATE.with(|gate| gate.borrow().clone()) {
                        glib::spawn_future_local(async move {
                            gate.future().await;
                            complete();
                        });
                        return;
                    }
                    complete();
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
            FsChange::Remove(path) => (path.as_path(), true),
            FsChange::Rename { old, .. } | FsChange::PendingRename { old, .. } => {
                (old.as_path(), false)
            }
        };
        let path = path.to_path_buf();
        // Invalidate both sides of a rename before a new destination can use
        // a cached result or join an in-flight decode of replaced contents.
        let invalidated = match &change {
            FsChange::Remove(path) => vec![path.clone()],
            FsChange::Rename { old, new, .. } | FsChange::PendingRename { old, new } => {
                vec![old.clone(), new.clone()]
            }
        };
        let pending_target = match &change {
            FsChange::PendingRename { new, .. } => Some(new.clone()),
            _ => None,
        };
        let (before_generation, presentation) = {
            let mut navigation = self.navigation.borrow_mut();
            let before = navigation.generation();
            let Some(presentation) = apply_fs_change_for_set(&mut navigation, set, change) else {
                return;
            };
            (before, presentation)
        };
        for path in invalidated {
            self.cache.invalidate(&path);
            self.decodes.borrow_mut().invalidate(&path);
        }
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
                if pending_target.as_deref() == Some(destination.path.as_path()) {
                    self.fs_refresh_timer.cancel();
                    self.stop_animation();
                    self.svg_timer.cancel();
                    self.stop_video();
                    if !self.invalidate_image(&destination.path) {
                        *self.media.borrow_mut() = MediaState::Loading(destination.path.clone());
                        self.update_control_mode();
                    }
                    self.set_current_name(Some(&destination.path));
                } else {
                    self.show_destination(destination, Arrival::Direct);
                }
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
        FsChange::PendingRename { old, new } => {
            rename_presentation(navigation.rename_pending(&old, &new))
        }
        FsChange::Remove(path) => match navigation.remove(&path) {
            RemovalOutcome::CurrentRemoved(Some(destination)) => FsPresentation::Show(destination),
            RemovalOutcome::CurrentRemoved(None) => FsPresentation::Empty,
            RemovalOutcome::NotFound | RemovalOutcome::CurrentPreserved => {
                FsPresentation::Unchanged
            }
        },
        FsChange::Rename { old, new, snapshot } => {
            rename_presentation(navigation.rename(&old, &new, snapshot))
        }
    }
}

fn rename_presentation(outcome: RenameOutcome) -> FsPresentation {
    match outcome {
        RenameOutcome::Renamed(destination) | RenameOutcome::Removed(Some(destination)) => {
            FsPresentation::Show(destination)
        }
        RenameOutcome::Removed(None) => FsPresentation::Empty,
        RenameOutcome::Preserved => FsPresentation::Unchanged,
    }
}

fn apply_fs_change_for_set(
    navigation: &mut Navigation,
    set: NavigationSetId,
    change: FsChange,
) -> Option<FsPresentation> {
    (navigation.set_id() == Some(set)).then(|| apply_fs_change(navigation, change))
}

/// Disappearance is an ordinary monitor race. Other failures must not be
/// applied as if metadata proved the file absent.
fn snapshot_from_query(
    path: PathBuf,
    result: Result<gio::FileInfo, glib::Error>,
) -> Result<Option<FileSnapshot>, glib::Error> {
    match result {
        Ok(info) => Ok(file_snapshot_from_info(path, &info)),
        Err(error) if error.matches(gio::IOErrorEnum::NotFound) => Ok(None),
        Err(error) => Err(error),
    }
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
    fn failed_metadata_queries_are_not_applied_as_absent_files() {
        let path = PathBuf::from("/tmp/media.png");
        assert!(
            snapshot_from_query(
                path.clone(),
                Err(glib::Error::new(gio::IOErrorEnum::NotFound, "gone"))
            )
            .unwrap()
            .is_none()
        );
        for kind in [
            gio::IOErrorEnum::Cancelled,
            gio::IOErrorEnum::PermissionDenied,
            gio::IOErrorEnum::Failed,
        ] {
            let error =
                snapshot_from_query(path.clone(), Err(glib::Error::new(kind, "original cause")))
                    .unwrap_err();
            assert!(error.matches(kind));
            assert_eq!(error.message(), "original cause");
        }
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
