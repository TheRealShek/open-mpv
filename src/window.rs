//! Main window assembly: frameless surface with fade-in overlay
//! controls (FR-6), the single action layer every input goes through
//! (NFR-6.2), folder monitoring (FR-3.5), and the trash/undo/save
//! flows (FR-5).

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use gtk4 as gtk;

use gtk::gdk;
use gtk::gio;
use gtk::glib;
use gtk::glib::clone;
use gtk::prelude::*;

use crate::config::{self, Config, FitMode};
use crate::fileops;
use crate::folder::Folder;
use crate::loader::{self, Decoded};
use crate::player::{self, Player};
use crate::viewer::ImageView;

/// Default key → action map; config `bind=` lines override per key.
const DEFAULT_BINDS: &[(&str, &str)] = &[
    ("Right", "next"),
    // Space pauses video, flips to the next photo otherwise.
    ("space", "play-pause"),
    ("Page_Down", "next"),
    ("Left", "prev"),
    ("BackSpace", "prev"),
    ("Page_Up", "prev"),
    ("Home", "first"),
    ("End", "last"),
    ("plus", "zoom-in"),
    ("equal", "zoom-in"),
    ("KP_Add", "zoom-in"),
    ("minus", "zoom-out"),
    ("KP_Subtract", "zoom-out"),
    ("0", "zoom-fit"),
    ("1", "zoom-actual"),
    ("z", "zoom-toggle"),
    ("r", "rotate-cw"),
    ("<Shift>r", "rotate-ccw"),
    ("s", "save"),
    // Video transport (FR-9), mpv-flavored.
    ("j", "seek-back"),
    ("l", "seek-forward"),
    ("m", "mute"),
    ("Up", "volume-up"),
    ("Down", "volume-down"),
    ("Delete", "trash"),
    ("KP_Delete", "trash"),
    ("<Control>z", "undo"),
    ("f", "fullscreen"),
    ("F11", "fullscreen"),
    ("q", "close"),
    ("question", "help"),
    ("Escape", "escape"),
];

const ACTION_NAMES: &[&str] = &[
    "next",
    "prev",
    "first",
    "last",
    "zoom-in",
    "zoom-out",
    "zoom-fit",
    "zoom-actual",
    "zoom-toggle",
    "rotate-cw",
    "rotate-ccw",
    "play-pause",
    "seek-back",
    "seek-forward",
    "mute",
    "volume-up",
    "volume-down",
    "save",
    "trash",
    "undo",
    "fullscreen",
    "close",
    "help",
    "escape",
];

const TOAST_TIMEOUT: Duration = Duration::from_secs(5);
const FLASH_TIMEOUT: Duration = Duration::from_millis(1200);
const SVG_DEBOUNCE: Duration = Duration::from_millis(200);

pub struct App {
    pub win: gtk::ApplicationWindow,
    cfg: Config,
    view: ImageView,
    folder: RefCell<Option<Folder>>,
    monitor: RefCell<Option<gio::FileMonitor>>,
    showing: RefCell<Option<PathBuf>>,
    current_mime: RefCell<Option<String>>,
    current_decoded: RefCell<Option<Rc<Decoded>>>,
    cache: loader::Cache,
    /// Bumped on every image change; async work checks it before
    /// touching the UI so stale decodes/frames are dropped (NFR-1.3).
    generation: Cell<u64>,
    editable_mimes: RefCell<BTreeSet<String>>,
    /// Created on the first video (lazy GStreamer init, NFR-1.1) and
    /// reused; `None` also while videos have never been opened.
    player: RefCell<Option<Rc<Player>>>,
    pending_undo: RefCell<Option<PathBuf>>,
    presented: Cell<bool>,
    // Widgets and timers.
    status: gtk::Label,
    pos_label: gtk::Label,
    indicator: gtk::Label,
    toast_revealer: gtk::Revealer,
    toast_label: gtk::Label,
    toast_undo: gtk::Button,
    help_label: gtk::Label,
    chrome: Vec<gtk::Widget>,
    chrome_timer: TimerSlot,
    indicator_timer: TimerSlot,
    toast_timer: TimerSlot,
    svg_timer: TimerSlot,
    save_action: gio::SimpleAction,
    undo_action: gio::SimpleAction,
}

impl App {
    pub fn new(gtk_app: &gtk::Application, cfg: Config) -> Rc<App> {
        let win = gtk::ApplicationWindow::builder()
            .application(gtk_app)
            .title("open-mpv")
            .decorated(false)
            .build();

        apply_css(&cfg.background);

        let view = ImageView::default();
        view.set_default_fit_actual(cfg.fit == FitMode::Actual);

        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(&view));

        // Empty / error state (FR-1.4, FR-1.5).
        let status = gtk::Label::new(Some("Open an image…"));
        status.set_halign(gtk::Align::Center);
        status.set_valign(gtk::Align::Center);
        status.set_wrap(true);
        status.set_justify(gtk::Justification::Center);
        status.add_css_class("status");
        overlay.add_overlay(&status);

        // Navigation arrows (FR-3.1).
        let prev_btn = osd_button("go-previous-symbolic", "win.prev", "Previous image");
        prev_btn.set_halign(gtk::Align::Start);
        prev_btn.set_valign(gtk::Align::Center);
        overlay.add_overlay(&prev_btn);
        let next_btn = osd_button("go-next-symbolic", "win.next", "Next image");
        next_btn.set_halign(gtk::Align::End);
        next_btn.set_valign(gtk::Align::Center);
        overlay.add_overlay(&next_btn);

        // Close button (FR-6.7).
        let close_btn = osd_button("window-close-symbolic", "win.close", "Close");
        close_btn.set_halign(gtk::Align::End);
        close_btn.set_valign(gtk::Align::Start);
        overlay.add_overlay(&close_btn);

