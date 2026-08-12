//! Async image loading through glycin's sandboxed loaders (FR-2,
//! NFR-3.2) and a small bounded cache so neighbor navigation is
//! instant without memory growing with folder size (NFR-1.2, NFR-2.1).

use std::cell::RefCell;
use std::collections::VecDeque;
use std::fmt;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gtk4::gdk;
use gtk4::gio;
use gtk4::prelude::TextureExt;

/// Longest edge we ask glycin to re-render an SVG at while zooming;
/// bounds memory for pathological zoom levels.
pub const SVG_RENDER_MAX: u32 = 4096;

pub enum Decoded {
    Static {
        texture: gdk::Texture,
    },
    /// Keeps the glycin image (and its loader process) alive so frames
    /// can be pulled for playback.
    Animated {
        image: glycin::Image,
        first: gdk::Texture,
    },
    /// Keeps the glycin image alive to re-render sharply at new zoom
    /// levels; `nominal` is the document's own size in px.
    Svg {
        image: glycin::Image,
        first: gdk::Texture,
        nominal: (f64, f64),
    },
}

#[derive(Debug)]
pub enum DecodeError {
    Load(glycin::ErrorCtx),
    FirstFrame(glycin::ErrorCtx),
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DecodeError::Load(source) => write!(f, "image loader failed: {source}"),
            DecodeError::FirstFrame(source) => write!(f, "could not decode first frame: {source}"),
        }
    }
}

impl std::error::Error for DecodeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            DecodeError::Load(source) | DecodeError::FirstFrame(source) => Some(source),
        }
    }
}

impl Decoded {
    pub fn first_texture(&self) -> gdk::Texture {
        match self {
            Decoded::Static { texture } => texture.clone(),
            Decoded::Animated { first, .. } => first.clone(),
            Decoded::Svg { first, .. } => first.clone(),
        }
    }
}

pub async fn decode(path: &Path) -> Result<(Rc<Decoded>, String), DecodeError> {
    let started = std::time::Instant::now();
    let file = gio::File::for_path(path);
    let image = glycin::Loader::new(file)
        .load()
        .await
        .map_err(DecodeError::Load)?;
    let mime = image.mime_type().to_string();
    let frame = image.next_frame().await.map_err(DecodeError::FirstFrame)?;
    crate::applog!(
        "decode: {} {}x{} {} in {:.1} ms",
        path.display(),
        frame.width(),
        frame.height(),
        mime,
        started.elapsed().as_secs_f64() * 1000.0
    );
    let is_svg = matches!(mime.as_str(), "image/svg+xml" | "image/svg+xml-compressed");
    let decoded = if is_svg {
        Decoded::Svg {
            nominal: (f64::from(frame.width()), f64::from(frame.height())),
            first: frame.texture(),
            image,
        }
    } else if frame.delay().is_some() {
        Decoded::Animated {
            first: frame.texture(),
            image,
        }
    } else {
        Decoded::Static {
            texture: frame.texture(),
        }
    };
    Ok((Rc::new(decoded), mime))
}

/// Upper-bound resident size of a decoded frame, assuming 4 bytes per
/// pixel; glycin frames are often RGB8, so this over-counts by ≤ ⅓,
/// erring toward staying under the budget.
fn frame_bytes(decoded: &Decoded) -> usize {
    let t = decoded.first_texture();
    let (Ok(width), Ok(height)) = (usize::try_from(t.width()), usize::try_from(t.height())) else {
        // An invalid foreign dimension must never wrap into a small cache
        // charge. Treat it as over-budget so it cannot retain neighbors.
        return usize::MAX;
    };
    width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(4))
        .unwrap_or(usize::MAX)
}

/// Tiny LRU keyed by path: current image plus pre-decoded neighbors.
/// Bounded twice — by entry count and by estimated decoded bytes
/// (NFR-2.1) — so a folder of 100 MP photos cannot triple its RAM the
/// way it would with a count-only cap.
pub struct Cache {
    cap: usize,
    budget_bytes: usize,
    entries: RefCell<VecDeque<Entry>>,
    /// The image currently on screen. It is resident regardless of the
    /// cache (the window holds its own Rc), so evicting it frees
    /// nothing — it is never evicted and does not count against the
    /// budget, which therefore bounds only the *extra* memory spent on
    /// preloaded neighbors.
    pinned: RefCell<Option<PathBuf>>,
}

struct Entry {
    path: PathBuf,
    decoded: Rc<Decoded>,
    mime: String,
    bytes: usize,
}

impl Cache {
    pub fn new(cap: usize, budget_bytes: usize) -> Cache {
        Cache {
            cap,
            budget_bytes,
            entries: RefCell::new(VecDeque::new()),
            pinned: RefCell::new(None),
        }
    }

