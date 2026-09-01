//! Source-media writes (FR-5): move to trash, restore from trash, and
//! rotate-save. Restores use no-replace rename semantics, and every image
//! edit is staged in a same-directory temporary file before replacement.

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
            RestoreError::Move { source, .. } => Some(source),
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

impl fmt::Display for SaveRotationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SaveRotationError::Editor { path, source } => {
                write!(f, "editor failed for {}: {source}", path.display())
            }
            SaveRotationError::Rotation { path, source } => {
                write!(f, "rotation failed for {}: {source}", path.display())
            }
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

pub async fn trash(path: &Path) -> Result<(), TrashError> {
    gio::File::for_path(path)
        .trash_future(glib::Priority::DEFAULT)
        .await
        .map_err(|source| TrashError {
            path: path.to_path_buf(),
            source,
        })
}

/// Restore the most recently trashed file whose original location was
/// `orig` (FR-5.2). Reads the freedesktop trash directories directly
/// (the same on-disk format GIO writes) rather than the `trash://`
/// gvfs backend, which is not reliably reachable outside a running
/// GUI main loop.
pub fn restore(orig: &Path) -> Result<(), RestoreError> {
    let trash_dirs = trash_dirs_for(orig);
    let (file, info) = find_trashed_file(orig, &trash_dirs)
        .ok_or_else(|| RestoreError::NotFound(orig.to_path_buf()))?;
    rustix::fs::renameat_with(
        rustix::fs::CWD,
        &file,
        rustix::fs::CWD,
        orig,
        rustix::fs::RenameFlags::NOREPLACE,
    )
    .map_err(|source| {
        let source = std::io::Error::from(source);
        if source.kind() == std::io::ErrorKind::AlreadyExists {
            RestoreError::DestinationExists(orig.to_path_buf())
        } else {
            RestoreError::Move {
                path: orig.to_path_buf(),
                source,
            }
        }
    })?;
    let _ = std::fs::remove_file(info);
    Ok(())
}

fn find_trashed_file(orig: &Path, trash_dirs: &[PathBuf]) -> Option<(PathBuf, PathBuf)> {
    let mut best: Option<(PathBuf, PathBuf, String)> = None; // (files/<n>, info file, date)
    for trash_dir in trash_dirs {
        let info_dir = trash_dir.join("info");
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
            let (mut path, mut date) = (None, String::new());
            for line in text.lines() {
                if let Some(v) = line.strip_prefix("Path=") {
                    path = Some(percent_decode(v));
                } else if let Some(v) = line.strip_prefix("DeletionDate=") {
                    date = v.to_string();
                }
            }
            let Some(path) = path else { continue };
            // Mount-level trashes store paths relative to the mount root.
            let abs = if path.is_absolute() {
                path
            } else {
                match trash_dir.parent().and_then(Path::parent) {
                    Some(top) => top.join(path),
                    None => continue,
                }
            };
            if abs != orig {
                continue;
            }
            let Some(stem) = info_path.file_stem() else {
                continue;
            };
            let file = trash_dir.join("files").join(stem);
            // ISO 8601 deletion dates compare correctly as strings.
            if best.as_ref().is_none_or(|(_, _, d)| date > *d) {
                best = Some((file, info_path, date));
            }
        }
    }
    best.map(|(file, info, _)| (file, info))
}

