//! Source-media writes (FR-5): move to trash, restore from trash, and
//! rotate-save. Restores use no-replace rename semantics, and every image
//! edit is staged in a same-directory temporary file before replacement.
//! Rotate-save captures source identity/version before editing and rejects
//! detected changes before replacement; the final check-to-rename race remains.

use std::collections::BTreeSet;
use std::fmt;
use std::fs::{File, FileTimes, Metadata};
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use gtk4::gio;
use gtk4::gio::prelude::*;
use gtk4::glib;

use gufo_common::orientation::Rotation;

#[derive(Debug)]
pub struct TrashError {
    path: PathBuf,
    source: glib::Error,
}

impl fmt::Display for TrashError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "could not move {} to trash: {}",
            self.path.display(),
            self.source
        )
    }
}

impl std::error::Error for TrashError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

#[derive(Debug)]
pub enum RestoreError {
    NotFound(PathBuf),
    DestinationExists(PathBuf),
    Move {
        path: PathBuf,
        source: std::io::Error,
    },
    Rollback {
        path: PathBuf,
        preserved: PathBuf,
        source: std::io::Error,
    },
}

impl fmt::Display for RestoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RestoreError::NotFound(path) => {
                write!(f, "{} is no longer in the trash", path.display())
            }
            RestoreError::DestinationExists(path) => write!(
                f,
                "could not restore {} because another file now exists there",
                path.display()
            ),
            RestoreError::Rollback {
                path,
                preserved,
                source,
            } => write!(
                f,
                "could not return {} to trash: {source}; the file is preserved at {}. Move it back manually",
                path.display(),
                preserved.display()
            ),
            RestoreError::Move { path, source } => {
                write!(f, "could not restore {}: {source}", path.display())
            }
        }
    }
}

impl std::error::Error for RestoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            RestoreError::NotFound(_) | RestoreError::DestinationExists(_) => None,
            RestoreError::Move { source, .. } | RestoreError::Rollback { source, .. } => {
                Some(source)
            }
        }
    }
}

#[derive(Debug)]
pub enum SaveRotationError {
    Editor {
        path: PathBuf,
        source: Box<glycin::ErrorCtx>,
    },
    Rotation {
        path: PathBuf,
        source: Box<glycin::ErrorCtx>,
    },
    LosslessUnavailable(PathBuf),
    SourceChanged(PathBuf),
    SparseWrite {
        path: PathBuf,
        source: glycin::Error,
    },
    ReadEdited(std::io::Error),
    AtomicWrite {
        path: PathBuf,
        source: std::io::Error,
    },
}

#[derive(Debug)]
pub enum SaveRotationOutcome {
    Saved,
    DurabilityUncertain(std::io::Error),
}

impl fmt::Display for SaveRotationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SaveRotationError::Editor { path, source } => {
                write!(f, "editor failed for {}: {source}", path.display())
            }
            SaveRotationError::Rotation { path, source } => {
                write!(f, "rotation failed for {}: {source}", path.display())
            }
            SaveRotationError::LosslessUnavailable(path) => write!(
                f,
                "could not rotate {} without re-encoding JPEG pixels",
                path.display()
            ),
            SaveRotationError::SourceChanged(path) => write!(
                f,
                "could not save {} because the source changed during saving; reopen the file and try again",
                path.display()
            ),
            SaveRotationError::SparseWrite { path, source } => {
                write!(f, "could not write {}: {source}", path.display())
            }
            SaveRotationError::AtomicWrite { path, source } => {
                write!(f, "could not write {}: {source}", path.display())
            }
            SaveRotationError::ReadEdited(source) => {
                write!(f, "could not read edited image data: {source}")
            }
        }
    }
}

impl std::error::Error for SaveRotationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            SaveRotationError::Editor { source, .. }
            | SaveRotationError::Rotation { source, .. } => Some(source.as_ref()),
            SaveRotationError::LosslessUnavailable(_) | SaveRotationError::SourceChanged(_) => None,
            SaveRotationError::SparseWrite { source, .. } => Some(source),
            SaveRotationError::ReadEdited(source)
            | SaveRotationError::AtomicWrite { source, .. } => Some(source),
        }
    }
}

/// MIME types the sandboxed editors can rewrite (queried once).
pub async fn editable_mime_types() -> BTreeSet<String> {
    glycin::EditableImage::supported_formats()
        .await
        .keys()
        .map(|m| m.to_string())
        .collect()
}

/// The exact payload moved by one Trash operation. Holding its descriptor keeps
/// its inode from being reused while Undo is offered, including after emptying Trash.
#[derive(Debug, Clone)]
pub struct TrashedFile {
    original: PathBuf,
    file: PathBuf,
    info: PathBuf,
    source: std::sync::Arc<File>,
    info_source: std::sync::Arc<File>,
}

impl PartialEq for TrashedFile {
    fn eq(&self, other: &Self) -> bool {
        self.original == other.original
            && self.file == other.file
            && self.info == other.info
            && std::sync::Arc::ptr_eq(&self.source, &other.source)
    }
}

impl Eq for TrashedFile {}

impl TrashedFile {
    #[cfg(test)]
    pub(crate) fn test_receipt(original: &Path) -> Self {
        Self {
            original: original.to_path_buf(),
            file: PathBuf::new(),
            info: PathBuf::new(),
            source: std::sync::Arc::new(File::open("/dev/null").unwrap()),
            info_source: std::sync::Arc::new(File::open("/dev/null").unwrap()),
        }
    }
}

#[cfg(test)]
thread_local! {
    pub(crate) static TRASH_FAILURE_TEST: std::cell::RefCell<Option<gio::Cancellable>> = const { std::cell::RefCell::new(None) };
}

/// GIO moves within a filesystem, preserving identity. A successful move whose
/// receipt cannot be identified still succeeds, but must not offer ambiguous Undo.
pub async fn trash(path: &Path) -> Result<Option<TrashedFile>, TrashError> {
    #[cfg(test)]
    if let Some(gate) = TRASH_FAILURE_TEST.with(|state| state.borrow().clone()) {
        gate.future().await;
        return Err(TrashError {
            path: path.to_path_buf(),
            source: glib::Error::new(gio::IOErrorEnum::PermissionDenied, "injected trash failure"),
        });
    }
    let path = path.to_path_buf();
    let worker_path = path.clone();
    gio::spawn_blocking(move || trash_sync(&worker_path))
        .await
        .unwrap_or_else(|_| {
            Err(TrashError {
                path,
                source: glib::Error::new(gio::IOErrorEnum::Failed, "trash worker failed"),
            })
        })
}

