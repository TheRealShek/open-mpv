//! mpv-style plain-text configuration (FR-8).
//!
//! Format: `key=value` lines and whole-line `#` comments. Keybindings use repeated
//! `bind=<key> <action>` lines; a user bind for a key overrides the
//! default binding of that key. Unknown or malformed lines warn on
//! stderr and are skipped — a bad config never prevents startup (FR-8.3).

use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortOrder {
    Name,
    Date,
}

/// Folder ordering: which key, and which way. Reversing is separate from
/// the key because the useful direction differs — names read naturally
/// ascending, dates newest-first (FR-3.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sort {
    pub order: SortOrder,
    pub reverse: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FitMode {
    Fit,
    Actual,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubtitleMode {
    Auto,
    Off,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub background: String,
    pub sort: Sort,
    pub wrap: bool,
    /// Replay a video at end of stream, the way animated images loop
    /// (FR-10.3). Off means the last frame simply stays up.
    pub loop_video: bool,
    /// Open fullscreen instead of sized to the media (FR-6.6).
    pub start_fullscreen: bool,
    /// Starting playback volume, 0.0..=1.5.
    pub volume: f64,
    /// Whether each video starts with GStreamer's automatic subtitle
    /// selection or with text streams disabled (FR-10.7).
    pub subtitles: SubtitleMode,
    pub fit: FitMode,
    /// Seconds of mouse inactivity before overlay controls fade out.
    pub overlay_timeout: f64,
    /// Hide the pointer along with the overlay controls, the way mpv
    /// does. Any pointer movement brings both back.
    pub hide_cursor: bool,
    /// Megabytes of decoded frames the cache may hold *beyond* the
    /// image on screen (NFR-2.1). 0 disables neighbor preloading's
    /// memory entirely.
    pub cache_budget_mb: u32,
    /// Key → action name, merged over the built-in defaults.
    pub binds: BTreeMap<String, String>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            background: "#121212".into(),
            sort: Sort {
                order: SortOrder::Name,
                reverse: false,
            },
            wrap: false,
            loop_video: true,
            start_fullscreen: false,
            volume: 1.0,
            subtitles: SubtitleMode::Auto,
            fit: FitMode::Fit,
            overlay_timeout: 2.0,
            hide_cursor: true,
            cache_budget_mb: 256,
            binds: BTreeMap::new(),
        }
    }
}

impl Config {
    /// Load from the default path, falling back to defaults if absent.
    pub fn load() -> Config {
        let path = gtk4::glib::user_config_dir().join("open-mpv/open-mpv.conf");
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                let cfg = Config::parse(&text, &path.display().to_string());
                crate::applog!(
                    "config: loaded {} ({} binds)",
                    path.display(),
                    cfg.binds.len()
                );
                cfg
            }
            Err(_) => {
                crate::applog!("config: {} absent, using defaults", path.display());
                Config::default()
            }
        }
    }

    pub fn parse(text: &str, origin: &str) -> Config {
        let mut cfg = Config::default();
        for (lineno, raw) in text.lines().enumerate() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                warn(origin, lineno, raw, "expected key=value");
                continue;
            };
            let (key, value) = (key.trim(), value.trim());
            match key {
                "background" => cfg.background = value.to_string(),
                "sort" => match value {
                    "name" => cfg.sort.order = SortOrder::Name,
                    "date" => cfg.sort.order = SortOrder::Date,
                    _ => warn(origin, lineno, raw, "sort must be name|date"),
                },
                "sort-reverse" => match parse_bool(value) {
                    Some(b) => cfg.sort.reverse = b,
                    None => warn(origin, lineno, raw, "sort-reverse must be yes|no"),
                },
                "loop" => match parse_bool(value) {
                    Some(b) => cfg.loop_video = b,
                    None => warn(origin, lineno, raw, "loop must be yes|no"),
                },
                "start-fullscreen" => match parse_bool(value) {
                    Some(b) => cfg.start_fullscreen = b,
                    None => warn(origin, lineno, raw, "start-fullscreen must be yes|no"),
                },
                "volume" => match value.parse::<f64>() {
                    Ok(v) if (0.0..=150.0).contains(&v) => cfg.volume = v / 100.0,
                    _ => warn(origin, lineno, raw, "volume must be a percentage, 0-150"),
                },
                "subtitles" => match value {
                    "auto" => cfg.subtitles = SubtitleMode::Auto,
                    "off" => cfg.subtitles = SubtitleMode::Off,
                    _ => warn(origin, lineno, raw, "subtitles must be auto|off"),
                },
                "wrap" => match parse_bool(value) {
                    Some(b) => cfg.wrap = b,
                    None => warn(origin, lineno, raw, "wrap must be yes|no"),
                },
                "fit" => match value {
                    "fit" => cfg.fit = FitMode::Fit,
                    "actual" => cfg.fit = FitMode::Actual,
                    _ => warn(origin, lineno, raw, "fit must be fit|actual"),
                },
                "overlay-timeout" => match value.parse::<f64>() {
                    Ok(t) if t >= 0.0 => cfg.overlay_timeout = t,
                    _ => warn(origin, lineno, raw, "overlay-timeout must be seconds"),
                },
                "hide-cursor" => match parse_bool(value) {
                    Some(b) => cfg.hide_cursor = b,
                    None => warn(origin, lineno, raw, "hide-cursor must be yes|no"),
                },
                "cache-budget-mb" => match value.parse::<u32>() {
                    Ok(mb) => cfg.cache_budget_mb = mb,
                    _ => warn(origin, lineno, raw, "cache-budget-mb must be megabytes"),
                },
                "bind" => match value.split_once(' ') {
                    Some((k, action)) if !k.is_empty() && !action.trim().is_empty() => {
                        cfg.binds.insert(k.to_string(), action.trim().to_string());
                    }
                    _ => warn(origin, lineno, raw, "bind needs `<key> <action>`"),
                },
                _ => warn(origin, lineno, raw, "unknown option"),
            }
        }
        cfg
    }
}

