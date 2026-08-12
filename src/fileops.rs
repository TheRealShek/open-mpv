//! The only module that writes to the filesystem (FR-5.6): move to
//! trash, restore from trash, and rotate-save. Saves are atomic —
//! either sparse in-place byte changes via glycin, or a full rewrite
//! staged in a temp file and renamed over the original (FR-5.5).

use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};

use gtk4::gio;
use gtk4::gio::prelude::*;
use gtk4::glib;

use gufo_common::orientation::Rotation;

/// MIME types the sandboxed editors can rewrite (queried once).
pub async fn editable_mime_types() -> BTreeSet<String> {
    glycin::EditableImage::supported_formats()
        .await
        .keys()
        .map(|m| m.to_string())
        .collect()
}

pub async fn trash(path: &Path) -> Result<(), String> {
    gio::File::for_path(path)
        .trash_future(glib::Priority::DEFAULT)
        .await
        .map_err(|e| format!("could not move {} to trash: {e}", path.display()))
}

/// Restore the most recently trashed file whose original location was
/// `orig` (FR-5.2). Reads the freedesktop trash directories directly
/// (the same on-disk format GIO writes) rather than the `trash://`
/// gvfs backend, which is not reliably reachable outside a running
/// GUI main loop.
pub fn restore(orig: &Path) -> Result<(), String> {
    let mut best: Option<(PathBuf, PathBuf, String)> = None; // (files/<n>, info file, date)
    for trash_dir in trash_dirs_for(orig) {
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
    let (file, info, _) =
        best.ok_or_else(|| format!("{} is no longer in the trash", orig.display()))?;
    std::fs::rename(&file, orig)
        .map_err(|e| format!("could not restore {}: {e}", orig.display()))?;
    let _ = std::fs::remove_file(info);
    Ok(())
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
            && let Ok(b) = u8::from_str_radix(&s[i + 1..i + 3], 16)
        {
            out.push(b);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    PathBuf::from(std::ffi::OsString::from_vec(out))
}

/// Persist a clockwise view rotation (in quarter turns) to disk via the
/// sandboxed editor. JPEG rotations are sparse metadata edits (no pixel
/// re-encode); other editable formats are rewritten atomically (FR-5.4).
pub async fn save_rotation(path: &Path, cw_quarter_turns: u8) -> Result<(), String> {
    // glycin rotations are counter-clockwise.
    let rotation = match cw_quarter_turns % 4 {
        1 => Rotation::_270,
        2 => Rotation::_180,
        3 => Rotation::_90,
        _ => return Ok(()),
    };
    let file = gio::File::for_path(path);
    let editable = glycin::Editor::new(file.clone())
        .edit()
        .await
        .map_err(|e| format!("editor failed for {}: {e}", path.display()))?;
    let ops = glycin::Operations::new(vec![glycin::Operation::Rotate(rotation)]);
    let edit = editable
        .apply_sparse(&ops)
        .await
        .map_err(|e| format!("rotation failed for {}: {e}", path.display()))?;
    match edit {
        glycin::SparseEdit::Sparse(_) => match edit.apply_to(file).await {
            Ok(_) => Ok(()),
            Err(e) => Err(format!("could not write {}: {e}", path.display())),
        },
        glycin::SparseEdit::Complete(data) => {
            let bytes = data
                .get_full()
                .map_err(|e| format!("could not read edited image data: {e}"))?;
            atomic_write(path, &bytes)
                .map_err(|e| format!("could not write {}: {e}", path.display()))
        }
    }
}

/// Write via a temp file in the same directory + fsync + rename, so a
/// crash never leaves a truncated file at `path` (FR-5.5, NFR-3.1).
fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().ok_or(std::io::ErrorKind::InvalidInput)?;
    let mut tmp = PathBuf::from(path);
    tmp.set_file_name(format!(
        ".{}.open-mpv-tmp",
        path.file_name().unwrap_or_default().to_string_lossy()
    ));
    let result = (|| {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)?;
        // Persist the rename itself; failure here doesn't lose data.
        if let Ok(d) = std::fs::File::open(dir) {
            let _ = d.sync_all();
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
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
        assert!(restore(&file).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
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
