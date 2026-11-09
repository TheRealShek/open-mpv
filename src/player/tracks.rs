//! Window-facing audio/subtitle values and automatic sidecar matching. Active
//! stream state and its transitions belong to `FocusedPlayback`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};

use crate::config;

/// A readable regular subtitle checked on a filesystem worker. Construction
/// performs I/O; consuming it on GTK does not. GStreamer reads the path later.
#[derive(Debug)]
pub struct ValidatedSubtitle {
    path: PathBuf,
}

impl ValidatedSubtitle {
    pub fn open(path: &Path, cancelled: &AtomicBool) -> io::Result<Self> {
        let check_cancelled = || {
            if cancelled.load(AtomicOrdering::Relaxed) {
                Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "subtitle request cancelled",
                ))
            } else {
                Ok(())
            }
        };
        check_cancelled()?;
        // Check the opened descriptor, not metadata followed by a blocking
        // open: a symlink or replacement may point to a FIFO by then.
        let file = rustix::fs::open(
            path,
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::NONBLOCK
                | rustix::fs::OFlags::CLOEXEC
                | rustix::fs::OFlags::NOCTTY,
            rustix::fs::Mode::empty(),
        )?;
        let metadata = rustix::fs::fstat(&file)?;
        if rustix::fs::FileType::from_raw_mode(metadata.st_mode)
            != rustix::fs::FileType::RegularFile
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "subtitle must be a regular file",
            ));
        }
        check_cancelled()?;
        Ok(Self {
            path: path.to_path_buf(),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioTrack {
    pub id: String,
    pub label: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum AudioChoice {
    #[default]
    Automatic,
    Track(String),
}

impl AudioChoice {
    pub fn action_target(&self) -> String {
        match self {
            AudioChoice::Automatic => "auto".to_string(),
            AudioChoice::Track(id) => format!("track:{id}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioSnapshot {
    pub tracks: Vec<AudioTrack>,
    pub choice: AudioChoice,
    pub active_label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubtitleTrack {
    pub id: String,
    pub label: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum SubtitleChoice {
    #[default]
    Automatic,
    Off,
    Track(String),
}

impl SubtitleChoice {
    pub fn action_target(&self) -> String {
        match self {
            SubtitleChoice::Automatic => "auto".to_string(),
            SubtitleChoice::Off => "off".to_string(),
            SubtitleChoice::Track(id) => format!("track:{id}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubtitleSnapshot {
    pub tracks: Vec<SubtitleTrack>,
    pub choice: SubtitleChoice,
    pub active_label: Option<String>,
}

/// Find one deterministic automatic sidecar without involving the folder
/// model or GIO. Exact `video.srt` wins, then SRT over WebVTT, then lexical
/// order among language/role suffixes (FR-10.7).
#[cfg(test)]
pub(crate) fn matching_sidecar(video: &Path) -> Option<PathBuf> {
    matching_sidecar_cancellable(video, &AtomicBool::new(false))
}

/// Find one deterministic automatic sidecar with cancellation support.
pub fn matching_sidecar_cancellable(video: &Path, cancelled: &AtomicBool) -> Option<PathBuf> {
    if cancelled.load(AtomicOrdering::Relaxed) {
        return None;
    }
    let stem = video.file_stem()?.to_str()?;
    let parent = video.parent()?;
    let mut matches: Vec<PathBuf> = Vec::new();
    for entry in fs::read_dir(parent).ok()?.filter_map(Result::ok) {
        if cancelled.load(AtomicOrdering::Relaxed) {
            return None;
        }
        let file_name = entry.file_name();
        let name_path = Path::new(&file_name);
        // Cheap filename predicates first: extension and stem matching before metadata/stat.
        if !config::is_subtitle(name_path) {
            continue;
        }
        let Some(candidate) = name_path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let stem_matches = candidate == stem
            || candidate
                .strip_prefix(stem)
                .and_then(|suffix| suffix.strip_prefix('.'))
                .is_some_and(|components| {
                    !components.is_empty()
                        && components.split('.').all(|component| !component.is_empty())
                });
        if !stem_matches {
            continue;
        }

        let is_regular = match entry.file_type() {
            Ok(ft) if ft.is_file() => true,
            Ok(ft) if ft.is_symlink() => entry.path().is_file(),
            Ok(_) => false,
            Err(_) => entry.path().is_file(),
        };
        if is_regular {
            matches.push(entry.path());
        }
    }

    if cancelled.load(AtomicOrdering::Relaxed) {
        return None;
    }

    matches.sort_by_key(|path| {
        let candidate = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        let exact = candidate != stem;
        let webvtt = path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("vtt"));
        (
            exact,
            webvtt,
            path.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_lowercase(),
        )
    });
    matches.into_iter().next()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_srt_sidecar_wins_over_language_and_webvtt_variants() {
        let dir =
            std::env::temp_dir().join(format!("open-mpv-sidecar-exact-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["movie.mkv", "movie.en.srt", "movie.vtt", "movie.srt"] {
            std::fs::write(dir.join(name), []).unwrap();
        }

        assert_eq!(
            matching_sidecar(&dir.join("movie.mkv")),
            Some(dir.join("movie.srt"))
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn sidecar_matching_rejects_prefix_collisions() {
        let dir =
            std::env::temp_dir().join(format!("open-mpv-sidecar-prefix-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["movie.mkv", "movie2.srt", "movie..srt", "movie.en.vtt"] {
            std::fs::write(dir.join(name), []).unwrap();
        }

        assert_eq!(
            matching_sidecar(&dir.join("movie.mkv")),
            Some(dir.join("movie.en.vtt"))
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn sidecar_matching_cancellable_stops_on_flag() {
        let dir =
            std::env::temp_dir().join(format!("open-mpv-sidecar-cancel-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("movie.mkv"), []).unwrap();
        std::fs::write(dir.join("movie.srt"), []).unwrap();

        let cancelled = AtomicBool::new(true);
        assert_eq!(
            matching_sidecar_cancellable(&dir.join("movie.mkv"), &cancelled),
            None
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn sidecar_matching_ignores_directories_and_non_subtitles() {
        let dir =
            std::env::temp_dir().join(format!("open-mpv-sidecar-filter-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("movie.mkv"), []).unwrap();
        std::fs::create_dir(dir.join("movie.srt")).unwrap(); // directory, not regular file
        std::fs::write(dir.join("movie.txt"), []).unwrap(); // non-subtitle
        std::fs::write(dir.join("movie.en.vtt"), []).unwrap();

        assert_eq!(
            matching_sidecar(&dir.join("movie.mkv")),
            Some(dir.join("movie.en.vtt"))
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[cfg(test)]
mod validation_tests {
    use super::*;
    use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};

    #[test]
    fn validation_accepts_readable_files_and_regular_symlink_targets() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("movie.srt");
        fs::write(&path, "subtitle").unwrap();
        let link = dir.path().join("linked.vtt");
        symlink(&path, &link).unwrap();
        for path in [&path, &link] {
            let checked = ValidatedSubtitle::open(path, &AtomicBool::new(false)).unwrap();
            assert_eq!(checked.path(), path);
        }
    }

    #[test]
    fn validation_rejects_special_files_and_symlinks_to_them() {
        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join("blocked.srt");
        rustix::fs::mknodat(
            rustix::fs::CWD,
            &fifo,
            rustix::fs::FileType::Fifo,
            rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
            0,
        )
        .unwrap();
        let link = dir.path().join("linked.srt");
        symlink(&fifo, &link).unwrap();
        for path in [
            fifo.as_path(),
            link.as_path(),
            dir.path(),
            Path::new("/dev/null"),
        ] {
            let error = ValidatedSubtitle::open(path, &AtomicBool::new(false)).unwrap_err();
            assert_eq!(
                error.kind(),
                io::ErrorKind::InvalidInput,
                "{}",
                path.display()
            );
        }
    }

    #[test]
    fn validation_preserves_unreadable_and_missing_file_errors() {
        let dir = tempfile::tempdir().unwrap();
        if fs::metadata(dir.path()).unwrap().uid() == 0 {
            use std::os::unix::process::CommandExt;

            // Fedora CI runs as root, which bypasses mode bits. Re-run as an
            // unprivileged user so permission-denied coverage stays meaningful.
            // Copy the test executable out of potentially private build paths.
            fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).unwrap();
            let executable = dir.path().join("subtitle-permission-test");
            fs::copy(std::env::current_exe().unwrap(), &executable).unwrap();
            fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
            let status = std::process::Command::new(executable)
                .args([
                    "--exact",
                    "player::tracks::validation_tests::validation_preserves_unreadable_and_missing_file_errors",
                    "--nocapture",
                ])
                .uid(65534)
                .gid(65534)
                .status()
                .unwrap();
            assert!(
                status.success(),
                "unprivileged subtitle validation failed: {status}"
            );
            return;
        }
        let path = dir.path().join("unreadable.srt");
        fs::write(&path, "subtitle").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
        let error = ValidatedSubtitle::open(&path, &AtomicBool::new(false)).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        let missing = dir.path().join("missing.srt");
        let link = dir.path().join("broken.srt");
        symlink(&missing, &link).unwrap();
        for path in [&missing, &link] {
            let error = ValidatedSubtitle::open(path, &AtomicBool::new(false)).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::NotFound);
        }
    }

    #[test]
    fn cancelled_validation_does_not_open_the_file() {
        let error =
            ValidatedSubtitle::open(Path::new("missing.srt"), &AtomicBool::new(true)).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    }
}