/// Trash directories that could hold `orig` per the freedesktop trash
/// spec: the home trash, and the `.Trash`/`.Trash-$uid` dirs at the top
/// of the file's mount point.
fn trash_dirs_for(orig: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![glib::user_data_dir().join("Trash")];
    if let Some(top) = mount_topdir(orig) {
        // Effective uid without adding a libc dependency (Linux).
        if let Ok(meta) = std::fs::metadata("/proc/self") {
            use std::os::unix::fs::MetadataExt;
            let uid = meta.uid();
            dirs.push(top.join(".Trash").join(uid.to_string()));
            dirs.push(top.join(format!(".Trash-{uid}")));
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
/// sandboxed editor. JPEG rotations are sparse metadata edits (no pixel
/// re-encode); every edit is staged and atomically installed (FR-5.4/5.5).
pub async fn save_rotation(path: &Path, cw_quarter_turns: u8) -> Result<(), SaveRotationError> {
    // glycin rotations are counter-clockwise.
    let rotation = match cw_quarter_turns % 4 {
        1 => Rotation::_270,
        2 => Rotation::_180,
        3 => Rotation::_90,
        _ => return Ok(()),
    };
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
    let edit = editable
        .apply_sparse(&ops)
        .await
        .map_err(|source| SaveRotationError::Rotation {
            path: path.to_path_buf(),
            source: Box::new(source),
        })?;
    match edit {
        glycin::SparseEdit::Sparse(_) => {
            let staged = AtomicReplacement::copy_of(path).map_err(|source| {
                SaveRotationError::AtomicWrite {
                    path: path.to_path_buf(),
                    source,
                }
            })?;
            let outcome = edit
                .apply_to(gio::File::for_path(staged.path()))
                .await
                .map_err(|source| SaveRotationError::SparseWrite {
                    path: path.to_path_buf(),
                    source,
                })?;
            debug_assert_eq!(outcome, glycin::EditOutcome::Changed);
            staged
                .commit()
                .map_err(|source| SaveRotationError::AtomicWrite {
                    path: path.to_path_buf(),
                    source,
                })
        }
        glycin::SparseEdit::Complete(data) => {
            let bytes = data.get_full().map_err(SaveRotationError::ReadEdited)?;
            atomic_write(path, &bytes).map_err(|source| SaveRotationError::AtomicWrite {
                path: path.to_path_buf(),
                source,
            })
        }
    }
}

/// Write through an exclusively created same-directory file, then fsync and
/// rename it over the destination (FR-5.5, NFR-3.1).
fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut staged = AtomicReplacement::empty(path)?;
    staged.file_mut().write_all(bytes)?;
    staged.commit()
}

struct AtomicReplacement {
    temp: tempfile::NamedTempFile,
    target: PathBuf,
    source: File,
    metadata: Metadata,
}

impl AtomicReplacement {
    fn empty(target: &Path) -> std::io::Result<Self> {
        let dir = target.parent().ok_or(std::io::ErrorKind::InvalidInput)?;
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
        Ok(Self {
            temp,
            target: target.to_path_buf(),
            source,
            metadata,
        })
    }

    fn copy_of(target: &Path) -> std::io::Result<Self> {
        let mut staged = Self::empty(target)?;
        let mut source = &staged.source;
        std::io::copy(&mut source, staged.temp.as_file_mut())?;
        Ok(staged)
    }

    fn path(&self) -> &Path {
        self.temp.path()
    }

    fn file_mut(&mut self) -> &mut File {
        self.temp.as_file_mut()
    }

    fn commit(self) -> std::io::Result<()> {
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

        let dir = self.target.parent().map(Path::to_path_buf);
        self.temp
            .persist(&self.target)
            .map_err(|error| error.error)?;
        if let Some(dir) = dir
            && let Ok(dir) = File::open(dir)
        {
            let _ = dir.sync_all();
        }
        Ok(())
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

        block_on(trash(&file)).unwrap();
        assert!(!file.exists(), "file must be gone after trash");

        restore(&file).unwrap();
        assert!(file.exists(), "file must be back after restore");
        assert_eq!(std::fs::read(&file).unwrap(), b"payload");

        // Restoring again must fail cleanly — nothing left in trash.
        assert!(matches!(
            restore(&file),
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

        block_on(trash(&file)).unwrap();
        std::fs::write(&file, b"replacement").unwrap();

        assert!(matches!(
            restore(&file),
            Err(RestoreError::DestinationExists(path)) if path == file
        ));
        assert_eq!(std::fs::read(&file).unwrap(), b"replacement");

        // The failed restore retained both the payload and its trash metadata,
        // so removing the conflict makes the same Undo recoverable.
        std::fs::remove_file(&file).unwrap();
        restore(&file).unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), b"trashed payload");
        std::fs::remove_dir_all(&dir).unwrap();
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

        let selected = find_trashed_file(&orig, std::slice::from_ref(&trash)).unwrap();
        assert_eq!(selected.0, trash.join("files/good"));
        assert_eq!(selected.1, trash.join("info/good.trashinfo"));
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

        block_on(save_rotation(&file, 1)).unwrap();

        // Either a sparse metadata edit (orientation flag, pixels kept)
        // or a full sandboxed rewrite is acceptable — but the displayed
        // result must be the 90° CW rotation: 20x40 after auto-orient.
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
        if rustix::fs::setxattr(
            &target,
            "user.open-mpv-test",
            b"kept",
            rustix::fs::XattrFlags::empty(),
        )
        .is_err()
        {
            eprintln!("skipping xattr metadata test: filesystem does not support user xattrs");
            return;
        }
        let before = std::fs::metadata(&target).unwrap();

        atomic_write(&target, b"new").unwrap();

        let after = std::fs::metadata(&target).unwrap();
        assert_eq!(after.uid(), before.uid());
        assert_eq!(after.gid(), before.gid());
        assert_eq!(after.permissions().mode() & 0o7777, 0o640);
        let file = File::open(&target).unwrap();
        let mut empty = [0_u8; 0];
        let len = rustix::fs::fgetxattr(&file, "user.open-mpv-test", &mut empty).unwrap();
        let mut value = vec![0; len];
        let len = rustix::fs::fgetxattr(&file, "user.open-mpv-test", &mut value).unwrap();
        value.truncate(len);
        assert_eq!(value, b"kept");
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
}
