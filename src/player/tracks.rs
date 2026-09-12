//! Window-facing audio/subtitle values and automatic sidecar matching. Active
//! stream state and its transitions belong to `FocusedPlayback`.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};

use crate::config;

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
