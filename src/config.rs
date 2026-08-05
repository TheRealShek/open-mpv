//! mpv-style plain-text configuration (FR-8).
//!
//! Format: `key=value` lines, `#` comments. Keybindings use repeated
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FitMode {
    Fit,
    Actual,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub background: String,
    pub sort: SortOrder,
    pub wrap: bool,
    pub fit: FitMode,
    /// Seconds of mouse inactivity before overlay controls fade out.
    pub overlay_timeout: f64,
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
            sort: SortOrder::Name,
            wrap: false,
            fit: FitMode::Fit,
            overlay_timeout: 2.0,
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
                    "name" => cfg.sort = SortOrder::Name,
                    "date" => cfg.sort = SortOrder::Date,
                    _ => warn(origin, lineno, raw, "sort must be name|date"),
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

/// True if the path has an extension we try to open (FR-2, FR-9).
pub fn is_supported(path: &Path) -> bool {
    is_image(path) || is_video(path)
}

pub fn is_image(path: &Path) -> bool {
    matches!(
        lowercase_ext(path).as_deref(),
        Some("jpg" | "jpeg" | "png" | "webp" | "avif" | "bmp" | "gif" | "svg" | "svgz")
    )
}

/// Video containers routed to the GStreamer player instead of glycin
/// (FR-9). The codec set inside is whatever the system's VA-API /
/// GStreamer plugins decode.
pub fn is_video(path: &Path) -> bool {
    matches!(
        lowercase_ext(path).as_deref(),
        Some("mp4" | "m4v" | "mkv" | "webm" | "mov" | "avi")
    )
}

fn lowercase_ext(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_on_empty() {
        let c = Config::parse("", "t");
        assert_eq!(c.background, "#121212");
        assert_eq!(c.sort, SortOrder::Name);
        assert!(!c.wrap);
        assert_eq!(c.fit, FitMode::Fit);
        assert_eq!(c.overlay_timeout, 2.0);
        assert_eq!(c.cache_budget_mb, 256);
        assert!(c.binds.is_empty());
    }

    #[test]
    fn parses_options_and_binds() {
        let text = "\n# comment\nbackground = #000000\nsort=date\nwrap=yes\nfit=actual\noverlay-timeout=1.5\ncache-budget-mb=64\nbind=n next\nbind=BackSpace prev\n";
        let c = Config::parse(text, "t");
        assert_eq!(c.cache_budget_mb, 64);
        assert_eq!(c.background, "#000000");
        assert_eq!(c.sort, SortOrder::Date);
        assert!(c.wrap);
        assert_eq!(c.fit, FitMode::Actual);
        assert_eq!(c.overlay_timeout, 1.5);
        assert_eq!(c.binds.get("n").map(String::as_str), Some("next"));
        assert_eq!(c.binds.get("BackSpace").map(String::as_str), Some("prev"));
    }

    #[test]
    fn malformed_lines_are_skipped() {
        let text = "nonsense\nsort=upside-down\nwrap=maybe\nbind=x\nunknown=1\ncache-budget-mb=lots\nsort=date\n";
        let c = Config::parse(text, "t");
        // The one valid line still applies; everything else falls back.
        assert_eq!(c.sort, SortOrder::Date);
        assert_eq!(c.cache_budget_mb, 256);
        assert!(!c.wrap);
        assert!(c.binds.is_empty());
    }

    #[test]
    fn supported_extensions() {
        assert!(is_supported(Path::new("a/b/photo.JPG")));
        assert!(is_supported(Path::new("scan.jpeg")));
        assert!(is_supported(Path::new("scan.JPEG")));
        assert!(is_supported(Path::new("anim.webp")));
        assert!(is_supported(Path::new("v.svgz")));
        assert!(!is_supported(Path::new("doc.pdf")));
        assert!(!is_supported(Path::new("noext")));
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
}
