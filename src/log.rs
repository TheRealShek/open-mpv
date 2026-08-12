//! Diagnostic logging, enabled by default and disabled with
//! `OPEN_MPV_LOG=0`. Every `applog!` site is one atomic bool check when
//! disabled — no
//! formatting, no I/O. Lines carry monotonic ms since launch so the
//! log doubles as a performance trace against the NFR-1 budgets.
//! Never log from the render path (`ImageView::snapshot`).

use std::sync::OnceLock;
use std::time::Instant;

static START: OnceLock<Instant> = OnceLock::new();
static ENABLED: OnceLock<bool> = OnceLock::new();

/// Anchor the timestamp clock; called first thing in `main`.
pub fn init() {
    START.get_or_init(Instant::now);
    if enabled() {
        crate::applog!("logging enabled");
    }
}

pub fn enabled() -> bool {
    *ENABLED.get_or_init(|| enabled_for(std::env::var_os("OPEN_MPV_LOG").as_deref()))
}

fn enabled_for(value: Option<&std::ffi::OsStr>) -> bool {
    value != Some(std::ffi::OsStr::new("0"))
}

pub fn write(args: std::fmt::Arguments) {
    let ms = START.get_or_init(Instant::now).elapsed().as_secs_f64() * 1000.0;
    eprintln!("open-mpv [{ms:9.1} ms] {args}");
}

/// Log unless `OPEN_MPV_LOG=0`; free otherwise (arguments are not even
/// formatted).
#[macro_export]
macro_rules! applog {
    ($($arg:tt)*) => {
        if $crate::log::enabled() {
            $crate::log::write(format_args!($($arg)*));
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logging_is_on_unless_explicitly_disabled() {
        assert!(enabled_for(None));
        assert!(enabled_for(Some(std::ffi::OsStr::new(""))));
        assert!(enabled_for(Some(std::ffi::OsStr::new("1"))));
        assert!(!enabled_for(Some(std::ffi::OsStr::new("0"))));
    }
}
