//! Opt-in diagnostic logging (`OPEN_MPV_LOG=1`), designed to be free
//! when disabled: every `applog!` site is one atomic bool check — no
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
        crate::applog!("logging enabled (OPEN_MPV_LOG)");
    }
}

pub fn enabled() -> bool {
    *ENABLED.get_or_init(|| std::env::var("OPEN_MPV_LOG").is_ok_and(|v| !v.is_empty() && v != "0"))
}

pub fn write(args: std::fmt::Arguments) {
    let ms = START.get_or_init(Instant::now).elapsed().as_secs_f64() * 1000.0;
    eprintln!("open-mpv [{ms:9.1} ms] {args}");
}

/// Log when `OPEN_MPV_LOG` is set; free otherwise (arguments are not
/// even formatted).
#[macro_export]
macro_rules! applog {
    ($($arg:tt)*) => {
        if $crate::log::enabled() {
            $crate::log::write(format_args!($($arg)*));
        }
    };
}