fn trash_sync(path: &Path) -> Result<Option<TrashedFile>, TrashError> {
    let capture = || -> std::io::Result<File> {
        let fd = rustix::fs::open(
            path,
            rustix::fs::OFlags::PATH | rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )?;
        Ok(File::from(fd))
    };
    let source = capture().map_err(|error| TrashError {
        path: path.to_path_buf(),
        source: glib::Error::new(gio::IOErrorEnum::Failed, &error.to_string()),
    })?;
    let dirs = trash_dirs_for(path);
    let before: BTreeSet<_> = find_trashed_files(path, &dirs).into_iter().collect();
    gio::File::for_path(path)
        .trash(gio::Cancellable::NONE)
        .map_err(|source| TrashError {
            path: path.to_path_buf(),
            source,
        })?;
    let selected = identify_trashed_file(&source, &before, find_trashed_files(path, &dirs));
    let receipt = selected.and_then(|(file, info)| {
        let info_source = File::open(&info).ok()?;
        Some(TrashedFile {
            original: path.to_path_buf(),
            file,
            info,
            source: std::sync::Arc::new(source),
            info_source: std::sync::Arc::new(info_source),
        })
    });
    if receipt.is_none() {
        eprintln!(
            "open-mpv: moved {} to trash, but could not identify its exact Undo entry",
            path.display()
        );
    }
    Ok(receipt)
}

fn identify_trashed_file(
    source: &File,
    before: &BTreeSet<(PathBuf, PathBuf)>,
    candidates: Vec<(PathBuf, PathBuf)>,
) -> Option<(PathBuf, PathBuf)> {
    let mut matches = candidates
        .into_iter()
        .filter(|entry| !before.contains(entry) && same_file(source, &entry.0));
    let selected = matches.next()?;
    matches.next().is_none().then_some(selected)
}

fn same_file(source: &File, path: &Path) -> bool {
    match (source.metadata(), std::fs::symlink_metadata(path)) {
        (Ok(source), Ok(candidate)) => {
            source.dev() == candidate.dev() && source.ino() == candidate.ino()
        }
        _ => false,
    }
}

#[cfg(test)]
thread_local! {
    static RESTORE_REPLACEMENT_TEST: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
    static RESTORE_ROLLBACK_CONFLICT_TEST: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn rename_no_replace(from: &Path, to: &Path) -> std::io::Result<()> {
    rustix::fs::renameat_with(
        rustix::fs::CWD,
        from,
        rustix::fs::CWD,
        to,
        rustix::fs::RenameFlags::NOREPLACE,
    )
    .map_err(std::io::Error::from)
}

/// Restore only the retained entry. Claim its pathname into a private directory
/// on the trash filesystem, then verify the claimed inode before publishing it.
pub fn restore(receipt: &TrashedFile) -> Result<(), RestoreError> {
    let orig = &receipt.original;
    if !same_file(&receipt.source, &receipt.file) {
        return Err(RestoreError::NotFound(orig.clone()));
    }
    let staging = tempfile::Builder::new()
        .prefix(".open-mpv-restore-")
        .tempdir_in(receipt.file.parent().unwrap_or(Path::new("/")))
        .map_err(|source| RestoreError::Move {
            path: orig.clone(),
            source,
        })?;
    let claimed = staging.path().join("payload");
    #[cfg(test)]
    RESTORE_REPLACEMENT_TEST.with(|replacement| {
        if let Some(replacement) = replacement.borrow_mut().take() {
            std::fs::rename(replacement, &receipt.file).unwrap();
        }
    });
    rename_no_replace(&receipt.file, &claimed).map_err(|source| RestoreError::Move {
        path: orig.clone(),
        source,
    })?;
    let result = if !same_file(&receipt.source, &claimed) {
        Err(RestoreError::NotFound(orig.clone()))
    } else {
        rename_no_replace(&claimed, orig).map_err(|source| {
            if source.kind() == std::io::ErrorKind::AlreadyExists {
                RestoreError::DestinationExists(orig.clone())
            } else {
                RestoreError::Move {
                    path: orig.clone(),
                    source,
                }
            }
        })
    };
    if let Err(error) = result {
        #[cfg(test)]
        RESTORE_ROLLBACK_CONFLICT_TEST.with(|conflict| {
            if conflict.replace(false) {
                std::fs::write(&receipt.file, b"concurrent entry").unwrap();
            }
        });
        if let Err(source) = rename_no_replace(&claimed, &receipt.file) {
            // Never let TempDir cleanup erase a claimed payload on rollback failure.
            let _ = staging.keep();
            return Err(RestoreError::Rollback {
                path: orig.clone(),
                preserved: claimed,
                source,
            });
        }
        return Err(error);
    }
    // Claim metadata before deleting it as well. Another process may have reused
    // the public trash name while the original payload was being restored.
    let info = staging.path().join("entry.trashinfo");
    if rename_no_replace(&receipt.info, &info).is_ok() {
        if same_file(&receipt.info_source, &info) {
            // Only the exact metadata we retained is eligible for cleanup.
            if let Err(error) = std::fs::remove_file(&info) {
                eprintln!(
                    "open-mpv: restored {}, but could not remove trash metadata: {error}",
                    orig.display()
                );
                let _ = staging.keep();
            }
        } else if let Err(error) = rename_no_replace(&info, &receipt.info) {
            eprintln!(
                "open-mpv: restored {}, but could not return replaced trash metadata: {error}; preserved at {}",
                orig.display(),
                info.display()
            );
            let _ = staging.keep();
        }
    }
    Ok(())
}

/// Relative metadata paths use the mount root for either mount-local layout,
/// or the data directory for the home trash. Layout depth is not a path root.
struct TrashDirectory {
    path: PathBuf,
    relative_root: PathBuf,
}

impl TrashDirectory {
    fn mount_local(top: &Path, uid: u32) -> [Self; 2] {
        [
            top.join(".Trash").join(uid.to_string()),
            top.join(format!(".Trash-{uid}")),
        ]
        .map(|path| Self {
            path,
            relative_root: top.to_path_buf(),
        })
    }
}

fn find_trashed_files(orig: &Path, trash_dirs: &[TrashDirectory]) -> Vec<(PathBuf, PathBuf)> {
    let mut matches = Vec::new();
    for trash_dir in trash_dirs {
        let info_dir = trash_dir.path.join("info");
        let Ok(entries) = std::fs::read_dir(&info_dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let info_path = entry.path();
            if info_path.extension().and_then(|e| e.to_str()) != Some("trashinfo") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&info_path) else {
                continue;
            };
            let mut path = None;
            for line in text.lines() {
                if let Some(v) = line.strip_prefix("Path=") {
                    path = Some(percent_decode(v));
                }
            }
            let Some(path) = path else { continue };
            let abs = if path.is_absolute() {
                path
            } else {
                trash_dir.relative_root.join(path)
            };
            if abs != orig {
                continue;
            }
            let Some(stem) = info_path.file_stem() else {
                continue;
            };
            let file = trash_dir.path.join("files").join(stem);
            matches.push((file, info_path));
        }
    }
    matches
}

