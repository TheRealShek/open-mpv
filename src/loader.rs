//! Async image loading through glycin's sandboxed loaders (FR-2,
//! NFR-3.2) and a small bounded cache so neighbor navigation is
//! instant without memory growing with folder size (NFR-1.2, NFR-2.1).

use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gtk4::gdk;
use gtk4::gio;

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

impl Decoded {
    pub fn first_texture(&self) -> gdk::Texture {
        match self {
            Decoded::Static { texture } => texture.clone(),
            Decoded::Animated { first, .. } => first.clone(),
            Decoded::Svg { first, .. } => first.clone(),
        }
    }

}

pub async fn decode(path: &Path) -> Result<(Rc<Decoded>, String), String> {
    let file = gio::File::for_path(path);
    let image = glycin::Loader::new(file)
        .load()
        .await
        .map_err(|e| e.to_string())?;
    let mime = image.mime_type().to_string();
    let frame = image.next_frame().await.map_err(|e| e.to_string())?;
    let is_svg = matches!(mime.as_str(), "image/svg+xml" | "image/svg+xml-compressed");
    let decoded = if is_svg {
        Decoded::Svg {
            nominal: (frame.width() as f64, frame.height() as f64),
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

/// Tiny LRU keyed by path: current image plus pre-decoded neighbors.
pub struct Cache {
    cap: usize,
    entries: RefCell<VecDeque<(PathBuf, Rc<Decoded>, String)>>,
}

impl Cache {
    pub fn new(cap: usize) -> Cache {
        Cache {
            cap,
            entries: RefCell::new(VecDeque::new()),
        }
    }

    pub fn get(&self, path: &Path) -> Option<(Rc<Decoded>, String)> {
        let mut entries = self.entries.borrow_mut();
        let pos = entries.iter().position(|(p, _, _)| p == path)?;
        let entry = entries.remove(pos).unwrap();
        let result = (entry.1.clone(), entry.2.clone());
        entries.push_front(entry);
        Some(result)
    }

    pub fn contains(&self, path: &Path) -> bool {
        self.entries.borrow().iter().any(|(p, _, _)| p == path)
    }

    pub fn put(&self, path: PathBuf, decoded: Rc<Decoded>, mime: String) {
        let mut entries = self.entries.borrow_mut();
        if let Some(pos) = entries.iter().position(|(p, _, _)| p == &path) {
            entries.remove(pos);
        }
        entries.push_front((path, decoded, mime));
        entries.truncate(self.cap);
    }

    /// Drop a path after the file changed on disk (rotate-save).
    pub fn invalidate(&self, path: &Path) {
        let mut entries = self.entries.borrow_mut();
        if let Some(pos) = entries.iter().position(|(p, _, _)| p == path) {
            entries.remove(pos);
        }
    }
}