    /// Mark `path` as the image on screen (see `pinned`).
    pub fn pin(&self, path: &Path) {
        *self.pinned.borrow_mut() = Some(path.to_path_buf());
    }

    pub fn get(&self, path: &Path) -> Option<(Rc<Decoded>, String)> {
        let mut entries = self.entries.borrow_mut();
        let pos = entries.iter().position(|e| e.path == path)?;
        let entry = entries.remove(pos).unwrap();
        let result = (entry.decoded.clone(), entry.mime.clone());
        entries.push_front(entry);
        Some(result)
    }

    pub fn contains(&self, path: &Path) -> bool {
        self.entries.borrow().iter().any(|e| e.path == path)
    }

    pub fn put(&self, path: PathBuf, decoded: Rc<Decoded>, mime: String) {
        let bytes = frame_bytes(&decoded);
        let mut entries = self.entries.borrow_mut();
        if let Some(pos) = entries.iter().position(|e| e.path == path) {
            entries.remove(pos);
        }
        entries.push_front(Entry {
            path,
            decoded,
            mime,
            bytes,
        });
        entries.truncate(self.cap);
        // The newest entry always stays, even alone over budget: it is
        // the image being (or about to be) shown.
        let pinned = self.pinned.borrow();
        let is_pinned = |e: &Entry| Some(e.path.as_path()) == pinned.as_deref();
        let mut total: usize = entries
            .iter()
            .filter(|e| !is_pinned(e))
            .map(|e| e.bytes)
            .sum();
        while total > self.budget_bytes && entries.len() > 1 {
            let Some(pos) = entries.iter().rposition(|e| !is_pinned(e)) else {
                break;
            };
            if pos == 0 {
                break;
            }
            let evicted = entries.remove(pos).unwrap();
            total -= evicted.bytes;
            crate::applog!(
                "cache: evicted {} ({:.1} MB, budget {:.0} MB)",
                evicted.path.display(),
                evicted.bytes as f64 / (1024.0 * 1024.0),
                self.budget_bytes as f64 / (1024.0 * 1024.0)
            );
        }
    }

    /// Drop a path after the file changed on disk (rotate-save).
    pub fn invalidate(&self, path: &Path) {
        let mut entries = self.entries.borrow_mut();
        if let Some(pos) = entries.iter().position(|e| e.path == path) {
            entries.remove(pos);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A solid-color texture of the given size; MemoryTexture needs no
    /// display connection, so these tests run headless.
    fn decoded(w: i32, h: i32) -> Rc<Decoded> {
        let bytes = gtk4::glib::Bytes::from_owned(vec![0u8; (w * h * 4) as usize]);
        let tex =
            gdk::MemoryTexture::new(w, h, gdk::MemoryFormat::R8g8b8a8, &bytes, (w * 4) as usize);
        Rc::new(Decoded::Static {
            texture: tex.into(),
        })
    }

    fn put(cache: &Cache, name: &str, w: i32, h: i32) {
        cache.put(PathBuf::from(name), decoded(w, h), "image/png".into());
    }

    #[test]
    fn evicts_oldest_over_byte_budget() {
        // Budget fits two 100x100 RGBA frames (40 000 B each), not three.
        let cache = Cache::new(3, 90_000);
        put(&cache, "a", 100, 100);
        put(&cache, "b", 100, 100);
        put(&cache, "c", 100, 100);
        assert!(!cache.contains(Path::new("a")), "oldest should be evicted");
        assert!(cache.contains(Path::new("b")));
        assert!(cache.contains(Path::new("c")));
    }

    #[test]
    fn newest_entry_survives_even_over_budget() {
        let cache = Cache::new(3, 10_000);
        put(&cache, "big", 200, 200); // 160 000 B, alone over budget
        assert!(cache.contains(Path::new("big")));
        put(&cache, "big2", 200, 200);
        assert!(cache.contains(Path::new("big2")));
        assert!(
            !cache.contains(Path::new("big")),
            "only the newest oversized frame is kept"
        );
    }

    #[test]
    fn pinned_shown_image_is_never_evicted() {
        // Budget fits one 100x100 neighbor beyond the pinned image.
        let cache = Cache::new(3, 50_000);
        put(&cache, "shown", 100, 100);
        cache.pin(Path::new("shown"));
        put(&cache, "n1", 100, 100);
        put(&cache, "n2", 100, 100);
        assert!(cache.contains(Path::new("shown")));
        assert!(cache.contains(Path::new("n2")));
        assert!(!cache.contains(Path::new("n1")), "older neighbor evicted");
    }

    #[test]
    fn count_cap_still_applies_under_budget() {
        let cache = Cache::new(3, usize::MAX);
        for name in ["a", "b", "c", "d"] {
            put(&cache, name, 10, 10);
        }
        assert!(!cache.contains(Path::new("a")));
        assert!(cache.contains(Path::new("d")));
    }
}