/// Trash directories that could hold `orig` per the freedesktop trash
/// spec: the home trash, and the `.Trash`/`.Trash-$uid` dirs at the top
/// of the file's mount point.
fn trash_dirs_for(orig: &Path) -> Vec<TrashDirectory> {
    let data_dir = glib::user_data_dir();
    let mut dirs = vec![TrashDirectory {
        path: data_dir.join("Trash"),
        relative_root: data_dir,
    }];
    if let Some(top) = mount_topdir(orig) {
        // Effective uid without adding a libc dependency (Linux).
        if let Ok(meta) = std::fs::metadata("/proc/self") {
            use std::os::unix::fs::MetadataExt;
            let uid = meta.uid();
            dirs.extend(TrashDirectory::mount_local(&top, uid));
        }
    }
    dirs
}

/// Highest ancestor of `path` on the same device — the mount top.
fn mount_topdir(path: &Path) -> Option<PathBuf> {
    use std::os::unix::fs::MetadataExt;
    let start = path.parent()?;
    let dev = std::fs::metadata(start).ok()?.dev();
    let mut top = start.to_path_buf();
    while let Some(parent) = top.parent() {
        match std::fs::metadata(parent) {
            Ok(m) if m.dev() == dev => top = parent.to_path_buf(),
            _ => break,
        }
    }
    Some(top)
}

/// Decode %XX escapes in trashinfo Path values.
fn percent_decode(s: &str) -> PathBuf {
    use std::os::unix::ffi::OsStringExt;
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(high), Some(low)) = (hex_value(bytes[i + 1]), hex_value(bytes[i + 2]))
        {
            out.push((high << 4) | low);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    PathBuf::from(std::ffi::OsString::from_vec(out))
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Persist a clockwise view rotation (in quarter turns) to disk via the
/// sandboxed editor. JPEG rotations are accepted only when Glycin guarantees
/// no pixel re-encode; every edit is staged and atomically installed
/// (FR-5.4/5.5).
pub async fn save_rotation(
    path: &Path,
    mime: &str,
    cw_quarter_turns: u8,
) -> Result<SaveRotationOutcome, SaveRotationError> {
    // glycin rotations are counter-clockwise.
    let rotation = match cw_quarter_turns % 4 {
        1 => Rotation::_270,
        2 => Rotation::_180,
        3 => Rotation::_90,
        _ => return Ok(SaveRotationOutcome::Saved),
    };
    // Capture identity and version before the editor can read asynchronously.
    let stage_path = path.to_path_buf();
    let staged = spawn_io(move || AtomicReplacement::empty(&stage_path))
        .await
        .map_err(|source| save_write_error(path, source))?;
    let file = gio::File::for_path(path);
    let editable =
        glycin::Editor::new(file)
            .edit()
            .await
            .map_err(|source| SaveRotationError::Editor {
                path: path.to_path_buf(),
                source: Box::new(source),
            })?;
    let ops = glycin::Operations::new(vec![glycin::Operation::Rotate(rotation)]);

    // `SparseEdit` does not expose Glycin's lossless flag for a complete
    // fallback. Ask for a complete JPEG edit directly so the contract can be
    // enforced instead of silently accepting a pixel re-encode.
    if mime == "image/jpeg" {
        let edit =
            editable
                .apply_complete(&ops)
                .await
                .map_err(|source| SaveRotationError::Rotation {
                    path: path.to_path_buf(),
                    source: Box::new(source),
                })?;
        if !edit.is_lossless() {
            return Err(SaveRotationError::LosslessUnavailable(path.to_path_buf()));
        }
        let data = edit.data();
        let bytes = spawn_io(move || data.get_full())
            .await
            .map_err(SaveRotationError::ReadEdited)?;
        return atomic_write_async(staged, bytes).await;
    }

    let edit = editable
        .apply_sparse(&ops)
        .await
        .map_err(|source| SaveRotationError::Rotation {
            path: path.to_path_buf(),
            source: Box::new(source),
        })?;
    match edit {
        glycin::SparseEdit::Sparse(_) => {
            let staged = spawn_io(move || staged.copy_source())
                .await
                .map_err(|source| save_write_error(path, source))?;
            let outcome = edit
                .apply_to(gio::File::for_path(staged.path()))
                .await
                .map_err(|source| SaveRotationError::SparseWrite {
                    path: path.to_path_buf(),
                    source,
                })?;
            debug_assert_eq!(outcome, glycin::EditOutcome::Changed);
            spawn_io(move || staged.commit())
                .await
                .map_err(|source| save_write_error(path, source))
        }
        glycin::SparseEdit::Complete(data) => {
            let bytes = spawn_io(move || data.get_full())
                .await
                .map_err(SaveRotationError::ReadEdited)?;
            atomic_write_async(staged, bytes).await
        }
    }
}

async fn atomic_write_async(
    mut staged: AtomicReplacement,
    bytes: Vec<u8>,
) -> Result<SaveRotationOutcome, SaveRotationError> {
    let path = staged.target.clone();
    spawn_io(move || {
        staged.file_mut().write_all(&bytes)?;
        staged.commit()
    })
    .await
    .map_err(|source| save_write_error(&path, source))
}

fn save_write_error(path: &Path, source: std::io::Error) -> SaveRotationError {
    if source
        .get_ref()
        .is_some_and(|error| error.is::<SourceChanged>())
    {
        SaveRotationError::SourceChanged(path.to_path_buf())
    } else {
        SaveRotationError::AtomicWrite {
            path: path.to_path_buf(),
            source,
        }
    }
}

#[derive(Debug)]
struct SourceChanged;

impl fmt::Display for SourceChanged {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("source changed during saving")
    }
}