fn parse_bool(v: &str) -> Option<bool> {
    match v {
        "yes" | "true" | "on" => Some(true),
        "no" | "false" | "off" => Some(false),
        _ => None,
    }
}

fn warn(origin: &str, lineno: usize, line: &str, msg: &str) {
    eprintln!(
        "open-mpv: {origin}:{}: ignoring `{line}`: {msg}",
        lineno + 1
    );
}

/// Extensions routed to glycin (FR-2). This mirrors what the installed
/// glycin loaders advertise — deliberately a static list rather than a
/// `Loader::supported_mime_types()` query, because the folder scan runs
/// before the first frame and a D-Bus round trip there would eat the
/// cold-start budget (NFR-1.1). `format_list_covers_installed_loaders`
/// fails the build's tests if the loader set ever outgrows this.
pub const IMAGE_EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", "png", "apng", "webp", "avif", "avifs", "bmp", "gif", "svg", "svgz", "heic",
    "heif", "hif", "jxl", "tif", "tiff", "jp2", "jpg2", "j2c", "jpc", "j2k", "ico", "cur", "tga",
    "qoi", "exr", "dds", "pnm", "pbm", "pgm", "ppm", "xbm", "xpm",
];

/// Video containers routed to the GStreamer player instead of glycin
/// (FR-10.1). The codec set inside is whatever the installed hardware and
/// software GStreamer plugins decode.
pub const VIDEO_EXTENSIONS: &[&str] = &["mp4", "m4v", "mkv", "webm", "mov", "avi"];

/// Text sidecars whose rendering is supplied by the base GStreamer stack
/// on the target Fedora installation (FR-10.7). They are deliberately not
/// part of `is_supported`: subtitles attach to a video and never enter the
/// folder navigation set.
pub const SUBTITLE_EXTENSIONS: &[&str] = &["srt", "vtt"];

/// True if the path has an extension we try to open (FR-2, FR-10).
pub fn is_supported(path: &Path) -> bool {
    is_image(path) || is_video(path)
}

pub fn is_image(path: &Path) -> bool {
    has_extension(path, IMAGE_EXTENSIONS)
}

pub fn is_video(path: &Path) -> bool {
    has_extension(path, VIDEO_EXTENSIONS)
}

pub fn is_subtitle(path: &Path) -> bool {
    has_extension(path, SUBTITLE_EXTENSIONS)
}