        // Bottom control bar (FR-6.2).
        let bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        bar.add_css_class("osd-bar");
        bar.set_halign(gtk::Align::Center);
        bar.set_valign(gtk::Align::End);
        let pos_label = gtk::Label::new(None);
        pos_label.add_css_class("dim");
        bar.append(&pos_label);
        for (icon, action, tip) in [
            (
                "object-rotate-left-symbolic",
                "win.rotate-ccw",
                "Rotate left",
            ),
            (
                "object-rotate-right-symbolic",
                "win.rotate-cw",
                "Rotate right",
            ),
            (
                "document-save-symbolic",
                "win.save",
                "Save rotation to file",
            ),
            ("user-trash-symbolic", "win.trash", "Move to trash"),
            ("view-fullscreen-symbolic", "win.fullscreen", "Fullscreen"),
        ] {
            bar.append(&bar_button(icon, action, tip));
        }
        overlay.add_overlay(&bar);

        // Zoom / edge-cue indicator (FR-4.4, FR-3.3).
        let indicator = gtk::Label::new(None);
        indicator.add_css_class("indicator");
        indicator.add_css_class("invisible");
        indicator.set_halign(gtk::Align::Start);
        indicator.set_valign(gtk::Align::Start);
        indicator.set_can_target(false);
        overlay.add_overlay(&indicator);