impl std::error::Error for SourceChanged {}

async fn spawn_io<T: Send + 'static>(
    operation: impl FnOnce() -> std::io::Result<T> + Send + 'static,
) -> std::io::Result<T> {
    gio::spawn_blocking(operation)
        .await
        .map_err(|_| std::io::Error::other("filesystem worker failed"))?
}

/// Write through an exclusively created same-directory file, then fsync and
/// rename it over the destination (FR-5.5, NFR-3.1).
#[cfg(test)]
fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<SaveRotationOutcome> {
    let mut staged = AtomicReplacement::empty(path)?;
    staged.file_mut().write_all(bytes)?;
    staged.commit()
}

struct AtomicReplacement {
    temp: tempfile::NamedTempFile,
    target: PathBuf,
    source: File,
    metadata: Metadata,
    entry_metadata: Metadata,
}

impl AtomicReplacement {
    fn empty(target: &Path) -> std::io::Result<Self> {
        let dir = target.parent().ok_or(std::io::ErrorKind::InvalidInput)?;
        let entry_metadata = std::fs::symlink_metadata(target)?;
        let source = File::open(target)?;
        let metadata = source.metadata()?;
        if !metadata.is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "rotate-save target is not a regular file",
            ));
        }
        let prefix = format!(
            ".{}.open-mpv-",
            target.file_name().unwrap_or_default().to_string_lossy()
        );
        let temp = tempfile::Builder::new().prefix(&prefix).tempfile_in(dir)?;
        let staged = Self {
            temp,
            target: target.to_path_buf(),
            source,
            metadata,
            entry_metadata,
        };
        staged.check_source()?;
        Ok(staged)
    }

    fn copy_source(mut self) -> std::io::Result<Self> {
        self.check_source()?;
        let mut source = &self.source;
        std::io::copy(&mut source, self.temp.as_file_mut())?;
        self.check_source()?;
        Ok(self)
    }

    fn check_source(&self) -> std::io::Result<()> {
        let entry = match std::fs::symlink_metadata(&self.target) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(std::io::Error::other(SourceChanged));
            }
            Err(error) => return Err(error),
        };
        if !same_source_version(&self.entry_metadata, &entry)
            || !same_source_version(&self.metadata, &self.source.metadata()?)
            || !same_source_version(&self.metadata, &std::fs::metadata(&self.target)?)
        {
            return Err(std::io::Error::other(SourceChanged));
        }
        Ok(())
    }

    fn path(&self) -> &Path {
        self.temp.path()
    }

    fn file_mut(&mut self) -> &mut File {
        self.temp.as_file_mut()
    }

    fn commit(self) -> std::io::Result<SaveRotationOutcome> {
        let owner = rustix::fs::Uid::from_raw(self.metadata.uid());
        let group = rustix::fs::Gid::from_raw(self.metadata.gid());
        rustix::fs::fchown(self.temp.as_file(), Some(owner), Some(group))?;
        self.temp
            .as_file()
            .set_permissions(self.metadata.permissions())?;
        copy_user_xattrs(&self.source, self.temp.as_file())?;
        if let Ok(accessed) = self.metadata.accessed() {
            self.temp
                .as_file()
                .set_times(FileTimes::new().set_accessed(accessed))?;
        }
        self.temp.as_file().sync_all()?;

        let dir = self
            .target
            .parent()
            .ok_or(std::io::ErrorKind::InvalidInput)?;
        let dir = File::open(dir)?;
        // This metadata check detects edits and replacements during staging.
        // It cannot close the final check-to-rename race with external writers.
        self.check_source()?;
        self.temp
            .persist(&self.target)
            .map_err(|error| error.error)?;
        Ok(saved_outcome(dir.sync_all()))
    }
}

/// Ignore atime: reading the image can update it without changing the source.
fn same_source_version(before: &Metadata, after: &Metadata) -> bool {
    before.dev() == after.dev()
        && before.ino() == after.ino()
        && before.len() == after.len()
        && before.mtime() == after.mtime()
        && before.mtime_nsec() == after.mtime_nsec()
        && before.ctime() == after.ctime()
        && before.ctime_nsec() == after.ctime_nsec()
}

fn saved_outcome(directory_sync: std::io::Result<()>) -> SaveRotationOutcome {
    match directory_sync {
        Ok(()) => SaveRotationOutcome::Saved,
        Err(error) => SaveRotationOutcome::DurabilityUncertain(error),
    }
}