/// Case-insensitive extension match, without lowercasing into a fresh
/// `String` first: this runs up to twice for every entry in a directory
/// during a folder scan, on the cold-start path (NFR-1.1).
fn has_extension(path: &Path, extensions: &[&str]) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| {
            extensions
                .iter()
                .any(|known| known.eq_ignore_ascii_case(ext))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_on_empty() {
        let c = Config::parse("", "t");
        assert_eq!(c.background, "#121212");
        assert_eq!(c.sort.order, SortOrder::Name);
        assert!(!c.sort.reverse);
        assert!(!c.wrap);
        assert_eq!(c.fit, FitMode::Fit);
        assert_eq!(c.overlay_timeout, 2.0);
        assert_eq!(c.cache_budget_mb, 256);
        assert_eq!(c.subtitles, SubtitleMode::Auto);
        assert!(c.binds.is_empty());
    }

    #[test]
    fn parses_options_and_binds() {
        let text = "\n# comment\nbackground = #000000\nsort=date\nsort-reverse=yes\nwrap=yes\nfit=actual\noverlay-timeout=1.5\nhide-cursor=no\nloop=no\nstart-fullscreen=yes\nvolume=70\nsubtitles=off\ncache-budget-mb=64\nbind=n next\nbind=BackSpace prev\n";
        let c = Config::parse(text, "t");
        assert_eq!(c.cache_budget_mb, 64);
        assert_eq!(c.background, "#000000");
        assert_eq!(c.sort.order, SortOrder::Date);
        assert!(c.wrap);
        assert_eq!(c.fit, FitMode::Actual);
        assert_eq!(c.overlay_timeout, 1.5);
        assert!(c.sort.reverse);
        assert!(!c.hide_cursor);
        assert!(!c.loop_video);
        assert!(c.start_fullscreen);
        assert_eq!(c.volume, 0.7);
        assert_eq!(c.subtitles, SubtitleMode::Off);
        assert_eq!(c.binds.get("n").map(String::as_str), Some("next"));
        assert_eq!(c.binds.get("BackSpace").map(String::as_str), Some("prev"));
    }

    #[test]
    fn documented_config_example_parses_as_shown() {
        let guide = include_str!("../docs/CONFIGURATION.md");
        let example = guide
            .split_once("```ini\n")
            .and_then(|(_, rest)| rest.split_once("\n```"))
            .map(|(example, _)| example)
            .expect("configuration guide must contain an ini example");

        let c = Config::parse(example, "docs/CONFIGURATION.md");
        assert_eq!(c.background, "#202020");
        assert_eq!(c.sort.order, SortOrder::Date);
        assert_eq!(c.fit, FitMode::Actual);
        assert_eq!(c.volume, 0.7);
        assert_eq!(c.subtitles, SubtitleMode::Off);
        assert!(!c.sort.reverse);
        assert!(!c.wrap);
        assert_eq!(c.overlay_timeout, 2.0);
        assert!(c.hide_cursor);
        assert!(!c.start_fullscreen);
        assert!(c.loop_video);
        assert_eq!(c.cache_budget_mb, 256);
        assert_eq!(c.binds.get("q").map(String::as_str), Some("none"));
    }

    #[test]
    fn configuration_guide_lists_every_setting() {
        let guide = include_str!("../docs/CONFIGURATION.md");
        let settings = [
            "background",
            "sort",
            "sort-reverse",
            "wrap",
            "fit",
            "overlay-timeout",
            "hide-cursor",
            "start-fullscreen",
            "loop",
            "volume",
            "subtitles",
            "cache-budget-mb",
            "bind",
        ];

        for setting in settings {
            assert!(
                guide.contains(&format!("`{setting}`")),
                "configuration guide does not document `{setting}`"
            );
        }
    }

    #[test]
    fn malformed_lines_are_skipped() {
        let text = "nonsense\nsort=upside-down\nwrap=maybe\nbind=x\nunknown=1\ncache-budget-mb=lots\nvolume=999\nsubtitles=maybe\nloop=perhaps\nsort=date\n";
        let c = Config::parse(text, "t");
        // The one valid line still applies; everything else falls back.
        assert_eq!(c.sort.order, SortOrder::Date);
        assert_eq!(c.cache_budget_mb, 256);
        assert!(!c.wrap);
        // Out-of-range and unparseable values keep their defaults.
        assert_eq!(c.volume, 1.0);
        assert_eq!(c.subtitles, SubtitleMode::Auto);
        assert!(c.loop_video);
        assert!(c.binds.is_empty());
    }

    #[test]
    fn supported_extensions() {
        assert!(is_supported(Path::new("a/b/photo.JPG")));
        assert!(is_supported(Path::new("scan.jpeg")));
        assert!(is_supported(Path::new("scan.JPEG")));
        assert!(is_supported(Path::new("anim.webp")));
        assert!(is_supported(Path::new("v.svgz")));
        // Formats the installed loaders decode that the allowlist used to
        // refuse on extension alone.
        assert!(is_supported(Path::new("IMG_0042.HEIC")));
        assert!(is_supported(Path::new("photo.jxl")));
        assert!(is_supported(Path::new("scan.tiff")));
        assert!(!is_supported(Path::new("doc.pdf")));
        assert!(!is_supported(Path::new("noext")));
    }

    /// The extension list is static so the folder scan never waits on
    /// D-Bus (NFR-1.1). This pins it to what this machine's glycin
    /// loaders actually advertise, so a loader gained or lost in a system
    /// update surfaces here instead of silently making files unopenable.
    #[test]
    fn format_list_covers_installed_loaders() {
        use std::collections::BTreeSet;

        // Alias types that shared-mime-info knows no filename glob for:
        // no extension can map to them, so they are not our gap.
        const NO_GLOB: &[&str] = &["image/x-qoi"];

        let ctx = gtk4::glib::MainContext::new();
        let _acquired = ctx.acquire().unwrap();
        let supported = ctx
            .with_thread_default(|| ctx.block_on(glycin::Loader::supported_mime_types()))
            .unwrap();
        assert!(
            !supported.is_empty(),
            "no glycin loaders installed; cannot verify format coverage"
        );

        let ours: BTreeSet<String> = IMAGE_EXTENSIONS
            .iter()
            .map(|e| {
                gtk4::gio::content_type_guess(Some(format!("x.{e}")), None::<&[u8]>)
                    .0
                    .to_string()
            })
            .collect();
        let missing: Vec<String> = supported
            .iter()
            .map(|m| m.to_string())
            .filter(|m| !ours.contains(m) && !NO_GLOB.contains(&m.as_str()))
            .collect();
        assert!(
            missing.is_empty(),
            "glycin loaders decode {missing:?} but no extension in IMAGE_EXTENSIONS maps to them"
        );
    }

    /// Adding a format to `IMAGE_EXTENSIONS` without registering its MIME
    /// type means double-clicking the file in Files still opens something
    /// else — the format works from the CLI and nowhere else (FR-9.1).
    #[test]
    fn desktop_entry_registers_every_image_format() {
        use std::collections::BTreeSet;

        let desktop = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/data/io.github.TheRealShek.OpenMpv.desktop"
        ))
        .expect("desktop entry must be readable");
        let registered: BTreeSet<&str> = desktop
            .lines()
            .find_map(|l| l.strip_prefix("MimeType="))
            .expect("desktop entry must declare MimeType")
            .split(';')
            .filter(|s| !s.is_empty())
            .collect();

        let unregistered: Vec<String> = IMAGE_EXTENSIONS
            .iter()
            .map(|e| {
                gtk4::gio::content_type_guess(Some(format!("x.{e}")), None::<&[u8]>)
                    .0
                    .to_string()
            })
            .filter(|m| !registered.contains(m.as_str()))
            .collect();
        assert!(
            unregistered.is_empty(),
            "IMAGE_EXTENSIONS opens {unregistered:?} but the .desktop MimeType line omits them"
        );
    }

    #[test]
    fn video_extensions() {
        assert!(is_video(Path::new("clip.mp4")));
        assert!(is_video(Path::new("CLIP.MKV")));
        assert!(is_video(Path::new("a/b.webm")));
        assert!(is_video(Path::new("v.mov")));
        assert!(!is_video(Path::new("photo.jpg")));
        assert!(!is_image(Path::new("clip.mp4")));
        // Videos are supported paths, but never images.
        assert!(is_supported(Path::new("clip.mp4")));
    }

    #[test]
    fn subtitle_extensions_attach_but_never_enter_navigation() {
        assert!(is_subtitle(Path::new("clip.en.SRT")));
        assert!(is_subtitle(Path::new("clip.vtt")));
        assert!(!is_subtitle(Path::new("clip.ass")));
        assert!(!is_supported(Path::new("clip.srt")));
    }
}