        // Toast with undo (FR-5.2).
        let toast_label = gtk::Label::new(None);
        let toast_undo = gtk::Button::with_label("Undo");
        toast_undo.set_action_name(Some("win.undo"));
        let toast_box = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        toast_box.add_css_class("toast");
        toast_box.append(&toast_label);
        toast_box.append(&toast_undo);
        let toast_revealer = gtk::Revealer::builder()
            .transition_type(gtk::RevealerTransitionType::Crossfade)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::End)
            .margin_bottom(56)
            .child(&toast_box)
            .build();
        overlay.add_overlay(&toast_revealer);

        // Keybinding cheat sheet (NFR-5.2).
        let help_label = gtk::Label::new(None);
        help_label.add_css_class("help");
        help_label.set_halign(gtk::Align::Center);
        help_label.set_valign(gtk::Align::Center);
        help_label.set_visible(false);
        overlay.add_overlay(&help_label);

        win.set_child(Some(&overlay));

        for w in [
            prev_btn.upcast_ref::<gtk::Widget>(),
            next_btn.upcast_ref(),
            close_btn.upcast_ref(),
            bar.upcast_ref(),
        ] {
            w.add_css_class("chrome");
            w.add_css_class("invisible");
            w.set_can_target(false);
        }

        let app = Rc::new(App {
            win: win.clone(),
            view: view.clone(),
            folder: RefCell::new(None),
            monitor: RefCell::new(None),
            showing: RefCell::new(None),
            current_mime: RefCell::new(None),
            current_decoded: RefCell::new(None),
            cache: loader::Cache::new(3, cfg.cache_budget_mb as usize * 1024 * 1024),
            generation: Cell::new(0),
            editable_mimes: RefCell::new(BTreeSet::new()),
            player: RefCell::new(None),
            pending_undo: RefCell::new(None),
            presented: Cell::new(false),
            status,
            pos_label,
            indicator,
            toast_revealer,
            toast_label,
            toast_undo,
            help_label,
            chrome: vec![
                prev_btn.upcast(),
                next_btn.upcast(),
                close_btn.upcast(),
                bar.upcast(),
            ],
            chrome_timer: TimerSlot::default(),
            indicator_timer: TimerSlot::default(),
            toast_timer: TimerSlot::default(),
            svg_timer: TimerSlot::default(),
            save_action: gio::SimpleAction::new("save", None),
            undo_action: gio::SimpleAction::new("undo", None),
            cfg,
        });

        app.setup_actions(gtk_app);
        app.setup_controllers();
        app.build_help(gtk_app);

        // Query which formats the sandboxed editors can rewrite.
        glib::spawn_future_local(clone!(
            #[strong]
            app,
            async move {
                let mimes = fileops::editable_mime_types().await;
                *app.editable_mimes.borrow_mut() = mimes;
                app.update_save_enabled();
            }
        ));

        app.view.connect_view_changed(clone!(
            #[strong]
            app,
            move |zoom_percent| {
                app.flash(&format!("{zoom_percent:.0}%"));
                app.update_save_enabled();
                app.schedule_svg_rerender();
            }
        ));
        app.view.connect_navigate(clone!(
            #[strong]
            app,
            move |dir| app.navigate(dir)
        ));

        app
    }

    // ----- opening ------------------------------------------------------

    /// Entry point for CLI, desktop launch, single-instance forwards and
    /// drag-and-drop (FR-1).
    pub fn open_path(self: &Rc<Self>, path: &Path) {
        let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        crate::applog!("open: {}", path.display());
        if path.is_dir() {
            self.open_folder(&path);
        } else {
            let Some(dir) = path.parent().map(Path::to_path_buf) else {
                self.show_error(&path, "path has no parent directory");
                return;
            };
            match Folder::scan(&dir, self.cfg.sort) {
                Ok(folder) => {
                    let idx = folder.index_of(&path);
                    self.install_folder(folder, &dir);
                    match idx {
                        Some(idx) => self.show_index(idx),
                        None => self.show_error(&path, "not a supported image, or unreadable"),
                    }
                }
                Err(e) => self.show_error(&path, &format!("cannot read directory: {e}")),
            }
        }
    }

    fn open_folder(self: &Rc<Self>, dir: &Path) {
        match Folder::scan(dir, self.cfg.sort) {
            Ok(folder) if !folder.is_empty() => {
                self.install_folder(folder, dir);
                self.show_index(0);
            }
            Ok(folder) => {
                self.install_folder(folder, dir);
                self.show_error(dir, "no supported images in this folder");
            }
            Err(e) => self.show_error(dir, &format!("cannot read directory: {e}")),
        }
    }

    fn install_folder(self: &Rc<Self>, folder: Folder, dir: &Path) {
        crate::applog!("folder: {} with {} images", dir.display(), folder.len());
        *self.folder.borrow_mut() = Some(folder);
        let monitor = gio::File::for_path(dir)
            .monitor_directory(gio::FileMonitorFlags::WATCH_MOVES, gio::Cancellable::NONE)
            .ok();
        if let Some(m) = &monitor {
            m.connect_changed(clone!(
                #[strong(rename_to = app)]
                self,
                move |_, file, other, event| app.on_fs_event(file, other, event)
            ));
        }
        // Dropping the previous monitor cancels it.
        *self.monitor.borrow_mut() = monitor;
    }

    /// Present the window for states with no image to size from.
    pub fn present_default(&self) {
        if !self.presented.get() {
            self.win.set_default_size(800, 600);
            self.presented.set(true);
        }
        self.win.present();
    }

    // ----- showing images ----------------------------------------------

    fn show_index(self: &Rc<Self>, idx: usize) {
        let Some(path) = self
            .folder
            .borrow()
            .as_ref()
            .and_then(|f| f.get(idx))
            .map(Path::to_path_buf)
        else {
            return;
        };
        *self.showing.borrow_mut() = Some(path.clone());
        self.cache.pin(&path);
        self.win.set_title(
            path.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .as_deref(),
        );
        self.update_pos_label();
        let generation = self.generation.get() + 1;
        self.generation.set(generation);

        if config::is_video(&path) {
            self.show_video(&path);
            self.preload_neighbors(idx);
            return;
        }
        // Leaving a video for an image: silence and free the decoder.
        self.stop_video();

        if let Some((decoded, mime)) = self.cache.get(&path) {
            crate::applog!("show: {} (cache hit)", path.display());
            self.apply_decoded(decoded, mime, generation);
        } else {
            glib::spawn_future_local(clone!(
                #[strong(rename_to = app)]
                self,
                async move {
                    match loader::decode(&path).await {
                        Ok((decoded, mime)) => {
                            app.cache.put(path.clone(), decoded.clone(), mime.clone());
                            if app.generation.get() == generation {
                                app.apply_decoded(decoded, mime, generation);
                            } else {
                                crate::applog!(
                                    "show: {} superseded, kept in cache",
                                    path.display()
                                );
                            }
                        }
                        Err(e) => {
                            eprintln!("open-mpv: error: {}: {e}", path.display());
                            if app.generation.get() == generation {
                                app.show_error(&path, &e);
                            }
                        }
                    }
                }
            ));
        }
        self.preload_neighbors(idx);
    }

    // ----- showing videos (FR-9) ---------------------------------------

    /// Lazily build the shared player; `None` if the pipeline cannot be
    /// assembled (missing plugins) — a routine state, not a panic.
    fn player(self: &Rc<Self>) -> Option<Rc<Player>> {
        if let Some(p) = self.player.borrow().as_ref() {
            return Some(p.clone());
        }
        let weak = Rc::downgrade(self);
        match Player::new(move |event| {
            if let Some(app) = weak.upgrade() {
                app.on_player_event(event);
            }
        }) {
            Ok(p) => {
                let p = Rc::new(p);
                *self.player.borrow_mut() = Some(p.clone());
                Some(p)
            }
            Err(e) => {
                eprintln!("open-mpv: error: {e}");
                None
            }
        }
    }

    fn show_video(self: &Rc<Self>, path: &Path) {
        let Some(player) = self.player() else {
            self.show_error(path, "video playback unavailable (GStreamer)");
            return;
        };
        self.status.set_visible(false);
        *self.current_decoded.borrow_mut() = None;
        *self.current_mime.borrow_mut() = None;
        self.view.show_paintable(player.paintable(), None);
        if let Err(e) = player.play(path) {
            self.show_error(path, &e);
            return;
        }
        crate::applog!("play: {}", path.display());
        self.update_save_enabled();
        // Dimensions arrive with preroll; until then present at the
        // default size rather than blocking on the pipeline.
        self.present_default();
    }

    fn stop_video(&self) {
        if let Some(p) = self.player.borrow().as_ref() {
            p.stop();
        }
    }

    fn is_video_showing(&self) -> bool {
        self.showing
            .borrow()
            .as_deref()
            .is_some_and(config::is_video)
    }

    fn on_player_event(self: &Rc<Self>, event: player::Event) {
        match event {
            player::Event::EndOfStream => {
                if self.is_video_showing() {
                    // Loop like animated images do (FR-9.3).
                    if let Some(p) = self.player.borrow().as_ref() {
                        p.rewind();
                    }
                }
            }
            player::Event::Error(message) => {
                let path = self.showing.borrow().clone();
                self.stop_video();
                if let Some(path) = path {
                    self.show_error(&path, &message);
                }
            }
        }
    }

    /// Run `f` on the player when a video is on screen; used by the
    /// transport actions so they are no-ops on images.
    fn with_video(self: &Rc<Self>, f: impl FnOnce(&Player)) {
        if !self.is_video_showing() {
            return;
        }
        let player = self.player.borrow().clone();
        if let Some(p) = player {
            f(&p);
        }
    }

    fn apply_decoded(self: &Rc<Self>, decoded: Rc<Decoded>, mime: String, generation: u64) {
        self.status.set_visible(false);
        *self.current_mime.borrow_mut() = Some(mime);
        *self.current_decoded.borrow_mut() = Some(decoded.clone());
        let texture = decoded.first_texture();
        let size = match &*decoded {
            Decoded::Svg { nominal, .. } => {
                self.view.show_texture(texture, Some(*nominal));
                *nominal
            }
            _ => {
                self.view.show_texture(texture.clone(), None);
                (texture.width() as f64, texture.height() as f64)
            }
        };
        if let Decoded::Animated { .. } = &*decoded {
            self.spawn_animation(decoded, generation);
        }
        self.update_save_enabled();
        self.maybe_first_present(size);
    }

    fn spawn_animation(self: &Rc<Self>, decoded: Rc<Decoded>, generation: u64) {
        glib::spawn_future_local(clone!(
            #[strong(rename_to = app)]
            self,
            async move {
                let Decoded::Animated { image, .. } = &*decoded else {
                    return;
                };
                loop {
                    let Ok(frame) = image.next_frame().await else {
                        break;
                    };
                    if app.generation.get() != generation {
                        break;
                    }
                    app.view.update_texture(frame.texture());
                    let delay = frame.delay().unwrap_or(Duration::from_millis(100));
                    glib::timeout_future(delay).await;
                    if app.generation.get() != generation {
                        break;
                    }
                }
            }
        ));
    }

    /// Re-render the current SVG at the displayed resolution once zoom
    /// settles, so vectors stay sharp at any zoom (FR-2.3).
    fn schedule_svg_rerender(self: &Rc<Self>) {
        let is_svg = matches!(
            self.current_decoded.borrow().as_deref(),
            Some(Decoded::Svg { .. })
        );
        if !is_svg {
            return;
        }
        let generation = self.generation.get();
        reset_timer(
            &self.svg_timer,
            SVG_DEBOUNCE,
            clone!(
                #[strong(rename_to = app)]
                self,
                move || {
                    let decoded = app.current_decoded.borrow().clone();
                    let Some(decoded) = decoded else { return };
                    let Decoded::Svg { nominal, .. } = &*decoded else {
                        return;
                    };
                    let zoom = app.view.zoom_percent() / 100.0;
                    let w = (nominal.0 * zoom)
                        .round()
                        .clamp(1.0, loader::SVG_RENDER_MAX as f64);
                    let h = (nominal.1 * zoom)
                        .round()
                        .clamp(1.0, loader::SVG_RENDER_MAX as f64);
                    glib::spawn_future_local(clone!(
                        #[strong]
                        app,
                        async move {
                            let Decoded::Svg { image, .. } = &*decoded else {
                                return;
                            };
                            let started = std::time::Instant::now();
                            let request = glycin::FrameRequest::new().scale(w as u32, h as u32);
                            if let Ok(frame) = image.specific_frame(request).await
                                && app.generation.get() == generation
                            {
                                crate::applog!(
                                    "svg: re-rendered {}x{} in {:.1} ms",
                                    w,
                                    h,
                                    started.elapsed().as_secs_f64() * 1000.0
                                );
                                app.view.update_texture(frame.texture());
                            }
                        }
                    ));
                }
            ),
        );
    }

    fn preload_neighbors(self: &Rc<Self>, idx: usize) {
        let neighbors: Vec<PathBuf> = {
            let folder = self.folder.borrow();
            let Some(folder) = folder.as_ref() else {
                return;
            };
            [idx.checked_sub(1), Some(idx + 1)]
                .into_iter()
                .flatten()
                .filter_map(|i| folder.get(i))
                // Videos are streamed, never pre-decoded (FR-9.4).
                .filter(|p| !config::is_video(p))
                .map(Path::to_path_buf)
                .collect()
        };
        for path in neighbors {
            if self.cache.contains(&path) {
                continue;
            }
            glib::spawn_future_local(clone!(
                #[strong(rename_to = app)]
                self,
                async move {
                    if let Ok((decoded, mime)) = loader::decode(&path).await {
                        crate::applog!("preload: {}", path.display());
                        app.cache.put(path, decoded, mime);
                    }
                }
            ));
        }
    }

    fn show_error(self: &Rc<Self>, path: &Path, message: &str) {
        self.generation.set(self.generation.get() + 1);
        self.stop_video();
        self.view.clear();
        *self.current_decoded.borrow_mut() = None;
        *self.current_mime.borrow_mut() = None;
        self.status.set_text(&format!(
            "{}\n\n{message}",
            path.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string())
        ));
        self.status.set_visible(true);
        self.update_save_enabled();
        self.present_default();
    }

    fn empty_state(self: &Rc<Self>, message: &str) {
        self.generation.set(self.generation.get() + 1);
        self.stop_video();
        self.view.clear();
        *self.current_decoded.borrow_mut() = None;
        *self.current_mime.borrow_mut() = None;
        *self.showing.borrow_mut() = None;
        self.win.set_title(Some("open-mpv"));
        self.status.set_text(message);
        self.status.set_visible(true);
        self.update_pos_label();
        self.update_save_enabled();
        self.present_default();
    }

    /// First image decides the initial window size: the image at 100%,
    /// capped at 85% of the monitor work area (FR-6.6).
    fn maybe_first_present(&self, size: (f64, f64)) {
        if !self.presented.get() {
            let (mut mw, mut mh) = (1920.0f64, 1080.0f64);
            if let Some(display) = gdk::Display::default()
                && let Some(monitor) = display.monitors().item(0).and_downcast::<gdk::Monitor>()
            {
                let geo = monitor.geometry();
                (mw, mh) = (geo.width() as f64, geo.height() as f64);
            }
            let (cap_w, cap_h) = (mw * 0.85, mh * 0.85);
            let s = (cap_w / size.0).min(cap_h / size.1).min(1.0);
            self.win.set_default_size(
                (size.0 * s).round().max(200.0) as i32,
                (size.1 * s).round().max(150.0) as i32,
            );
            self.presented.set(true);
            // The cold-start metric (NFR-1.1): launch → first present.
            crate::applog!("first present: window shown");
        }
        self.win.present();
    }

    // ----- navigation ---------------------------------------------------

    fn current_index(&self) -> Option<usize> {
        let showing = self.showing.borrow();
        let folder = self.folder.borrow();
        folder.as_ref()?.index_of(showing.as_deref()?)
    }

    fn navigate(self: &Rc<Self>, delta: i32) {
        let target = {
            let folder = self.folder.borrow();
            let Some(folder) = folder.as_ref() else {
                return;
            };
            let Some(current) = self.current_index() else {
                return;
            };
            if delta > 0 {
                folder.next(current, self.cfg.wrap)
            } else {
                folder.prev(current, self.cfg.wrap)
            }
        };
        match target {
            Some(idx) => self.show_index(idx),
            None => self.flash(if delta > 0 {
                "Last image"
            } else {
                "First image"
            }),
        }
    }

    fn update_pos_label(&self) {
        let text = {
            let folder = self.folder.borrow();
            match (folder.as_ref(), self.current_index()) {
                (Some(f), Some(i)) if !f.is_empty() => format!("{} / {}", i + 1, f.len()),
                _ => String::new(),
            }
        };
        self.pos_label.set_text(&text);
    }

    // ----- filesystem events (FR-3.5) ----------------------------------

    fn on_fs_event(
        self: &Rc<Self>,
        file: &gio::File,
        other: Option<&gio::File>,
        event: gio::FileMonitorEvent,
    ) {
        use gio::FileMonitorEvent as E;
        let mut showing_vanished = false;
        {
            let mut folder = self.folder.borrow_mut();
            let Some(folder) = folder.as_mut() else {
                return;
            };
            let showing = self.showing.borrow().clone();
            match event {
                E::Created | E::MovedIn => {
                    if let Some(p) = file.path() {
                        folder.insert(&p);
                    }
                }
                E::Deleted | E::MovedOut => {
                    if let Some(p) = file.path() {
                        folder.remove(&p);
                        if showing.as_deref() == Some(p.as_path()) {
                            showing_vanished = true;
                        }
                    }
                }
                E::Renamed => {
                    if let Some(p) = file.path() {
                        folder.remove(&p);
                        if let Some(new) = other.and_then(|f| f.path()) {
                            folder.insert(&new);
                            if showing.as_deref() == Some(p.as_path()) {
                                *self.showing.borrow_mut() = Some(new.clone());
                                self.win.set_title(
                                    new.file_name()
                                        .map(|n| n.to_string_lossy().into_owned())
                                        .as_deref(),
                                );
                            }
                        }
                    }
                }
                _ => return,
            }
        }
        crate::applog!(
            "fs event: {event:?} {}{}",
            file.path().unwrap_or_default().display(),
            if showing_vanished {
                " (current image vanished)"
            } else {
                ""
            }
        );
        if showing_vanished {
            self.after_current_removed();
        }
        self.update_pos_label();
    }

    /// The image on screen disappeared (external delete/move): show the
    /// nearest remaining one, or the empty state.
    fn after_current_removed(self: &Rc<Self>) {
        let len = self.folder.borrow().as_ref().map_or(0, Folder::len);
        if len == 0 {
            self.empty_state("No images left in this folder");
        } else {
            // Index of the removed file is gone; land on the same slot.
            let idx = self
                .showing
                .borrow()
                .as_deref()
                .and_then(|p| self.folder.borrow().as_ref().and_then(|f| f.index_of(p)))
                .unwrap_or(0);
            self.show_index(idx.min(len - 1));
        }
    }

    // ----- file operations (FR-5) --------------------------------------

    fn trash_current(self: &Rc<Self>) {
        let Some(path) = self.showing.borrow().clone() else {
            return;
        };
        // Release the file before it moves to trash.
        if config::is_video(&path) {
            self.stop_video();
        }
        let idx = self.current_index().unwrap_or(0);
        glib::spawn_future_local(clone!(
            #[strong(rename_to = app)]
            self,
            async move {
                let started = std::time::Instant::now();
                match fileops::trash(&path).await {
                    Ok(()) => {
                        crate::applog!(
                            "trash: {} in {:.1} ms",
                            path.display(),
                            started.elapsed().as_secs_f64() * 1000.0
                        );
                        app.cache.invalidate(&path);
                        let len = {
                            let mut folder = app.folder.borrow_mut();
                            if let Some(f) = folder.as_mut() {
                                f.remove(&path);
                                f.len()
                            } else {
                                0
                            }
                        };
                        *app.pending_undo.borrow_mut() = Some(path);
                        if len == 0 {
                            app.empty_state("No images left in this folder");
                        } else {
                            app.show_index(idx.min(len - 1));
                        }
                        app.show_toast("Moved to trash", true);
                    }
                    Err(e) => {
                        eprintln!("open-mpv: error: {e}");
                        app.show_toast(&e, false);
                    }
                }
            }
        ));
    }

    fn undo_trash(self: &Rc<Self>) {
        let Some(path) = self.pending_undo.borrow_mut().take() else {
            return;
        };
        self.hide_toast();
        glib::spawn_future_local(clone!(
            #[strong(rename_to = app)]
            self,
            async move {
                match fileops::restore(&path).await {
                    Ok(()) => {
                        crate::applog!("restore: {}", path.display());
                        let idx = {
                            let mut folder = app.folder.borrow_mut();
                            match folder.as_mut() {
                                // insert() returns None if the monitor
                                // already re-added it (gotcha: dedup).
                                Some(f) => f.insert(&path).or_else(|| f.index_of(&path)),
                                None => None,
                            }
                        };
                        if let Some(idx) = idx {
                            app.show_index(idx);
                        }
                    }
                    Err(e) => {
                        eprintln!("open-mpv: error: {e}");
                        app.show_toast(&e, false);
                    }
                }
            }
        ));
    }

    fn save_rotation(self: &Rc<Self>) {
        let rotation = self.view.rotation();
        let Some(path) = self.showing.borrow().clone() else {
            return;
        };
        if rotation == 0 {
            return;
        }
        self.save_action.set_enabled(false);
        glib::spawn_future_local(clone!(
            #[strong(rename_to = app)]
            self,
            async move {
                let started = std::time::Instant::now();
                match fileops::save_rotation(&path, rotation).await {
                    Ok(()) => {
                        crate::applog!(
                            "save-rotation: {} ({}°) in {:.1} ms",
                            path.display(),
                            rotation as u32 * 90,
                            started.elapsed().as_secs_f64() * 1000.0
                        );
                        app.cache.invalidate(&path);
                        app.flash("Saved");
                        // Reload: the file now carries the rotation.
                        if let Some(idx) = app.current_index() {
                            app.show_index(idx);
                        }
                    }
                    Err(e) => {
                        eprintln!("open-mpv: error: {e}");
                        app.show_toast(&e, false);
                        app.update_save_enabled();
                    }
                }
            }
        ));
    }

    /// Rotate-save is offered only where it is safe and meaningful:
    /// still raster images in formats the sandboxed editor can rewrite;
    /// SVG and animations stay view-only (FR-5.4).
    fn update_save_enabled(&self) {
        let editable = match (
            self.current_decoded.borrow().as_deref(),
            self.current_mime.borrow().as_deref(),
        ) {
            (Some(Decoded::Static { .. }), Some(mime)) => {
                self.editable_mimes.borrow().contains(mime)
            }
            _ => false,
        };
        self.save_action
            .set_enabled(editable && self.view.rotation() != 0);
    }

    // ----- overlay chrome, toasts, indicator (FR-6.2) -------------------

    fn show_chrome(self: &Rc<Self>) {
        for w in &self.chrome {
            w.remove_css_class("invisible");
            w.set_can_target(true);
        }
        let timeout = Duration::from_secs_f64(self.cfg.overlay_timeout.max(0.2));
        reset_timer(
            &self.chrome_timer,
            timeout,
            clone!(
                #[strong(rename_to = app)]
                self,
                move || app.hide_chrome()
            ),
        );
    }

    fn hide_chrome(&self) {
        for w in &self.chrome {
            w.add_css_class("invisible");
            w.set_can_target(false);
        }
    }

    /// Flash the current video position after a seek (FR-9.5).
    fn flash_progress(self: &Rc<Self>) {
        let mut progress = None;
        self.with_video(|p| progress = p.progress());
        if let Some((pos, dur)) = progress {
            self.flash(&format!("{} / {}", format_time(pos), format_time(dur)));
        }
    }

    fn change_volume(self: &Rc<Self>, delta: f64) {
        let mut vol = None;
        self.with_video(|p| vol = Some(p.add_volume(delta)));
        if let Some(v) = vol {
            self.flash(&format!("Volume {:.0}%", v * 100.0));
        }
    }

    /// Brief top-left indicator: zoom level and edge cues (FR-4.4, FR-3.3).
    fn flash(self: &Rc<Self>, text: &str) {
        self.indicator.set_text(text);
        self.indicator.remove_css_class("invisible");
        reset_timer(
            &self.indicator_timer,
            FLASH_TIMEOUT,
            clone!(
                #[strong(rename_to = app)]
                self,
                move || {
                    app.indicator.add_css_class("invisible");
                }
            ),
        );
    }

    fn show_toast(self: &Rc<Self>, text: &str, with_undo: bool) {
        self.toast_label.set_text(text);
        self.toast_undo.set_visible(with_undo);
        self.undo_action.set_enabled(with_undo);
        self.toast_revealer.set_reveal_child(true);
        reset_timer(
            &self.toast_timer,
            TOAST_TIMEOUT,
            clone!(
                #[strong(rename_to = app)]
                self,
                move || {
                    app.hide_toast();
                    // The undo window lapses; the file stays in trash.
                    *app.pending_undo.borrow_mut() = None;
                }
            ),
        );
    }

    fn hide_toast(&self) {
        self.toast_revealer.set_reveal_child(false);
        self.undo_action.set_enabled(false);
    }

    // ----- actions and input -------------------------------------------

    fn setup_actions(self: &Rc<Self>, gtk_app: &gtk::Application) {
        type Handler = Box<dyn Fn(&Rc<App>)>;
        let add = |name: &str, f: Handler| {
            let action = gio::SimpleAction::new(name, None);
            let app = self.clone();
            action.connect_activate(move |_, _| f(&app));
            self.win.add_action(&action);
            action
        };

        add("next", Box::new(|a| a.navigate(1)));
        add("prev", Box::new(|a| a.navigate(-1)));
        add(
            "first",
            Box::new(|a| {
                if a.folder.borrow().as_ref().is_some_and(|f| !f.is_empty()) {
                    a.show_index(0);
                }
            }),
        );
        add(
            "last",
            Box::new(|a| {
                let len = a.folder.borrow().as_ref().map_or(0, Folder::len);
                if len > 0 {
                    a.show_index(len - 1);
                }
            }),
        );
        add("zoom-in", Box::new(|a| a.view.zoom_by(1.25, None)));
        add("zoom-out", Box::new(|a| a.view.zoom_by(0.8, None)));
        add("zoom-fit", Box::new(|a| a.view.zoom_fit()));
        add("zoom-actual", Box::new(|a| a.view.zoom_to(1.0, None)));
        add("zoom-toggle", Box::new(|a| a.view.toggle_fit_actual()));
        add("rotate-cw", Box::new(|a| a.view.rotate_view(1)));
        add("rotate-ccw", Box::new(|a| a.view.rotate_view(-1)));
        add(
            "play-pause",
            Box::new(|a| {
                if a.is_video_showing() {
                    let mut playing = None;
                    a.with_video(|p| playing = Some(p.toggle_pause()));
                    match playing {
                        Some(true) => a.flash("Play"),
                        Some(false) => a.flash("Paused"),
                        None => {}
                    }
                } else {
                    // Space keeps its photo-flipping habit on images.
                    a.navigate(1);
                }
            }),
        );
        add(
            "seek-back",
            Box::new(|a| {
                a.with_video(|p| p.seek_by(-5.0));
                a.flash_progress();
            }),
        );
        add(
            "seek-forward",
            Box::new(|a| {
                a.with_video(|p| p.seek_by(5.0));
                a.flash_progress();
            }),
        );
        add(
            "mute",
            Box::new(|a| {
                let mut muted = None;
                a.with_video(|p| muted = Some(p.toggle_mute()));
                match muted {
                    Some(true) => a.flash("Muted"),
                    Some(false) => a.flash("Sound on"),
                    None => {}
                }
            }),
        );
        add("volume-up", Box::new(|a| a.change_volume(0.1)));
        add("volume-down", Box::new(|a| a.change_volume(-0.1)));
        add("trash", Box::new(|a| a.trash_current()));
        add(
            "fullscreen",
            Box::new(|a| {
                if a.win.is_fullscreen() {
                    a.win.unfullscreen();
                } else {
                    a.win.fullscreen();
                }
            }),
        );
        add("close", Box::new(|a| a.win.close()));
        add(
            "help",
            Box::new(|a| a.help_label.set_visible(!a.help_label.is_visible())),
        );
        // Escape: help → fullscreen → close (FR-6.7).
        add(
            "escape",
            Box::new(|a| {
                if a.help_label.is_visible() {
                    a.help_label.set_visible(false);
                } else if a.win.is_fullscreen() {
                    a.win.unfullscreen();
                } else {
                    a.win.close();
                }
            }),
        );

        let app = self.clone();
        self.save_action.set_enabled(false);
        self.save_action
            .connect_activate(move |_, _| app.save_rotation());
        self.win.add_action(&self.save_action);

        let app = self.clone();
        self.undo_action.set_enabled(false);
        self.undo_action
            .connect_activate(move |_, _| app.undo_trash());
        self.win.add_action(&self.undo_action);

        // Defaults merged with user binds (FR-8.2); a user bind takes
        // the key over from the default action.
        let mut key_to_action: BTreeMap<String, String> = DEFAULT_BINDS
            .iter()
            .map(|(k, a)| (k.to_string(), a.to_string()))
            .collect();
        for (key, action) in &self.cfg.binds {
            if !ACTION_NAMES.contains(&action.as_str()) {
                eprintln!("open-mpv: config: unknown action `{action}` for bind `{key}`");
                continue;
            }
            if gtk::accelerator_parse(key).is_none() {
                eprintln!("open-mpv: config: cannot parse key `{key}`");
                continue;
            }
            key_to_action.insert(key.clone(), action.clone());
        }
        let mut action_to_keys: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for (key, action) in &key_to_action {
            action_to_keys
                .entry(action.as_str())
                .or_default()
                .push(key.as_str());
        }
        for (action, keys) in &action_to_keys {
            gtk_app.set_accels_for_action(&format!("win.{action}"), keys);
        }
    }

    fn build_help(&self, gtk_app: &gtk::Application) {
        let mut lines = vec!["<b>Keys</b>".to_string(), String::new()];
        for action in ACTION_NAMES {
            if *action == "escape" {
                continue;
            }
            let accels = gtk_app.accels_for_action(&format!("win.{action}"));
            if accels.is_empty() {
                continue;
            }
            let keys: Vec<String> = accels.iter().map(|a| a.to_string()).collect();
            lines.push(format!(
                "<tt>{:<24}</tt> {}",
                glib::markup_escape_text(&keys.join(", ")),
                action
            ));
        }
        lines.push(String::new());
        lines.push("<tt>Escape</tt> leaves fullscreen, then closes".to_string());
        lines.push("Scroll: zoom · horizontal scroll: navigate".to_string());
        lines.push("Drag: pan (zoomed) or move window · double-click: fullscreen".to_string());
        self.help_label.set_markup(&lines.join("\n"));
    }

    fn setup_controllers(self: &Rc<Self>) {
        // Mouse movement reveals the chrome (FR-6.2); keyboard-only use
        // never shows it.
        let motion = gtk::EventControllerMotion::new();
        motion.connect_motion(clone!(
            #[strong(rename_to = app)]
            self,
            move |_, _, _| app.show_chrome()
        ));
        motion.connect_leave(clone!(
            #[strong(rename_to = app)]
            self,
            move |_| app.hide_chrome()
        ));
        self.win.add_controller(motion);

        // Double-click: fullscreen. Middle-click: fit/100% toggle (FR-4.3).
        let click = gtk::GestureClick::new();
        click.set_button(gdk::BUTTON_PRIMARY);
        click.connect_pressed(clone!(
            #[strong(rename_to = app)]
            self,
            move |_, n_press, _, _| {
                if n_press == 2 {
                    WidgetExt::activate_action(&app.win, "win.fullscreen", None).ok();
                }
            }
        ));
        self.win.add_controller(click);
        let middle = gtk::GestureClick::new();
        middle.set_button(gdk::BUTTON_MIDDLE);
        middle.connect_pressed(clone!(
            #[strong(rename_to = app)]
            self,
            move |_, _, _, _| app.view.toggle_fit_actual()
        ));
        self.win.add_controller(middle);

        // Dragging the (non-pannable) image moves the window (FR-6.4).
        // The viewer's pan gesture claims the drag first when zoomed in.
        let drag = gtk::GestureDrag::new();
        let began = Rc::new(Cell::new(false));
        drag.connect_drag_begin(clone!(
            #[strong]
            began,
            move |_, _, _| began.set(false)
        ));
        drag.connect_drag_update(clone!(
            #[strong(rename_to = app)]
            self,
            #[strong]
            began,
            move |gesture, dx, dy| {
                if began.get() || (dx * dx + dy * dy) < 36.0 {
                    return;
                }
                let Some(surface) = app.win.surface() else {
                    return;
                };
                let Ok(toplevel) = surface.downcast::<gdk::Toplevel>() else {
                    return;
                };
                let Some(device) = gesture.device() else {
                    return;
                };
                let (sx, sy) = gesture.start_point().unwrap_or((0.0, 0.0));
                began.set(true);
                gesture.set_state(gtk::EventSequenceState::Claimed);
                toplevel.begin_move(
                    &device,
                    gdk::BUTTON_PRIMARY as i32,
                    sx,
                    sy,
                    gesture.current_event_time(),
                );
            }
        ));
        self.win.add_controller(drag);

        // Drag-and-drop a file onto the window (FR-1.5).
        let drop = gtk::DropTarget::new(gio::File::static_type(), gdk::DragAction::COPY);
        drop.connect_drop(clone!(
            #[strong(rename_to = app)]
            self,
            move |_, value, _, _| {
                if let Ok(file) = value.get::<gio::File>()
                    && let Some(path) = file.path()
                {
                    app.open_path(&path);
                    return true;
                }
                false
            }
        ));
        self.win.add_controller(drop);
    }
}