fn copy_user_xattrs(source: &File, destination: &File) -> std::io::Result<()> {
    let mut empty = [0_u8; 0];
    let names_len = rustix::fs::flistxattr(source, &mut empty)?;
    let mut names = vec![0; names_len];
    let names_len = rustix::fs::flistxattr(source, &mut names)?;
    names.truncate(names_len);

    for name in names
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty())
    {
        if !name.starts_with(b"user.") {
            continue;
        }
        let name = std::ffi::OsStr::from_bytes(name);
        let mut empty = [0_u8; 0];
        let value_len = rustix::fs::fgetxattr(source, name, &mut empty)?;
        let mut value = vec![0; value_len];
        let value_len = rustix::fs::fgetxattr(source, name, &mut value)?;
        value.truncate(value_len);
        rustix::fs::fsetxattr(destination, name, &value, rustix::fs::XattrFlags::empty())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::sync::Mutex;
    use std::time::SystemTime;

    /// gio futures use the thread-default main context; serialize the
    /// async tests and give each its own context.
    static ASYNC_LOCK: Mutex<()> = Mutex::new(());

    fn block_on<F: std::future::Future>(fut: F) -> F::Output {
        let ctx = glib::MainContext::new();
        let _acquired = ctx.acquire().unwrap();
        ctx.with_thread_default(|| ctx.block_on(fut)).unwrap()
    }

    /// Temp dir on the home filesystem — the freedesktop trash does not
    /// accept files from tmpfs without a mount-level trash dir.
    fn home_tempdir(name: &str) -> PathBuf {
        let dir = glib::home_dir().join(format!(
            ".cache/open-mpv-test-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn trash_and_restore_roundtrip() {
        let _guard = ASYNC_LOCK.lock().unwrap();
        let dir = home_tempdir("trash");
        let file = dir.join("victim.txt");
        std::fs::write(&file, b"payload").unwrap();

        let receipt = block_on(trash(&file)).unwrap().unwrap();
        assert!(!file.exists(), "file must be gone after trash");

        restore(&receipt).unwrap();
        assert!(file.exists(), "file must be back after restore");
        assert_eq!(std::fs::read(&file).unwrap(), b"payload");

        // Restoring again must fail cleanly — nothing left in trash.
        assert!(matches!(
            restore(&receipt),
            Err(RestoreError::NotFound(path)) if path == file
        ));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn restore_refuses_to_replace_a_recreated_destination() {
        let _guard = ASYNC_LOCK.lock().unwrap();
        let dir = home_tempdir("trash-conflict");
        let file = dir.join("victim.txt");
        std::fs::write(&file, b"trashed payload").unwrap();

        let receipt = block_on(trash(&file)).unwrap().unwrap();
        std::fs::write(&file, b"replacement").unwrap();

        assert!(matches!(
            restore(&receipt),
            Err(RestoreError::DestinationExists(path)) if path == file
        ));
        assert_eq!(std::fs::read(&file).unwrap(), b"replacement");

        // The failed restore retained both the payload and its trash metadata,
        // so removing the conflict makes the same Undo recoverable.
        std::fs::remove_file(&file).unwrap();
        restore(&receipt).unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), b"trashed payload");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn restore_resolves_each_trash_layout_without_replacing_the_destination() {
        let dir = tempfile::tempdir().unwrap();
        let top = dir.path().join("mount");
        let data_dir = dir.path().join("data");
        let mut trash_dirs = Vec::from(TrashDirectory::mount_local(&top, 1000));
        trash_dirs.push(TrashDirectory {
            path: data_dir.join("Trash"),
            relative_root: data_dir,
        });

        for trash in trash_dirs {
            std::fs::create_dir_all(trash.path.join("info")).unwrap();
            std::fs::create_dir_all(trash.path.join("files")).unwrap();
            for (relative, encoded) in [
                ("photo.jpg", "photo.jpg"),
                ("photos/photo one.jpg", "photos/photo%20one.jpg"),
            ] {
                let orig = trash.relative_root.join(relative);
                std::fs::create_dir_all(orig.parent().unwrap()).unwrap();
                for metadata_path in [
                    encoded.to_owned(),
                    orig.to_str().unwrap().replace(' ', "%20"),
                ] {
                    let payload = trash.path.join("files/unique-name.jpg");
                    let info = trash.path.join("info/unique-name.jpg.trashinfo");
                    let metadata = format!(
                        "[Trash Info]\nPath={metadata_path}\nDeletionDate=2026-01-01T00:00:00\n"
                    );
                    std::fs::write(&payload, b"trashed payload").unwrap();
                    std::fs::write(&info, &metadata).unwrap();
                    let candidates = std::slice::from_ref(&trash);
                    assert_eq!(
                        find_trashed_files(&orig, candidates),
                        vec![(payload.clone(), info.clone())]
                    );
                    assert!(
                        find_trashed_files(&dir.path().join("unrelated/photo.jpg"), candidates)
                            .is_empty()
                    );

                    let receipt = TrashedFile {
                        original: orig.clone(),
                        file: payload.clone(),
                        info: info.clone(),
                        source: std::sync::Arc::new(File::open(&payload).unwrap()),
                        info_source: std::sync::Arc::new(File::open(&info).unwrap()),
                    };
                    std::fs::write(&orig, b"replacement").unwrap();
                    assert!(matches!(
                        restore(&receipt),
                        Err(RestoreError::DestinationExists(path)) if path == orig
                    ));
                    assert_eq!(std::fs::read(&orig).unwrap(), b"replacement");
                    assert_eq!(std::fs::read(&payload).unwrap(), b"trashed payload");
                    assert_eq!(std::fs::read_to_string(&info).unwrap(), metadata);

                    std::fs::remove_file(&orig).unwrap();
                    restore(&receipt).unwrap();
                    assert_eq!(std::fs::read(&orig).unwrap(), b"trashed payload");
                    assert!(!payload.exists());
                    assert!(!info.exists());
                    assert!(matches!(
                        restore(&receipt),
                        Err(RestoreError::NotFound(path)) if path == orig
                    ));
                    std::fs::remove_file(&orig).unwrap();
                }
            }
        }
    }

    #[test]
    fn undo_restores_exact_version_with_equal_deletion_dates() {
        for reverse in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let mut dirs = TrashDirectory::mount_local(dir.path(), 1000);
            if reverse {
                dirs.reverse();
            }
            let orig = dir.path().join("photo.jpg");
            for (index, trash) in dirs.iter().enumerate() {
                std::fs::create_dir_all(trash.path.join("info")).unwrap();
                std::fs::create_dir_all(trash.path.join("files")).unwrap();
                std::fs::write(trash.path.join("files/photo.jpg"), [index as u8]).unwrap();
                std::fs::write(
                    trash.path.join("info/photo.jpg.trashinfo"),
                    "[Trash Info]\nPath=photo.jpg\nDeletionDate=2026-01-01T00:00:00\n",
                )
                .unwrap();
            }
            let source = File::open(dirs[1].path.join("files/photo.jpg")).unwrap();
            let mut candidates = find_trashed_files(&orig, &dirs);
            if reverse {
                candidates.reverse();
            }
            let (file, info) =
                identify_trashed_file(&source, &BTreeSet::new(), candidates).unwrap();
            let receipt = TrashedFile {
                original: orig.clone(),
                file,
                info_source: std::sync::Arc::new(File::open(&info).unwrap()),
                info,
                source: std::sync::Arc::new(source),
            };
            restore(&receipt).unwrap();
            assert_eq!(
                std::fs::read(&orig).unwrap(),
                [1],
                "enumeration order: {reverse}"
            );
        }
    }

    #[test]
    fn replacement_between_restore_check_and_claim_is_never_published() {
        for conflict in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let original = dir.path().join("original");
            let file = dir.path().join("payload");
            let info = dir.path().join("payload.trashinfo");
            let replacement = dir.path().join("replacement");
            std::fs::write(&file, b"exact version").unwrap();
            std::fs::write(&info, b"original metadata").unwrap();
            std::fs::write(&replacement, b"replaced entry").unwrap();
            let receipt = TrashedFile {
                original: original.clone(),
                file: file.clone(),
                info: info.clone(),
                source: std::sync::Arc::new(File::open(&file).unwrap()),
                info_source: std::sync::Arc::new(File::open(&info).unwrap()),
            };
            RESTORE_REPLACEMENT_TEST.with(|state| *state.borrow_mut() = Some(replacement));
            RESTORE_ROLLBACK_CONFLICT_TEST.with(|state| state.set(conflict));
            let error = restore(&receipt).unwrap_err();
            assert!(
                !original.exists(),
                "a replacement must never reach the destination"
            );
            assert_eq!(std::fs::read(&info).unwrap(), b"original metadata");
            if conflict {
                let RestoreError::Rollback { preserved, .. } = error else {
                    panic!("must preserve claimed payload on conflict")
                };
                assert_eq!(std::fs::read(&file).unwrap(), b"concurrent entry");
                assert_eq!(std::fs::read(preserved).unwrap(), b"replaced entry");
            } else {
                assert!(matches!(error, RestoreError::NotFound(_)));
                assert_eq!(std::fs::read(&file).unwrap(), b"replaced entry");
                assert_eq!(
                    std::fs::read_dir(dir.path()).unwrap().count(),
                    2,
                    "private staging must be cleaned up"
                );
            }
        }
    }

    #[test]
    fn missing_or_replaced_receipt_never_restores_another_version() {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("original");
        let file = dir.path().join("payload");
        std::fs::write(&file, b"original payload").unwrap();
        let receipt = TrashedFile {
            original: original.clone(),
            file: file.clone(),
            info: dir.path().join("info"),
            source: std::sync::Arc::new(File::open(&file).unwrap()),
            info_source: std::sync::Arc::new(File::open("/dev/null").unwrap()),
        };
        std::fs::remove_file(&file).unwrap();
        assert!(matches!(restore(&receipt), Err(RestoreError::NotFound(_))));
        std::fs::write(&file, b"replacement").unwrap();
        assert!(matches!(restore(&receipt), Err(RestoreError::NotFound(_))));
        assert!(!original.exists());
        assert_eq!(std::fs::read(file).unwrap(), b"replacement");
    }

    #[test]
    fn restore_preserves_replaced_trash_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("original");
        let file = dir.path().join("payload");
        let info = dir.path().join("entry.trashinfo");
        std::fs::write(&file, b"exact version").unwrap();
        std::fs::write(&info, b"original metadata").unwrap();
        let receipt = TrashedFile {
            original: original.clone(),
            file: file.clone(),
            info: info.clone(),
            source: std::sync::Arc::new(File::open(&file).unwrap()),
            info_source: std::sync::Arc::new(File::open(&info).unwrap()),
        };
        std::fs::remove_file(&info).unwrap();
        std::fs::write(&info, b"replacement metadata").unwrap();
        restore(&receipt).unwrap();
        assert_eq!(std::fs::read(original).unwrap(), b"exact version");
        assert_eq!(std::fs::read(info).unwrap(), b"replacement metadata");
        assert!(!file.exists());
    }

    #[test]
    fn trash_receipt_excludes_existing_hardlinks_and_rejects_ambiguity() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("source");
        std::fs::write(&path, b"payload").unwrap();
        let source = File::open(&path).unwrap();
        let old = (dir.path().join("old"), dir.path().join("old.info"));
        let new = (dir.path().join("new"), dir.path().join("new.info"));
        std::fs::hard_link(&path, &old.0).unwrap();
        std::fs::hard_link(&path, &new.0).unwrap();
        for candidates in [
            vec![old.clone(), new.clone()],
            vec![new.clone(), old.clone()],
        ] {
            assert_eq!(
                identify_trashed_file(&source, &BTreeSet::from([old.clone()]), candidates.clone()),
                Some(new.clone())
            );
            assert_eq!(
                identify_trashed_file(&source, &BTreeSet::new(), candidates),
                None
            );
        }
    }

    #[test]
    fn successive_real_trash_operations_keep_independent_receipts() {
        let _guard = ASYNC_LOCK.lock().unwrap();
        let dir = home_tempdir("trash-versions");
        let path = dir.join("same.txt");
        std::fs::write(&path, b"older").unwrap();
        let older = block_on(trash(&path)).unwrap().unwrap();
        std::fs::write(&path, b"newer").unwrap();
        let newer = block_on(trash(&path)).unwrap().unwrap();
        restore(&newer).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"newer");
        std::fs::remove_file(&path).unwrap();
        restore(&older).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"older");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn percent_decode_handles_untrusted_bytes_without_panicking() {
        assert_eq!(
            percent_decode("photo%20one%2Ejpg").as_os_str().as_bytes(),
            b"photo one.jpg"
        );
        assert_eq!(
            percent_decode("non-utf8-%FF").as_os_str().as_bytes(),
            b"non-utf8-\xff"
        );

        for malformed in ["%", "%A", "%GG", "%é", "é%", "é%A"] {
            assert_eq!(
                percent_decode(malformed).as_os_str().as_bytes(),
                malformed.as_bytes()
            );
        }
    }

    #[test]
    fn malformed_trashinfo_does_not_hide_a_valid_entry() {
        let dir = tempfile::tempdir().unwrap();
        let trash = dir.path().join("Trash");
        std::fs::create_dir_all(trash.join("info")).unwrap();
        std::fs::create_dir_all(trash.join("files")).unwrap();
        let orig = dir.path().join("original.jpg");

        std::fs::write(
            trash.join("info/bad.trashinfo"),
            "[Trash Info]\nPath=%é\nDeletionDate=2026-01-02T00:00:00\n",
        )
        .unwrap();
        std::fs::write(
            trash.join("info/good.trashinfo"),
            format!(
                "[Trash Info]\nPath={}\nDeletionDate=2026-01-01T00:00:00\n",
                orig.display()
            ),
        )
        .unwrap();
        std::fs::write(trash.join("files/good"), b"payload").unwrap();

        let selected = find_trashed_files(
            &orig,
            &[TrashDirectory {
                path: trash.clone(),
                relative_root: dir.path().to_path_buf(),
            }],
        );
        assert_eq!(
            selected,
            vec![(trash.join("files/good"), trash.join("info/good.trashinfo"))]
        );
    }

    #[test]
    fn rotate_save_jpeg_90_cw() {
        let _guard = ASYNC_LOCK.lock().unwrap();
        let dir = home_tempdir("rotate");
        let file = dir.join("photo.jpg");
        // 40x20 so a 90° rotation is observable in the dimensions.
        let magick = std::process::Command::new("magick")
            .args(["-size", "40x20", "xc:red", file.to_str().unwrap()])
            .status();
        if !magick.map(|s| s.success()).unwrap_or(false) {
            eprintln!("skipping: ImageMagick unavailable to generate fixture");
            return;
        }
        // Add a minimal EXIF orientation entry. Glycin can then rotate the
        // JPEG losslessly by changing that metadata instead of pixel data.
        let jpeg = std::fs::read(&file).unwrap();
        assert_eq!(&jpeg[..2], b"\xff\xd8");
        let exif = [
            0xff, 0xe1, 0x00, 0x22, b'E', b'x', b'i', b'f', 0, 0, b'I', b'I', 0x2a, 0, 8, 0, 0, 0,
            1, 0, 0x12, 1, 3, 0, 1, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0,
        ];
        let mut with_exif = Vec::with_capacity(jpeg.len() + exif.len());
        with_exif.extend_from_slice(&jpeg[..2]);
        with_exif.extend_from_slice(&exif);
        with_exif.extend_from_slice(&jpeg[2..]);
        std::fs::write(&file, with_exif).unwrap();
        let before = std::fs::read(&file).unwrap();

        block_on(save_rotation(&file, "image/jpeg", 1)).unwrap();

        let after = std::fs::read(&file).unwrap();
        assert_eq!(
            jpeg_scan_payload(&after),
            jpeg_scan_payload(&before),
            "JPEG compressed pixel data must not be re-encoded"
        );

        // Successful JPEG saves are guaranteed lossless. The displayed result
        // must be 90° CW: 20x40 after auto-orient, without re-encoding pixels.
        let out = std::process::Command::new("magick")
            .args([
                file.to_str().unwrap(),
                "-auto-orient",
                "-format",
                "%w %h",
                "info:",
            ])
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            "20 40",
            "saved file must display rotated 90 degrees clockwise"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn jpeg_scan_payload(bytes: &[u8]) -> &[u8] {
        let marker = bytes
            .windows(2)
            .position(|window| window == b"\xff\xda")
            .expect("JPEG must contain a start-of-scan marker");
        let segment_len = usize::from(u16::from_be_bytes([bytes[marker + 2], bytes[marker + 3]]));
        &bytes[marker + 2 + segment_len..]
    }

    #[test]
    fn staged_save_rejects_external_changes() {
        let _guard = ASYNC_LOCK.lock().unwrap();
        for replacement in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let target = dir.path().join("img.png");
            std::fs::write(&target, b"old").unwrap();
            let before = std::fs::metadata(&target).unwrap();
            let staged = AtomicReplacement::empty(&target).unwrap();
            let staged_path = staged.path().to_path_buf();

            if replacement {
                let external = dir.path().join("external.png");
                std::fs::write(&external, b"new").unwrap();
                File::open(&external)
                    .unwrap()
                    .set_times(FileTimes::new().set_modified(before.modified().unwrap()))
                    .unwrap();
                std::fs::rename(&external, &target).unwrap();
            } else {
                std::fs::write(&target, b"new").unwrap();
                // Same length and restored mtime must still conflict via ctime.
                File::open(&target)
                    .unwrap()
                    .set_times(FileTimes::new().set_modified(before.modified().unwrap()))
                    .unwrap();
            }
            let external = std::fs::metadata(&target).unwrap();

            let error = block_on(atomic_write_async(staged, b"rotated".to_vec())).unwrap_err();
            assert!(matches!(
                error,
                SaveRotationError::SourceChanged(ref path) if path == &target
            ));
            assert_eq!(std::fs::read(&target).unwrap(), b"new");
            assert_eq!(std::fs::metadata(&target).unwrap().ino(), external.ino());
            assert!(!staged_path.exists());
            assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
        }
    }

    #[test]
    fn sparse_save_rejects_changes_before_and_after_copy() {
        for change_before_copy in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let target = dir.path().join("img.png");
            std::fs::write(&target, b"original").unwrap();
            let staged = AtomicReplacement::empty(&target).unwrap();
            let staged_path = staged.path().to_path_buf();
            let error = if change_before_copy {
                std::fs::write(&target, b"external edit").unwrap();
                staged.copy_source().err().unwrap()
            } else {
                let mut staged = staged.copy_source().unwrap();
                staged.file_mut().write_all(b"rotation").unwrap();
                std::fs::write(&target, b"external edit").unwrap();
                staged.commit().unwrap_err()
            };
            assert!(matches!(
                save_write_error(&target, error),
                SaveRotationError::SourceChanged(_)
            ));
            assert_eq!(std::fs::read(&target).unwrap(), b"external edit");
            assert!(!staged_path.exists());
        }
    }

    #[test]
    fn staged_save_rejects_removal_and_symlink_replacement() {
        for symlink in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let target = dir.path().join("img.png");
            let other = dir.path().join("other.png");
            std::fs::write(&target, b"original").unwrap();
            std::fs::write(&other, b"other image").unwrap();
            let mut staged = AtomicReplacement::empty(&target).unwrap();
            staged.file_mut().write_all(b"rotation").unwrap();
            let staged_path = staged.path().to_path_buf();
            std::fs::remove_file(&target).unwrap();
            if symlink {
                std::os::unix::fs::symlink(&other, &target).unwrap();
            }

            assert!(matches!(
                save_write_error(&target, staged.commit().unwrap_err()),
                SaveRotationError::SourceChanged(_)
            ));
            assert_eq!(std::fs::read(&other).unwrap(), b"other image");
            assert_eq!(target.is_symlink(), symlink);
            assert!(!staged_path.exists());
        }
    }

    #[test]
    fn staged_save_rejects_permission_changes() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("img.png");
        std::fs::write(&target, b"original").unwrap();
        let mut staged = AtomicReplacement::empty(&target).unwrap();
        staged.file_mut().write_all(b"rotation").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600)).unwrap();

        assert!(matches!(
            save_write_error(&target, staged.commit().unwrap_err()),
            SaveRotationError::SourceChanged(_)
        ));
        assert_eq!(std::fs::read(&target).unwrap(), b"original");
        assert_eq!(std::fs::metadata(&target).unwrap().mode() & 0o777, 0o600);
    }

    #[test]
    fn sparse_save_preserves_source_copy_and_ignores_access_time_changes() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("img.png");
        std::fs::write(&target, b"original").unwrap();
        File::open(&target)
            .unwrap()
            .set_times(FileTimes::new().set_accessed(SystemTime::UNIX_EPOCH))
            .unwrap();
        let staged = AtomicReplacement::empty(&target)
            .unwrap()
            .copy_source()
            .unwrap();
        assert_eq!(std::fs::read(staged.path()).unwrap(), b"original");
        assert_eq!(std::fs::read(&target).unwrap(), b"original");
        staged.commit().unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"original");
    }

    #[test]
    fn atomic_write_replaces_content() {
        let dir = std::env::temp_dir().join(format!("open-mpv-fileops-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("img.png");
        std::fs::write(&target, b"old").unwrap();
        atomic_write(&target, b"new-content").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"new-content");
        // No temp file left behind.
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn abandoned_staged_write_keeps_original_and_cleans_up() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("img.png");
        std::fs::write(&target, b"old").unwrap();

        let mut staged = AtomicReplacement::empty(&target).unwrap();
        staged.file_mut().write_all(b"new").unwrap();
        let staged_path = staged.path().to_path_buf();
        drop(staged);

        assert_eq!(std::fs::read(&target).unwrap(), b"old");
        assert!(!staged_path.exists());
    }

    #[test]
    fn atomic_write_does_not_follow_a_predictable_temp_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("img.png");
        let decoy = dir.path().join("decoy");
        let old_temp = dir.path().join(".img.png.open-mpv-tmp");
        std::fs::write(&target, b"old").unwrap();
        std::fs::write(&decoy, b"guard").unwrap();
        std::os::unix::fs::symlink(&decoy, &old_temp).unwrap();

        atomic_write(&target, b"new").unwrap();

        assert_eq!(std::fs::read(&target).unwrap(), b"new");
        assert_eq!(std::fs::read(&decoy).unwrap(), b"guard");
        assert!(old_temp.is_symlink());
    }

    #[test]
    fn atomic_write_preserves_owned_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("img.png");
        std::fs::write(&target, b"old").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o640)).unwrap();
        let accessed = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_600_000_000);
        let modified = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        File::open(&target)
            .unwrap()
            .set_times(
                FileTimes::new()
                    .set_accessed(accessed)
                    .set_modified(modified),
            )
            .unwrap();
        let xattrs_supported = rustix::fs::setxattr(
            &target,
            "user.open-mpv-test",
            b"kept",
            rustix::fs::XattrFlags::empty(),
        )
        .is_ok();
        let before = std::fs::metadata(&target).unwrap();

        atomic_write(&target, b"new").unwrap();

        let after = std::fs::metadata(&target).unwrap();
        assert_eq!(after.uid(), before.uid());
        assert_eq!(after.gid(), before.gid());
        assert_eq!(after.permissions().mode() & 0o7777, 0o640);
        assert_eq!(after.accessed().unwrap(), accessed);
        assert!(after.modified().unwrap() > before.modified().unwrap());
        assert_ne!(
            after.ino(),
            before.ino(),
            "replacement must install a new inode"
        );

        if xattrs_supported {
            let file = File::open(&target).unwrap();
            let mut empty = [0_u8; 0];
            let len = rustix::fs::fgetxattr(&file, "user.open-mpv-test", &mut empty).unwrap();
            let mut value = vec![0; len];
            let len = rustix::fs::fgetxattr(&file, "user.open-mpv-test", &mut value).unwrap();
            value.truncate(len);
            assert_eq!(value, b"kept");
        } else {
            eprintln!("xattr assertion skipped: filesystem does not support user xattrs");
        }
    }

    #[test]
    fn atomic_write_cleans_up_on_failure() {
        let dir =
            std::env::temp_dir().join(format!("open-mpv-fileops-fail-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // Renaming over a directory fails after the temp file is written.
        let target = dir.join("occupied");
        std::fs::create_dir(&target).unwrap();
        assert!(atomic_write(&target, b"x").is_err());
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            1,
            "temp file must be cleaned up"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn post_commit_sync_error_is_not_reported_as_an_untouched_original() {
        let error = std::io::Error::from_raw_os_error(5);
        assert!(matches!(
            saved_outcome(Err(error)),
            SaveRotationOutcome::DurabilityUncertain(source) if source.raw_os_error() == Some(5)
        ));
    }
}
