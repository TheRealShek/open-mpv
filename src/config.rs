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
            binds: BTreeMap::new(),
        }
    }
}

impl Config {
    /// Load from the default path, falling back to defaults if absent.
    pub fn load() -> Config {
        let path = gtk4::glib::user_config_dir().join("open-mpv/open-mpv.conf");
        match std::fs::read_to_string(&path) {
            Ok(text) => Config::parse(&text, &path.display().to_string()),
            Err(_) => Config::default(),
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

/// True if the path has an extension we try to open (FR-2).
pub fn is_supported(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .as_deref(),
        Some("jpg" | "jpeg" | "png" | "webp" | "avif" | "bmp" | "gif" | "svg" | "svgz")
    )
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
        assert!(c.binds.is_empty());
    }

    #[test]
    fn parses_options_and_binds() {
        let text = "\n# comment\nbackground = #000000\nsort=date\nwrap=yes\nfit=actual\noverlay-timeout=1.5\nbind=n next\nbind=BackSpace prev\n";
        let c = Config::parse(text, "t");
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
        let text = "nonsense\nsort=upside-down\nwrap=maybe\nbind=x\nunknown=1\nsort=date\n";
        let c = Config::parse(text, "t");
        // The one valid line still applies; everything else falls back.
        assert_eq!(c.sort, SortOrder::Date);
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
}