// ----- helpers ----------------------------------------------------------

fn osd_button(icon: &str, action: &str, tooltip: &str) -> gtk::Button {
    let b = gtk::Button::from_icon_name(icon);
    b.set_action_name(Some(action));
    b.set_tooltip_text(Some(tooltip));
    b.add_css_class("osd-btn");
    b.set_margin_start(12);
    b.set_margin_end(12);
    b.set_margin_top(12);
    b.set_margin_bottom(12);
    b
}

fn bar_button(icon: &str, action: &str, tooltip: &str) -> gtk::Button {
    let b = gtk::Button::from_icon_name(icon);
    b.set_action_name(Some(action));
    b.set_tooltip_text(Some(tooltip));
    b.add_css_class("flat");
    b
}

fn apply_css(background: &str) {
    let css = format!(
        r#"
        window {{ background-color: {background}; }}
        .chrome {{ transition: opacity 200ms ease; opacity: 1; }}
        .chrome.invisible {{ opacity: 0; }}
        .indicator {{ transition: opacity 200ms ease; opacity: 1;
            background-color: rgba(0, 0, 0, 0.55); color: #eeeeee;
            padding: 4px 12px; margin: 12px; border-radius: 6px; }}
        .indicator.invisible {{ opacity: 0; }}
        .osd-btn, .osd-bar {{ background-color: rgba(0, 0, 0, 0.55);
            color: #eeeeee; border-radius: 8px; }}
        .osd-bar {{ padding: 4px 10px; margin: 12px; }}
        .osd-bar label {{ margin-right: 8px; }}
        .toast {{ background-color: rgba(25, 25, 25, 0.92); color: #eeeeee;
            padding: 8px 14px; border-radius: 8px; }}
        .status {{ color: #999999; font-size: 1.1em; }}
        .help {{ background-color: rgba(0, 0, 0, 0.85); color: #dddddd;
            padding: 18px 24px; border-radius: 10px; }}
        "#
    );
    let provider = gtk::CssProvider::new();
    provider.load_from_string(&css);
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}

/// `M:SS`, or `H:MM:SS` from the first hour (FR-9.5).
fn format_time(secs: f64) -> String {
    let s = secs.max(0.0).round() as u64;
    let (h, m, s) = (s / 3600, (s % 3600) / 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// One-shot timer slot where scheduling again supersedes the pending
/// timer. The slot is cleared from inside the callback because a fired
/// source auto-removes — removing its id later would log a critical.
#[derive(Default, Clone)]
struct TimerSlot(Rc<RefCell<Option<glib::SourceId>>>);

fn reset_timer(slot: &TimerSlot, after: Duration, f: impl FnOnce() + 'static) {
    if let Some(id) = slot.0.borrow_mut().take() {
        id.remove();
    }
    let inner = slot.0.clone();
    let id = glib::timeout_add_local_once(after, move || {
        inner.borrow_mut().take();
        f();
    });
    *slot.0.borrow_mut() = Some(id);
}

#[cfg(test)]
mod tests {
    use super::format_time;

    #[test]
    fn time_formatting() {
        assert_eq!(format_time(0.0), "0:00");
        assert_eq!(format_time(5.4), "0:05");
        assert_eq!(format_time(65.0), "1:05");
        assert_eq!(format_time(3600.0), "1:00:00");
        assert_eq!(format_time(3725.0), "1:02:05");
        assert_eq!(format_time(-3.0), "0:00");
    }
}
