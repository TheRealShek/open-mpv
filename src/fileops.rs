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
/// `orig` (FR-5.2). Uses the freedesktop trash via GIO, so the entry
/// disappears from Files' trash view too.
pub async fn restore(orig: &Path) -> Result<(), String> {
    let trash_root = gio::File::for_uri("trash:///");
    let attrs = "standard::name,trash::orig-path,trash::deletion-date";
    let enumerator = trash_root
        .enumerate_children_future(attrs, gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS, glib::Priority::DEFAULT)
        .await
        .map_err(|e| format!("could not read trash: {e}"))?;

    let mut best: Option<(String, String)> = None; // (trash item name, deletion date)
    loop {
        let batch = enumerator
            .next_files_future(64, glib::Priority::DEFAULT)
            .await
            .map_err(|e| format!("could not read trash: {e}"))?;
        if batch.is_empty() {
            break;
        }
        for info in batch {
            let orig_path = info
                .attribute_byte_string("trash::orig-path")
                .map(|s| PathBuf::from(s.as_str()));
            if orig_path.as_deref() == Some(orig) {
                let date = info
                    .attribute_string("trash::deletion-date")
                    .map(|s| s.to_string())
                    .unwrap_or_default();
                // ISO 8601 dates compare correctly as strings.
                if best.as_ref().is_none_or(|(_, d)| date > *d) {
                    best = Some((info.name().display().to_string(), date));
                }
            }
        }
    }

    let (name, _) = best.ok_or_else(|| {
        format!("{} is no longer in the trash", orig.display())
    })?;
    let src = trash_root.child(&name);
    let dst = gio::File::for_path(orig);
    src.move_future(
        &dst,
        gio::FileCopyFlags::NOFOLLOW_SYMLINKS,
        glib::Priority::DEFAULT,
    )
    .0
    .await
    .map_err(|e| format!("could not restore {}: {e}", orig.display()))
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
            atomic_write(path, &bytes).map_err(|e| format!("could not write {}: {e}", path.display()))
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
        let dir = std::env::temp_dir().join(format!("open-mpv-fileops-fail-{}", std::process::id()));
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
