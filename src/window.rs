//! Main window assembly: frameless surface with fade-in overlay
//! controls (FR-6), the single action layer every input goes through
//! (NFR-6.2), folder monitoring (FR-3.5), and the trash/undo/save
//! flows (FR-5).

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU32;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Action {
    Right,
    Left,
    Up,
    Down,
    Next,
    Previous,
    First,
    Last,
    PlayPause,
    SeekBack,
    SeekForward,
    Mute,
    VolumeUp,
    VolumeDown,
    ZoomIn,
    ZoomOut,
    ZoomFit,
    ZoomActual,
    ZoomToggle,
    RotateClockwise,
    RotateCounterclockwise,
    Save,
    Trash,
    Undo,
    Fullscreen,
    Help,
    Close,
    Escape,
}

impl Action {
    const fn as_str(self) -> &'static str {
        match self {
            Action::Right => "right",
            Action::Left => "left",
            Action::Up => "up",
            Action::Down => "down",
            Action::Next => "next",
            Action::Previous => "prev",
            Action::First => "first",
            Action::Last => "last",
            Action::PlayPause => "play-pause",
            Action::SeekBack => "seek-back",
            Action::SeekForward => "seek-forward",
            Action::Mute => "mute",
            Action::VolumeUp => "volume-up",
            Action::VolumeDown => "volume-down",
            Action::ZoomIn => "zoom-in",
            Action::ZoomOut => "zoom-out",
            Action::ZoomFit => "zoom-fit",
            Action::ZoomActual => "zoom-actual",
            Action::ZoomToggle => "zoom-toggle",
            Action::RotateClockwise => "rotate-cw",
            Action::RotateCounterclockwise => "rotate-ccw",
            Action::Save => "save",
            Action::Trash => "trash",
            Action::Undo => "undo",
            Action::Fullscreen => "fullscreen",
            Action::Help => "help",
            Action::Close => "close",
            Action::Escape => "escape",
        }
    }

    fn parse(name: &str) -> Option<Self> {
        ACTIONS
            .iter()
            .find_map(|(action, _)| (action.as_str() == name).then_some(*action))
    }
}

/// Default key → action map; config `bind=` lines override per key.
const DEFAULT_BINDS: &[(&str, Action)] = &[
    // The arrow keys are contextual (see `App::arrow`), which is why they
    // bind to their own actions rather than straight to next/prev: Page
    // Down must keep stepping through the folder even when a zoomed
    // image has the arrows panning.
    ("Right", Action::Right),
    ("Left", Action::Left),
    ("Up", Action::Up),
    ("Down", Action::Down),
    // Space pauses video, flips to the next photo otherwise.
    ("space", Action::PlayPause),
    ("Page_Down", Action::Next),
    ("BackSpace", Action::Previous),
    ("Page_Up", Action::Previous),
    ("Home", Action::First),
    ("End", Action::Last),
    ("plus", Action::ZoomIn),
    ("equal", Action::ZoomIn),
    ("KP_Add", Action::ZoomIn),
    ("minus", Action::ZoomOut),
    ("KP_Subtract", Action::ZoomOut),
    ("0", Action::ZoomFit),
    ("1", Action::ZoomActual),
    ("z", Action::ZoomToggle),
    ("r", Action::RotateClockwise),
    ("<Shift>r", Action::RotateCounterclockwise),
    ("s", Action::Save),
    // Video transport (FR-10.4), mpv-flavored.
    ("j", Action::SeekBack),
    ("l", Action::SeekForward),
    ("m", Action::Mute),
    ("Delete", Action::Trash),
    ("KP_Delete", Action::Trash),
    ("<Control>z", Action::Undo),
    ("f", Action::Fullscreen),
    ("F11", Action::Fullscreen),
    ("q", Action::Close),
    ("question", Action::Help),
    ("Escape", Action::Escape),
];

/// Every action, paired with the description the cheat sheet shows
/// (NFR-5.2). One list, so an action cannot be added without being
/// documented, and config binds are validated against the same names
/// (FR-8.2). `escape` carries no description: its layered behaviour is
/// spelled out in the footer instead.
const ACTIONS: &[(Action, &str)] = &[
    (Action::Right, "Next image, or pan when zoomed in"),
    (Action::Left, "Previous image, or pan when zoomed in"),
    (Action::Up, "Volume up, or pan when zoomed in"),
    (Action::Down, "Volume down, or pan when zoomed in"),
    (Action::Next, "Next image"),
    (Action::Previous, "Previous image"),
    (Action::First, "First image"),
    (Action::Last, "Last image"),
    (Action::PlayPause, "Pause video, or next image"),
    (Action::SeekBack, "Seek back 5 seconds"),
    (Action::SeekForward, "Seek forward 5 seconds"),
    (Action::Mute, "Mute audio"),
    (Action::VolumeUp, "Volume up"),
    (Action::VolumeDown, "Volume down"),
    (Action::ZoomIn, "Zoom in"),
    (Action::ZoomOut, "Zoom out"),
    (Action::ZoomFit, "Fit to window"),
    (Action::ZoomActual, "Actual size, 100%"),
    (Action::ZoomToggle, "Toggle fit and 100%"),
    (Action::RotateClockwise, "Rotate right"),
    (Action::RotateCounterclockwise, "Rotate left"),
    (Action::Save, "Save rotation to the file"),
    (Action::Trash, "Move to trash"),
    (Action::Undo, "Undo the delete"),
    (Action::Fullscreen, "Fullscreen"),
    (Action::Help, "This list"),
    (Action::Close, "Quit"),
    (Action::Escape, ""),
];

/// Seek bar width when the window has room for it: wide enough that a
/// pixel is a usable unit of time (a ten-minute video scrubs in ~2 s
/// steps) without turning the compact control bar into a strip.
const SEEK_BAR_MAX_WIDTH: i32 = 320;
/// Floor for narrow windows; below this the bar stops being aimable. The
/// control bar sheds other things before letting the seek bar reach it —
/// clamping here without shedding is what used to push the whole bar past
/// the window edge and clip the position/duration readout off it.
const SEEK_BAR_MIN_WIDTH: i32 = 96;

/// Invisible resize border along the window edges. Frameless means no
/// client-side decorations, and the decorations are what normally carry
/// the resize handles — so the app draws its own border (FR-6.4).
const RESIZE_MARGIN: f64 = 8.0;

/// Consecutive undecodable files navigation will step over before it
/// gives up and shows the error. A folder of unreadable files must not
/// spin, and with wrap on it would otherwise loop forever.
const SKIP_BUDGET: u32 = 32;

/// How far one arrow-key press moves a zoomed image, in logical pixels.
const PAN_STEP: f64 = 64.0;

/// Why an image is being shown, which decides what a decode failure
/// means: a file the user opened explicitly shows its error, while one
/// merely stepped over on the way through a folder is skipped (FR-2.5).
#[derive(Clone, Copy)]
enum Direction {
    Previous,
    Next,
}

#[derive(Clone, Copy)]
enum Arrival {
    Direct,
    Step { direction: Direction, budget: u32 },
}

/// The mutually exclusive states of the media area. Keeping the path,
/// decoded image and MIME type in one enum makes mismatched combinations
/// unrepresentable while the reusable video pipeline remains lazy.
enum MediaState {
    Empty,
    Loading(PathBuf),
    Image {
        path: PathBuf,
        decoded: Rc<Decoded>,
        mime: String,
    },
    Video(PathBuf),
    Error(PathBuf),
}

impl MediaState {
    fn path(&self) -> Option<&Path> {
        match self {
            MediaState::Loading(path)
            | MediaState::Image { path, .. }
            | MediaState::Video(path)
            | MediaState::Error(path) => Some(path),
            MediaState::Empty => None,
        }
    }

    fn replace_path(&mut self, new: PathBuf) {
        match self {
            MediaState::Loading(path)
            | MediaState::Image { path, .. }
            | MediaState::Video(path)
            | MediaState::Error(path) => *path = new,
            MediaState::Empty => {}
        }
    }
}

const TOAST_TIMEOUT: Duration = Duration::from_secs(5);
const FLASH_TIMEOUT: Duration = Duration::from_millis(1200);
const SVG_DEBOUNCE: Duration = Duration::from_millis(200);
/// How often a paused animation re-checks whether its window came back.
/// Long enough to cost nothing, short enough that restoring a window
/// does not visibly stall the picture.
const SUSPENDED_POLL: Duration = Duration::from_millis(500);

pub struct App {
    pub win: gtk::ApplicationWindow,
    cfg: Config,
    view: ImageView,
    folder: RefCell<Option<Folder>>,
    monitor: RefCell<Option<gio::FileMonitor>>,
    media: RefCell<MediaState>,
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
    name_label: gtk::Label,
    pos_label: gtk::Label,
    transport: gtk::Box,
    play_btn: gtk::Button,
    mute_btn: gtk::Button,
    save_btn: gtk::Button,
    /// Image-editing controls, the first thing the control bar gives up
    /// when a video leaves it no room (see `fit_seek_bar`).
    compact_group: Vec<gtk::Widget>,
    /// (window width, transport shown) the bar was last fitted for.
    fitted_for: Cell<Option<(i32, bool, usize, usize)>>,
    /// The bottom control bar; measured to size the seek bar to the room
    /// left over beside the labels and buttons.
    control_bar: gtk::Box,
    seek_bar: gtk::Scale,
    time_label: gtk::Label,
    /// Frame-clock tick driving the seek bar; installed only while the
    /// transport is on screen so a hidden overlay costs nothing (NFR-2.1).
    transport_tick: RefCell<Option<gtk::TickCallbackId>>,
    /// True while the pointer holds the seek bar: playback positions must
    /// not write the thumb back under the user's hand.
    scrubbing: Cell<bool>,
    /// True while the pointer rests on the overlay controls, which must
    /// not fade out from under it (FR-6.2).
    pointer_on_chrome: Cell<bool>,
    /// Last known pointer position, so the cursor can be re-decided when
    /// the overlay fades on a timer rather than on movement.
    pointer: Cell<(f64, f64)>,
    /// Session idle-inhibit cookie; `None` when nothing is held.
    inhibit_cookie: Cell<Option<NonZeroU32>>,
    /// True once the window has been sized from the media on screen. A
    /// video presents before its dimensions are known, so its sizing
    /// arrives late and must still be applied once (FR-6.6).
    sized_from_media: Cell<bool>,
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
        // What you are actually looking at. A frameless window has no
        // titlebar to carry the filename, so without this the name of the
        // file on screen appears nowhere in the UI at all.
        let name_label = gtk::Label::new(None);
        name_label.add_css_class("dim");
        name_label.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        // Bounds the natural width so a long name cannot push the bar
        // wider than the window; ellipsizing keeps its *minimum* small,
        // which is what fit_seek_bar measures against.
        name_label.set_max_width_chars(28);
        bar.append(&name_label);
        let pos_label = gtk::Label::new(None);
        pos_label.add_css_class("dim");
        bar.append(&pos_label);
        // Video transport: seek bar + position, hidden for images
        // (FR-10.5). The position tick runs only while this is visible.
        let seek_bar = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 1.0, 0.01);
        seek_bar.set_size_request(SEEK_BAR_MAX_WIDTH, -1);
        seek_bar.set_draw_value(false);
        // `with_range` derives round-digits from the step increment, which
        // would quantise the thumb to 1 % of the duration — 36 s of a
        // one-hour video. Scrubbing has to be continuous.
        seek_bar.set_round_digits(-1);
        let time_label = gtk::Label::new(None);
        time_label.add_css_class("dim");
        // Reserve "0:00 / 0:00" so the bar does not twitch every time a
        // digit rolls over; longer runtimes grow it once and settle.
        time_label.set_width_chars(11);
        // A seek bar with no play button was the clearest hole in the
        // overlay: pausing was keyboard-only (FR-6.5). Icons track the
        // pipeline in update_transport.
        let play_btn = bar_button(
            "media-playback-pause-symbolic",
            "win.play-pause",
            "Play / pause",
        );
        let mute_btn = bar_button("audio-volume-high-symbolic", "win.mute", "Mute");
        let transport = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        transport.append(&play_btn);
        transport.append(&seek_bar);
        transport.append(&time_label);
        transport.append(&mute_btn);
        transport.set_visible(false);
        bar.append(&transport);
        let rotate_ccw = bar_button(
            "object-rotate-left-symbolic",
            "win.rotate-ccw",
            "Rotate left",
        );
        let rotate_cw = bar_button(
            "object-rotate-right-symbolic",
            "win.rotate-cw",
            "Rotate right",
        );
        bar.append(&rotate_ccw);
        bar.append(&rotate_cw);
        // Held onto: a disabled save button has to be able to say why
        // (FR-5.4), which update_save_enabled writes into its tooltip.
        let save_btn = bar_button(
            "document-save-symbolic",
            "win.save",
            "Save rotation to file",
        );
        bar.append(&save_btn);
        for (icon, action, tip) in [
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
            media: RefCell::new(MediaState::Empty),
            cache: loader::Cache::new(3, cfg.cache_budget_mb as usize * 1024 * 1024),
            generation: Cell::new(0),
            editable_mimes: RefCell::new(BTreeSet::new()),
            player: RefCell::new(None),
            pending_undo: RefCell::new(None),
            presented: Cell::new(false),
            status,
            name_label,
            pos_label,
            transport,
            play_btn,
            mute_btn,
            compact_group: vec![
                rotate_ccw.upcast(),
                rotate_cw.upcast(),
                save_btn.clone().upcast(),
            ],
            save_btn,
            fitted_for: Cell::new(None),
            control_bar: bar.clone(),
            seek_bar,
            time_label,
            transport_tick: RefCell::new(None),
            scrubbing: Cell::new(false),
            pointer_on_chrome: Cell::new(false),
            pointer: Cell::new((0.0, 0.0)),
            inhibit_cookie: Cell::new(None),
            sized_from_media: Cell::new(false),
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
            save_action: gio::SimpleAction::new(Action::Save.as_str(), None),
            undo_action: gio::SimpleAction::new(Action::Undo.as_str(), None),
            cfg,
        });

        app.setup_actions(gtk_app);
        app.setup_controllers();
        app.build_help(gtk_app);

        if app.cfg.start_fullscreen {
            app.win.fullscreen();
        }

        // Clicking or dragging the seek bar seeks. Proceed — not Stop —
        // because GtkRange only moves the thumb to the pointer in its
        // default handler, which a handled signal would suppress. There is
        // no feedback loop: programmatic set_value does not fire
        // change-value.
        app.seek_bar.connect_change_value(clone!(
            #[strong(rename_to = app)]
            app,
            move |_, _, value| {
                app.with_video(|p| p.seek_fraction(value));
                glib::Propagation::Proceed
            }
        ));

        // Knowing when the pointer holds the bar keeps playback from
        // dragging the thumb back mid-scrub. GtkRange claims the pointer
        // sequence for its own drag, which would cancel a GestureClick of
        // ours — watching the raw events is what survives that.
        let press = gtk::EventControllerLegacy::new();
        press.set_propagation_phase(gtk::PropagationPhase::Capture);
        press.connect_event(clone!(
            #[strong(rename_to = app)]
            app,
            move |_, event| {
                match event.event_type() {
                    gdk::EventType::ButtonPress | gdk::EventType::TouchBegin => {
                        app.scrubbing.set(true)
                    }
                    gdk::EventType::ButtonRelease
                    | gdk::EventType::TouchEnd
                    | gdk::EventType::TouchCancel => {
                        app.scrubbing.set(false);
                        // Restart the fade-out that was held off while the
                        // bar was under the pointer.
                        app.show_chrome();
                    }
                    _ => {}
                }
                glib::Propagation::Proceed
            }
        ));
        app.seek_bar.add_controller(press);

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
            move |dir| {
                app.navigate(if dir > 0 {
                    Direction::Next
                } else {
                    Direction::Previous
                });
            }
        ));
        // A video is on screen before the pipeline knows how big it is;
        // the size arrives with preroll (FR-6.6).
        app.view.connect_source_size(clone!(
            #[strong]
            app,
            move |size| app.size_to_media(size)
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
                        Some(idx) => self.show_index(idx, Arrival::Direct),
                        None => self.show_error(&path, &excluded_path_message(&path)),
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
                self.show_index(0, Arrival::Direct);
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

    fn show_index(self: &Rc<Self>, idx: usize, arrival: Arrival) {
        let Some(path) = self
            .folder
            .borrow()
            .as_ref()
            .and_then(|f| f.get(idx))
            .map(Path::to_path_buf)
        else {
            return;
        };
        *self.media.borrow_mut() = MediaState::Loading(path.clone());
        self.cache.pin(&path);
        self.set_current_name(Some(&path));
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
            self.apply_decoded(path, decoded, mime, generation);
        } else {
            glib::spawn_future_local(clone!(
                #[strong(rename_to = app)]
                self,
                async move {
                    match loader::decode(&path).await {
                        Ok((decoded, mime)) => {
                            app.cache.put(path.clone(), decoded.clone(), mime.clone());
                            if app.generation.get() == generation {
                                app.apply_decoded(path.clone(), decoded, mime, generation);
                            } else {
                                crate::applog!(
                                    "show: {} superseded, kept in cache",
                                    path.display()
                                );
                            }
                        }
                        Err(e) => {
                            if app.generation.get() == generation {
                                app.on_decode_failed(&path, &e.to_string(), idx, arrival);
                            }
                        }
                    }
                }
            ));
        }
        self.preload_neighbors(idx);
    }

    /// A file did not decode. Stepping through a folder carries on in the
    /// direction of travel so one unreadable file is a hesitation rather
    /// than a wall; a file opened directly shows its error, so the user
    /// learns why nothing appeared (FR-2.5).
    fn on_decode_failed(self: &Rc<Self>, path: &Path, message: &str, idx: usize, arrival: Arrival) {
        let Arrival::Step { direction, budget } = arrival else {
            self.show_error(path, message);
            return;
        };
        let next = {
            let folder = self.folder.borrow();
            folder
                .as_ref()
                .and_then(|f| skip_target(f, idx, arrival, self.cfg.wrap))
        };
        match next {
            Some(next) => {
                crate::applog!("skip: {} ({message})", path.display());
                self.show_index(
                    next,
                    Arrival::Step {
                        direction,
                        budget: budget - 1,
                    },
                );
            }
            // Nowhere left to step, or the budget ran out.
            None => self.show_error(path, message),
        }
    }

    // ----- showing videos (FR-10) ---------------------------------------

    /// Lazily build the shared player; `Err` if the pipeline cannot be
    /// assembled (missing plugins) — a routine state, not a panic.
    fn player(self: &Rc<Self>) -> Result<Rc<Player>, player::PlayerError> {
        if let Some(p) = self.player.borrow().as_ref() {
            return Ok(p.clone());
        }
        let weak = Rc::downgrade(self);
        match Player::new(move |event| {
            if let Some(app) = weak.upgrade() {
                app.on_player_event(event);
            }
        }) {
            Ok(p) => {
                let p = Rc::new(p);
                p.set_volume(self.cfg.volume);
                *self.player.borrow_mut() = Some(p.clone());
                Ok(p)
            }
            Err(e) => Err(e),
        }
    }

    fn show_video(self: &Rc<Self>, path: &Path) {
        let player = match self.player() {
            Ok(player) => player,
            Err(e) => {
                self.show_error(path, &e.to_string());
                return;
            }
        };
        self.status.set_visible(false);
        self.view.show_paintable(player.paintable(), None);
        if let Err(e) = player.play(path) {
            self.show_error(path, &e.to_string());
            return;
        }
        *self.media.borrow_mut() = MediaState::Video(path.to_path_buf());
        crate::applog!("play: {}", path.display());
        self.set_idle_inhibited(true);
        self.update_save_enabled();
        self.transport.set_visible(true);
        self.fit_seek_bar();
        // Blank rather than carry the previous video's numbers over the
        // moment before this one's duration is known.
        self.seek_bar.set_value(0.0);
        self.time_label.set_text("");
        self.update_transport();
        if self.chrome_visible() {
            self.start_transport_tick();
        }
        // Dimensions arrive with preroll; until then present at the
        // default size rather than blocking on the pipeline.
        self.present_default();
    }

    fn stop_video(&self) {
        if let Some(p) = self.player.borrow().as_ref() {
            p.stop();
        }
        self.set_idle_inhibited(false);
        self.transport.set_visible(false);
        // Give back whatever the transport made the bar shed: nothing
        // else re-fits it, since the tick only runs for video.
        self.fit_seek_bar();
        // Hiding the bar mid-drag means no button release reaches it.
        self.scrubbing.set(false);
        self.stop_transport_tick();
    }

    // ----- video transport (FR-10.5) ------------------------------------

    /// Keep the control bar inside what the window can actually show. It
    /// is an overlay child, so an oversized bar does not push the window
    /// wider — it is squeezed, and whatever cannot shrink is clipped off
    /// the ends. With the transport shown the bar needs ~520 px before
    /// the seek bar gets any width at all, which is more than a window
    /// sized to a 640-wide video has to give; the position/duration
    /// readout was being clipped away as a result.
    ///
    /// So things yield in order of what the medium needs least: the seek
    /// bar gives up width first, then the filename, then rotate/save —
    /// which for a video are a fringe case and a disabled button anyway.
    fn fit_seek_bar(&self) {
        // Every measurement here forces a layout pass, and this is called
        // from the per-frame transport tick. Only the window width and
        // which mode we are in can change the answer.
        // The label lengths belong in the key, not just the window size:
        // the bar's minimum grows the moment the clock goes from empty to
        // "0:03 / 0:10", and a fit computed before that is stale.
        let key = (
            self.win.width(),
            self.transport.is_visible(),
            self.time_label.text().len(),
            self.pos_label.text().len(),
        );
        if self.fitted_for.get() == Some(key) {
            return;
        }
        self.fitted_for.set(Some(key));
        let (width, video, ..) = key;

        // Measure from everything restored, with the seek bar
        // contributing nothing, so `others` is the rest at its minimum.
        self.seek_bar.set_size_request(0, -1);
        self.name_label.set_visible(true);
        for w in &self.compact_group {
            w.set_visible(true);
        }
        let others = |s: &Self| s.control_bar.measure(gtk::Orientation::Horizontal, -1).0;

        let mut room = width - others(self);
        if room < SEEK_BAR_MIN_WIDTH && video {
            self.name_label.set_visible(false);
            room = width - others(self);
        }
        if room < SEEK_BAR_MIN_WIDTH && video {
            for w in &self.compact_group {
                w.set_visible(false);
            }
            room = width - others(self);
        }
        self.seek_bar
            .set_size_request(room.clamp(SEEK_BAR_MIN_WIDTH, SEEK_BAR_MAX_WIDTH), -1);
    }

    fn update_transport(&self) {
        // Cheap to re-assert every tick: GTK ignores an unchanged
        // request, and this is the one place that sees every resize.
        self.fit_seek_bar();
        // Button icons before the progress bail-out below: a video whose
        // duration is not known yet still has a truthful play state.
        if let Some(p) = self.player.borrow().as_ref() {
            set_icon(
                &self.play_btn,
                if p.is_playing() {
                    "media-playback-pause-symbolic"
                } else {
                    "media-playback-start-symbolic"
                },
            );
            set_icon(
                &self.mute_btn,
                if p.is_muted() {
                    "audio-volume-muted-symbolic"
                } else {
                    "audio-volume-high-symbolic"
                },
            );
        }
        let progress = self.player.borrow().as_ref().and_then(|p| p.progress());
        let Some((pos, dur)) = progress else {
            return;
        };
        // While the pointer holds the thumb it owns the bar's value.
        if dur > 0.0 && !self.scrubbing.get() {
            self.seek_bar.set_value(pos / dur);
        }
        let time = format!("{} / {}", format_time(pos), format_time(dur));
        if self.time_label.text() != time {
            self.time_label.set_text(&time);
        }
    }

    /// Drive the bar from the frame clock: it advances with the frames the
    /// user is watching, instead of stepping once per polling interval.
    fn start_transport_tick(self: &Rc<Self>) {
        if self.transport_tick.borrow().is_some() || !self.is_video_showing() {
            return;
        }
        self.update_transport();
        let weak = Rc::downgrade(self);
        let id = self.seek_bar.add_tick_callback(move |_, _| {
            let Some(app) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            app.update_transport();
            glib::ControlFlow::Continue
        });
        *self.transport_tick.borrow_mut() = Some(id);
    }

    fn stop_transport_tick(&self) {
        if let Some(id) = self.transport_tick.borrow_mut().take() {
            id.remove();
        }
    }

    // ----- window edges (FR-6.4) ----------------------------------------

    /// The edge the pointer is close enough to grab, if any. Fullscreen
    /// has no edges to pull.
    fn resize_edge(&self, x: f64, y: f64) -> Option<gdk::SurfaceEdge> {
        if self.win.is_fullscreen() {
            return None;
        }
        resize_edge_at(x, y, self.win.width() as f64, self.win.height() as f64)
    }

    /// Show the grab as a resize cursor, so the border can be found
    /// without knowing it is there.
    ///
    /// The single owner of the cursor: a resize arrow near an edge, hidden
    /// once the overlay has faded (mpv-style, `hide-cursor`), the theme
    /// default otherwise. Routing both through here keeps the resize
    /// border and the idle-hide from overwriting each other.
    fn update_cursor(&self) {
        let (x, y) = self.pointer.get();
        let name = cursor_name(
            self.resize_edge(x, y),
            self.chrome_visible(),
            self.cfg.hide_cursor,
        );
        let current = self.win.cursor().and_then(|c| c.name());
        if current.as_deref() != name {
            self.win.set_cursor_from_name(name);
        }
    }

    /// Hold off the session's idle blanker while a video actually plays.
    /// Nothing else stops GNOME from blanking the screen mid-film; the
    /// inhibit is dropped on pause and on leaving the video so a paused
    /// or image-only session never holds it (NFR-2.2).
    fn set_idle_inhibited(&self, inhibited: bool) {
        if inhibited == self.inhibit_cookie.get().is_some() {
            return;
        }
        let Some(gtk_app) = self.win.application() else {
            return;
        };
        if inhibited {
            let cookie = gtk_app.inhibit(
                Some(&self.win),
                gtk::ApplicationInhibitFlags::IDLE,
                Some("playing video"),
            );
            let Some(cookie) = NonZeroU32::new(cookie) else {
                return;
            };
            self.inhibit_cookie.set(Some(cookie));
            crate::applog!("idle inhibit: taken (cookie {cookie})");
        } else if let Some(cookie) = self.inhibit_cookie.take() {
            gtk_app.uninhibit(cookie.get());
            crate::applog!("idle inhibit: released");
        }
    }

    fn chrome_visible(&self) -> bool {
        !self
            .chrome
            .first()
            .is_some_and(|w| w.has_css_class("invisible"))
    }

    fn is_video_showing(&self) -> bool {
        matches!(*self.media.borrow(), MediaState::Video(_))
    }

    fn on_player_event(self: &Rc<Self>, event: player::Event) {
        match event {
            player::Event::EndOfStream => {
                // Loop like animated images do, unless told not to
                // (FR-10.3, `loop=no`) — then the last frame stays up.
                if self.is_video_showing()
                    && self.cfg.loop_video
                    && let Some(p) = self.player.borrow().as_ref()
                {
                    p.rewind();
                }
            }
            player::Event::Error(error) => {
                let path = self.current_path();
                self.stop_video();
                if let Some(path) = path {
                    self.show_error(&path, &error.to_string());
                }
            }
            player::Event::MissingVideoDecoder(description) => {
                let path = self.current_path();
                self.stop_video();
                if let Some(path) = path {
                    self.show_error(&path, &format!("video decoder unavailable: {description}"));
                }
            }
        }
    }

    /// Run `f` on the player when a video is on screen; used by the
    /// transport actions so they are no-ops on images.
    fn with_video<T>(&self, f: impl FnOnce(&Player) -> T) -> Option<T> {
        if !self.is_video_showing() {
            return None;
        }
        let player = self.player.borrow().clone();
        player.as_deref().map(f)
    }

    fn apply_decoded(
        self: &Rc<Self>,
        path: PathBuf,
        decoded: Rc<Decoded>,
        mime: String,
        generation: u64,
    ) {
        self.status.set_visible(false);
        *self.media.borrow_mut() = MediaState::Image {
            path,
            decoded: decoded.clone(),
            mime,
        };
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

    /// Play an animated image.
    ///
    /// Every frame costs an IPC round trip to the sandboxed loader plus a
    /// texture upload — around 3 % of a core even for a small GIF — and a
    /// bare timer goes on paying that whether or not the result can be
    /// seen. `is_suspended` is GTK's answer to exactly that question:
    /// minimised, fully obscured, or on another workspace. Idling there
    /// costs one wake-up every `SUSPENDED_POLL` instead.
    ///
    /// The frame clock would also stall while hidden, but driving from it
    /// means ticking at the display's refresh rate rather than the GIF's:
    /// measured on this machine that traded 1.1 % while hidden for an
    /// extra 0.6 % every time an animation *is* on screen, which is the
    /// case that actually happens.
    fn spawn_animation(self: &Rc<Self>, decoded: Rc<Decoded>, generation: u64) {
        glib::spawn_future_local(clone!(
            #[strong(rename_to = app)]
            self,
            async move {
                let Decoded::Animated { image, .. } = &*decoded else {
                    return;
                };
                loop {
                    // Hold the current frame rather than animate to a
                    // surface nobody is looking at.
                    while app.win.is_suspended() {
                        glib::timeout_future(SUSPENDED_POLL).await;
                        if app.generation.get() != generation {
                            return;
                        }
                    }
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
            &*self.media.borrow(),
            MediaState::Image {
                decoded,
                ..
            } if matches!(&**decoded, Decoded::Svg { .. })
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
                    let decoded = match &*app.media.borrow() {
                        MediaState::Image { decoded, .. } => Some(decoded.clone()),
                        _ => None,
                    };
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
                // Videos are streamed, never pre-decoded (FR-10.2).
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

    /// Take down whatever is on screen: silence any video, drop the
    /// decoded image, and bump the generation so async work already in
    /// flight knows it has been superseded.
    fn clear_media(&self) {
        self.generation.set(self.generation.get() + 1);
        self.stop_video();
        self.view.clear();
        *self.media.borrow_mut() = MediaState::Empty;
    }

    /// Put a message where the image would be (FR-1.4).
    fn show_status(&self, text: &str) {
        self.status.set_text(text);
        self.status.set_visible(true);
        self.update_save_enabled();
    }

    fn show_error(self: &Rc<Self>, path: &Path, message: &str) {
        eprintln!("open-mpv: error: {}: {message}", path.display());
        self.clear_media();
        // The error retains its path so folder navigation remains
        // positioned on the file that failed.
        *self.media.borrow_mut() = MediaState::Error(path.to_path_buf());
        self.show_status(&format!(
            "{}\n\n{message}",
            path.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string())
        ));
        self.present_default();
    }

    fn empty_state(self: &Rc<Self>, message: &str) {
        self.clear_media();
        // Nothing is positioned anywhere any more, unlike show_error.
        self.set_current_name(None);
        self.update_pos_label();
        self.show_status(message);
        self.present_default();
    }

    /// First image decides the initial window size: the image at 100%,
    /// capped at 85% of the monitor work area (FR-6.6).
    fn maybe_first_present(&self, size: (f64, f64)) {
        if !self.presented.get() {
            self.size_to_media(size);
            self.presented.set(true);
            // The cold-start metric (NFR-1.1): launch → first present.
            crate::applog!("first present: window shown");
        }
        self.win.present();
    }

    /// Size the window to `size` at 100%, capped at 85% of the monitor
    /// work area (FR-6.6). Only the first media of a session does this;
    /// everything after reuses whatever size the window has (FR-4.6).
    fn size_to_media(&self, size: (f64, f64)) {
        if self.sized_from_media.get() || size.0 <= 0.0 || size.1 <= 0.0 {
            return;
        }
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
        self.sized_from_media.set(true);
        crate::applog!("window sized to media {}x{}", size.0, size.1);
    }

    // ----- navigation ---------------------------------------------------

    fn current_path(&self) -> Option<PathBuf> {
        self.media.borrow().path().map(Path::to_path_buf)
    }

    fn current_index(&self) -> Option<usize> {
        let showing = self.current_path();
        let folder = self.folder.borrow();
        folder.as_ref()?.index_of(showing.as_deref()?)
    }

    fn navigate(self: &Rc<Self>, direction: Direction) {
        let target = {
            let folder = self.folder.borrow();
            let current = self.current_index();
            folder
                .as_ref()
                .and_then(|f| nav_target(f, current, direction, self.cfg.wrap))
        };
        match target {
            Some(idx) => self.show_index(
                idx,
                Arrival::Step {
                    direction,
                    budget: SKIP_BUDGET,
                },
            ),
            None => self.flash(match direction {
                Direction::Next => "Last image",
                Direction::Previous => "First image",
            }),
        }
    }

    /// The window title and the overlay's name label go together: on a
    /// frameless window the title is invisible, so the label is the only
    /// place the filename is ever shown.
    fn set_current_name(&self, path: Option<&Path>) {
        let name = path
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy());
        self.name_label.set_text(name.as_deref().unwrap_or(""));
        self.name_label.set_tooltip_text(name.as_deref());
        self.win
            .set_title(Some(name.as_deref().unwrap_or("open-mpv")));
    }

    /// The arrow keys are contextual, like Space: a zoomed image has
    /// somewhere to go, so they pan it (FR-4.3); a fitted one does not,
    /// so sideways steps through the folder and vertical works the video
    /// volume. `dx`/`dy` are the direction of travel in screen terms.
    fn arrow(self: &Rc<Self>, dx: i32, dy: i32) {
        if self.view.is_pannable() {
            // Moving the view right means moving the image left.
            self.view
                .pan_by(-f64::from(dx) * PAN_STEP, -f64::from(dy) * PAN_STEP);
        } else if dx != 0 {
            self.navigate(if dx > 0 {
                Direction::Next
            } else {
                Direction::Previous
            });
        } else {
            self.change_volume(if dy < 0 { 0.1 } else { -0.1 });
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
            let showing = self.current_path();
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
                                self.media.borrow_mut().replace_path(new.clone());
                                self.set_current_name(Some(&new));
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
                .current_path()
                .as_deref()
                .and_then(|p| self.folder.borrow().as_ref().and_then(|f| f.index_of(p)))
                .unwrap_or(0);
            self.show_index(idx.min(len - 1), Arrival::Direct);
        }
    }

    // ----- file operations (FR-5) --------------------------------------

    fn trash_current(self: &Rc<Self>) {
        let Some(path) = self.current_path() else {
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
                            app.show_index(idx.min(len - 1), Arrival::Direct);
                        }
                        app.show_toast("Moved to trash", true);
                    }
                    Err(e) => {
                        eprintln!("open-mpv: error: {e}");
                        app.show_toast(&e.to_string(), false);
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
                // `restore` has to enumerate the freedesktop trash on
                // disk. Keep that synchronous filesystem work off the GTK
                // main thread while the Gio pool runs it (NFR-1.2).
                let restore_path = path.clone();
                let result: Result<(), String> =
                    match gio::spawn_blocking(move || fileops::restore(&restore_path)).await {
                        Ok(result) => result.map_err(|error| error.to_string()),
                        Err(_) => Err(format!(
                            "could not restore {}: restore worker failed",
                            path.display()
                        )),
                    };
                match result {
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
                            app.show_index(idx, Arrival::Direct);
                        }
                    }
                    Err(e) => {
                        eprintln!("open-mpv: error: {e}");
                        app.show_toast(&e.to_string(), false);
                    }
                }
            }
        ));
    }

    fn save_rotation(self: &Rc<Self>) {
        let rotation = self.view.rotation();
        let Some(path) = self.current_path() else {
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
                            app.show_index(idx, Arrival::Direct);
                        }
                    }
                    Err(e) => {
                        eprintln!("open-mpv: error: {e}");
                        app.show_toast(&e.to_string(), false);
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
        // Why, not just whether: a greyed-out button that never says what
        // is wrong reads as a bug (FR-5.4).
        let media = self.media.borrow();
        let reason = match &*media {
            MediaState::Image { decoded, mime, .. }
                if matches!(&**decoded, Decoded::Static { .. }) =>
            {
                if self.editable_mimes.borrow().contains(mime) {
                    None
                } else {
                    Some(format!("{mime} images cannot be written back"))
                }
            }
            MediaState::Image { decoded, .. } if matches!(&**decoded, Decoded::Svg { .. }) => {
                Some("SVG rotation is view-only — the file is never rewritten".into())
            }
            MediaState::Image { decoded, .. } if matches!(&**decoded, Decoded::Animated { .. }) => {
                Some("Animated images can only be rotated in the view".into())
            }
            MediaState::Video(_) => Some("Video cannot be rotated and saved".into()),
            _ => Some("Nothing to save".into()),
        };
        // `set_enabled` can emit into application code; never hold a
        // RefCell borrow across that framework boundary.
        drop(media);
        let enabled = reason.is_none() && self.view.rotation() != 0;
        self.save_action.set_enabled(enabled);
        self.save_btn.set_tooltip_text(Some(&match reason {
            Some(why) => why,
            None if enabled => "Save rotation to file".into(),
            // Editable, but nothing has been rotated yet.
            None => "Rotate the image first".into(),
        }));
    }

    // ----- overlay chrome, toasts, indicator (FR-6.2) -------------------

    fn show_chrome(self: &Rc<Self>) {
        for w in &self.chrome {
            w.remove_css_class("invisible");
            w.set_can_target(true);
        }
        if self.is_video_showing() {
            self.start_transport_tick();
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
        // Never pull the controls out from under the pointer: mid-scrub,
        // or while it simply rests on them. Both paths restart the
        // fade-out when the pointer is done.
        if self.scrubbing.get() || self.pointer_on_chrome.get() {
            return;
        }
        for w in &self.chrome {
            w.add_css_class("invisible");
            w.set_can_target(false);
        }
        // The pointer goes with them; the chrome had to be marked hidden
        // first, since that is what update_cursor reads.
        self.update_cursor();
        // A hidden overlay must not keep polling the pipeline.
        self.stop_transport_tick();
    }

    /// Flash the current video position after a seek (FR-10.5).
    fn flash_progress(self: &Rc<Self>) {
        if let Some((pos, dur)) = self.with_video(Player::progress).flatten() {
            self.flash(&format!("{} / {}", format_time(pos), format_time(dur)));
        }
    }

    fn change_volume(self: &Rc<Self>, delta: f64) {
        if let Some(v) = self.with_video(|p| p.add_volume(delta)) {
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
        let add = |name: Action, f: Handler| {
            let action = gio::SimpleAction::new(name.as_str(), None);
            let app = self.clone();
            action.connect_activate(move |_, _| f(&app));
            self.win.add_action(&action);
            action
        };

        add(Action::Next, Box::new(|a| a.navigate(Direction::Next)));
        add(
            Action::Previous,
            Box::new(|a| a.navigate(Direction::Previous)),
        );
        add(Action::Right, Box::new(|a| a.arrow(1, 0)));
        add(Action::Left, Box::new(|a| a.arrow(-1, 0)));
        add(Action::Up, Box::new(|a| a.arrow(0, -1)));
        add(Action::Down, Box::new(|a| a.arrow(0, 1)));
        add(
            Action::First,
            Box::new(|a| {
                if a.folder.borrow().as_ref().is_some_and(|f| !f.is_empty()) {
                    a.show_index(0, Arrival::Direct);
                }
            }),
        );
        add(
            Action::Last,
            Box::new(|a| {
                let len = a.folder.borrow().as_ref().map_or(0, Folder::len);
                if len > 0 {
                    a.show_index(len - 1, Arrival::Direct);
                }
            }),
        );
        add(Action::ZoomIn, Box::new(|a| a.view.zoom_by(1.25, None)));
        add(Action::ZoomOut, Box::new(|a| a.view.zoom_by(0.8, None)));
        add(Action::ZoomFit, Box::new(|a| a.view.zoom_fit()));
        add(Action::ZoomActual, Box::new(|a| a.view.zoom_to(1.0, None)));
        add(Action::ZoomToggle, Box::new(|a| a.view.toggle_fit_actual()));
        add(Action::RotateClockwise, Box::new(|a| a.view.rotate_view(1)));
        add(
            Action::RotateCounterclockwise,
            Box::new(|a| a.view.rotate_view(-1)),
        );
        add(
            Action::PlayPause,
            Box::new(|a| {
                if a.is_video_showing() {
                    // A paused video must not keep the screen awake.
                    match a.with_video(Player::toggle_pause) {
                        Some(true) => {
                            a.set_idle_inhibited(true);
                            a.flash("Play");
                        }
                        Some(false) => {
                            a.set_idle_inhibited(false);
                            a.flash("Paused");
                        }
                        None => {}
                    }
                } else {
                    // Space keeps its photo-flipping habit on images.
                    a.navigate(Direction::Next);
                }
            }),
        );
        add(
            Action::SeekBack,
            Box::new(|a| {
                a.with_video(|p| p.seek_by(-5.0));
                a.flash_progress();
            }),
        );
        add(
            Action::SeekForward,
            Box::new(|a| {
                a.with_video(|p| p.seek_by(5.0));
                a.flash_progress();
            }),
        );
        add(
            Action::Mute,
            Box::new(|a| match a.with_video(Player::toggle_mute) {
                Some(true) => a.flash("Muted"),
                Some(false) => a.flash("Sound on"),
                None => {}
            }),
        );
        add(Action::VolumeUp, Box::new(|a| a.change_volume(0.1)));
        add(Action::VolumeDown, Box::new(|a| a.change_volume(-0.1)));
        add(Action::Trash, Box::new(|a| a.trash_current()));
        add(
            Action::Fullscreen,
            Box::new(|a| {
                if a.win.is_fullscreen() {
                    a.win.unfullscreen();
                } else {
                    a.win.fullscreen();
                }
            }),
        );
        add(Action::Close, Box::new(|a| a.win.close()));
        add(
            Action::Help,
            Box::new(|a| a.help_label.set_visible(!a.help_label.is_visible())),
        );
        // Escape: help → fullscreen → close (FR-6.7).
        add(
            Action::Escape,
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
        let mut key_to_action: BTreeMap<String, Action> = DEFAULT_BINDS
            .iter()
            .map(|(key, action)| (key.to_string(), *action))
            .collect();
        for (key, action) in &self.cfg.binds {
            if gtk::accelerator_parse(key).is_none() {
                eprintln!("open-mpv: config: cannot parse key `{key}`");
                continue;
            }
            // `bind=<key> none` takes a key away without replacing it —
            // otherwise a default binding can only ever be overridden.
            if action == "none" {
                key_to_action.remove(key);
                continue;
            }
            let Some(action) = Action::parse(action) else {
                eprintln!("open-mpv: config: unknown action `{action}` for bind `{key}`");
                continue;
            };
            key_to_action.insert(key.clone(), action);
        }
        let mut action_to_keys: BTreeMap<Action, Vec<&str>> = BTreeMap::new();
        for (key, action) in &key_to_action {
            action_to_keys
                .entry(*action)
                .or_default()
                .push(key.as_str());
        }
        for (action, keys) in &action_to_keys {
            gtk_app.set_accels_for_action(&format!("win.{}", action.as_str()), keys);
        }
    }

    fn build_help(&self, gtk_app: &gtk::Application) {
        let mut rows: Vec<(String, &str)> = Vec::new();
        for (action, description) in ACTIONS {
            if description.is_empty() {
                continue;
            }
            let accels = gtk_app.accels_for_action(&format!("win.{}", action.as_str()));
            if accels.is_empty() {
                continue;
            }
            // "<Control>z" is the binding syntax, not something to read;
            // GTK renders it the way the rest of the desktop writes it.
            let keys: Vec<String> = accels
                .iter()
                .map(|a| match gtk::accelerator_parse(a) {
                    Some((key, mods)) => gtk::accelerator_get_label(key, mods).to_string(),
                    None => a.to_string(),
                })
                .collect();
            rows.push((keys.join(", "), description));
        }
        let width = rows
            .iter()
            .map(|(keys, _)| keys.chars().count())
            .max()
            .unwrap_or(0);
        let mut lines = vec!["<b>Keys</b>".to_string(), String::new()];
        lines.extend(
            rows.iter()
                .map(|(keys, description)| help_line(keys, description, width)),
        );
        lines.push(String::new());
        lines.push("<tt>Escape</tt> leaves fullscreen, then closes".to_string());
        lines.push("Scroll: zoom · horizontal scroll: navigate".to_string());
        lines.push("Drag: pan (zoomed) or move window · double-click: fullscreen".to_string());
        lines.push("Drag an edge or corner: resize the window".to_string());
        self.help_label.set_markup(&lines.join("\n"));
    }

    fn setup_controllers(self: &Rc<Self>) {
        // Mouse movement reveals the chrome (FR-6.2); keyboard-only use
        // never shows it.
        let motion = gtk::EventControllerMotion::new();
        motion.connect_motion(clone!(
            #[strong(rename_to = app)]
            self,
            move |_, x, y| {
                app.pointer.set((x, y));
                app.show_chrome();
                app.update_cursor();
            }
        ));
        motion.connect_leave(clone!(
            #[strong(rename_to = app)]
            self,
            move |_| {
                // Order matters: hide_chrome re-decides the cursor from
                // the last known position, so the reset goes after it.
                app.hide_chrome();
                app.win.set_cursor_from_name(None);
            }
        ));
        self.win.add_controller(motion);

        // A frameless window has no decorations to grab, so the edges are
        // the app's job (FR-6.4). Capture phase, and claiming only inside
        // the margin, puts the edge ahead of the pan and window-move
        // drags that start on the image while leaving them untouched
        // everywhere else.
        let resize = gtk::GestureDrag::new();
        resize.set_propagation_phase(gtk::PropagationPhase::Capture);
        resize.connect_drag_begin(clone!(
            #[strong(rename_to = app)]
            self,
            move |gesture, x, y| {
                let Some(edge) = app.resize_edge(x, y) else {
                    return;
                };
                let Some(surface) = app.win.surface() else {
                    return;
                };
                let Ok(toplevel) = surface.downcast::<gdk::Toplevel>() else {
                    return;
                };
                let Some(device) = gesture.device() else {
                    return;
                };
                gesture.set_state(gtk::EventSequenceState::Claimed);
                // The compositor owns the drag from here (Wayland has no
                // client-side window geometry).
                toplevel.begin_resize(
                    edge,
                    Some(&device),
                    gdk::BUTTON_PRIMARY as i32,
                    x,
                    y,
                    gesture.current_event_time(),
                );
            }
        ));
        self.win.add_controller(resize);

        // A pointer parked on the controls generates no motion, so the
        // fade timer would run out under it and take the click target
        // away. Hovering holds them until the pointer moves off.
        for w in &self.chrome {
            let hover = gtk::EventControllerMotion::new();
            hover.connect_enter(clone!(
                #[strong(rename_to = app)]
                self,
                move |_, _, _| app.pointer_on_chrome.set(true)
            ));
            hover.connect_leave(clone!(
                #[strong(rename_to = app)]
                self,
                move |_| {
                    app.pointer_on_chrome.set(false);
                    app.show_chrome();
                }
            ));
            w.add_controller(hover);
        }

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

/// One cheat-sheet row, padded to `width`. The keys are padded *before*
/// escaping, so markup entities never count toward the column — `&lt;` is
/// one character on screen and four in the string. The width is measured
/// from the rows themselves rather than fixed, so a long accelerator
/// list widens the column instead of spilling out of it.
fn help_line(keys: &str, description: &str, width: usize) -> String {
    format!(
        "<tt>{}</tt> {description}",
        glib::markup_escape_text(&format!("{keys:<width$}"))
    )
}

/// Set a button's icon only when it actually changes — this runs from the
/// per-frame transport tick.
fn set_icon(button: &gtk::Button, icon: &str) {
    if button.icon_name().as_deref() != Some(icon) {
        button.set_icon_name(icon);
    }
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

/// Index to move to for a next/previous step. With nothing on screen —
/// an unsupported file was opened directly, so the folder loaded but
/// never landed on an image — the folder is entered from whichever end
/// the key points at, rather than leaving the arrows inert.
fn nav_target(
    folder: &Folder,
    current: Option<usize>,
    direction: Direction,
    wrap: bool,
) -> Option<usize> {
    if folder.is_empty() {
        return None;
    }
    match (current, direction) {
        (Some(current), Direction::Next) => folder.next(current, wrap),
        (Some(current), Direction::Previous) => folder.prev(current, wrap),
        (None, Direction::Next) => Some(0),
        (None, Direction::Previous) => Some(folder.len() - 1),
    }
}

/// Where a decode failure at `idx` sends us: onward in the direction of
/// travel, or `None` to stop and show the error (FR-2.5).
fn skip_target(folder: &Folder, idx: usize, arrival: Arrival, wrap: bool) -> Option<usize> {
    let Arrival::Step { direction, budget } = arrival else {
        return None;
    };
    if budget == 0 {
        return None;
    }
    match direction {
        Direction::Next => folder.next(idx, wrap),
        Direction::Previous => folder.prev(idx, wrap),
    }
}

/// Explain why a directly requested path was excluded from the folder.
/// Keeping the distinctions here prevents a vanished download from
/// being mislabeled as an unsupported format (FR-1.4).
fn excluded_path_message(path: &Path) -> String {
    match path.try_exists() {
        Ok(false) => "file does not exist".to_owned(),
        Err(e) => format!("cannot access path: {e}"),
        Ok(true) if !config::is_supported(path) => "unsupported file type".to_owned(),
        Ok(true) => "not a regular file".to_owned(),
    }
}

/// Which window edge a point belongs to, given the window size. Corners
/// win over sides so the diagonal grabs stay reachable.
fn resize_edge_at(x: f64, y: f64, w: f64, h: f64) -> Option<gdk::SurfaceEdge> {
    let (left, right) = (x <= RESIZE_MARGIN, x >= w - RESIZE_MARGIN);
    let (top, bottom) = (y <= RESIZE_MARGIN, y >= h - RESIZE_MARGIN);
    Some(match (left, right, top, bottom) {
        (true, _, true, _) => gdk::SurfaceEdge::NorthWest,
        (_, true, true, _) => gdk::SurfaceEdge::NorthEast,
        (true, _, _, true) => gdk::SurfaceEdge::SouthWest,
        (_, true, _, true) => gdk::SurfaceEdge::SouthEast,
        (true, ..) => gdk::SurfaceEdge::West,
        (_, true, ..) => gdk::SurfaceEdge::East,
        (_, _, true, _) => gdk::SurfaceEdge::North,
        (_, _, _, true) => gdk::SurfaceEdge::South,
        _ => return None,
    })
}

/// The cursor to show: the resize arrow wins wherever there is an edge to
/// grab, so the border stays discoverable even after the overlay fades;
/// otherwise the pointer goes away with the controls (mpv-style).
fn cursor_name(
    edge: Option<gdk::SurfaceEdge>,
    chrome_visible: bool,
    hide_cursor: bool,
) -> Option<&'static str> {
    match edge {
        Some(edge) => Some(edge_cursor_name(edge)),
        // Only once the controls are gone: a pointer that vanishes while
        // there are still buttons to aim at is just lost.
        None if hide_cursor && !chrome_visible => Some("none"),
        None => None,
    }
}

fn edge_cursor_name(edge: gdk::SurfaceEdge) -> &'static str {
    match edge {
        gdk::SurfaceEdge::NorthWest => "nw-resize",
        gdk::SurfaceEdge::North => "n-resize",
        gdk::SurfaceEdge::NorthEast => "ne-resize",
        gdk::SurfaceEdge::West => "w-resize",
        gdk::SurfaceEdge::East => "e-resize",
        gdk::SurfaceEdge::SouthWest => "sw-resize",
        gdk::SurfaceEdge::SouthEast => "se-resize",
        _ => "s-resize",
    }
}

/// `M:SS`, or `H:MM:SS` from the first hour (FR-10.5).
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
    use super::{
        ACTIONS, Action, Arrival, DEFAULT_BINDS, Direction, MediaState, SKIP_BUDGET,
        excluded_path_message, format_time, nav_target, resize_edge_at, skip_target,
    };

    use crate::config::{Sort, SortOrder};
    use crate::folder::Folder;
    use gtk4::gdk::SurfaceEdge;
    use std::path::PathBuf;

    /// A three-image folder on disk, which is what `Folder` reads.
    fn folder_of(name: &str, files: &[&str]) -> (std::path::PathBuf, Folder) {
        let dir =
            std::env::temp_dir().join(format!("open-mpv-window-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for f in files {
            std::fs::File::create(dir.join(f)).unwrap();
        }
        let folder = Folder::scan(
            &dir,
            Sort {
                order: SortOrder::Name,
                reverse: false,
            },
        )
        .unwrap();
        (dir, folder)
    }

    #[test]
    fn arrows_work_when_nothing_is_on_screen() {
        // Opening an unsupported file loads the folder but never lands on
        // an image, leaving the media state empty. The arrows used to go dead:
        // navigate() bailed out on the missing current index.
        let (dir, folder) = folder_of("nav", &["a.jpg", "b.jpg", "c.jpg"]);
        assert_eq!(
            nav_target(&folder, None, Direction::Next, false),
            Some(0),
            "right enters at the first image"
        );
        assert_eq!(
            nav_target(&folder, None, Direction::Previous, false),
            Some(2),
            "left enters at the last"
        );
        // Normal stepping is unchanged.
        assert_eq!(
            nav_target(&folder, Some(0), Direction::Next, false),
            Some(1)
        );
        assert_eq!(nav_target(&folder, Some(2), Direction::Next, false), None);
        assert_eq!(nav_target(&folder, Some(2), Direction::Next, true), Some(0));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn media_state_owns_the_position_through_transitions_and_renames() {
        let original = PathBuf::from("a.jpg");
        let renamed = PathBuf::from("b.jpg");
        let mut state = MediaState::Loading(original.clone());
        assert_eq!(state.path(), Some(original.as_path()));

        state = MediaState::Error(original);
        state.replace_path(renamed.clone());
        assert_eq!(state.path(), Some(renamed.as_path()));

        state = MediaState::Empty;
        state.replace_path(PathBuf::from("ignored.jpg"));
        assert_eq!(state.path(), None);
    }

    #[test]
    fn excluded_paths_report_why_they_cannot_open() {
        let (dir, _) = folder_of("excluded", &["notes.txt"]);
        let missing = dir.join("gone.mp4");
        let unsupported = dir.join("notes.txt");

        assert_eq!(excluded_path_message(&missing), "file does not exist");
        assert_eq!(excluded_path_message(&unsupported), "unsupported file type");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_empty_folder_has_nowhere_to_enter() {
        let (dir, folder) = folder_of("empty", &["notes.txt"]);
        assert_eq!(nav_target(&folder, None, Direction::Next, false), None);
        assert_eq!(nav_target(&folder, None, Direction::Previous, true), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn undecodable_files_are_stepped_over_only_while_navigating() {
        let (dir, folder) = folder_of("skip", &["a.jpg", "b.jpg", "c.jpg"]);
        let step = |direction, budget| Arrival::Step { direction, budget };

        // Opened directly: the error is the answer (FR-2.5).
        assert_eq!(skip_target(&folder, 1, Arrival::Direct, false), None);
        // Stepping: carry on the way the user was already going.
        assert_eq!(
            skip_target(&folder, 1, step(Direction::Next, SKIP_BUDGET), false),
            Some(2)
        );
        assert_eq!(
            skip_target(&folder, 1, step(Direction::Previous, SKIP_BUDGET), false),
            Some(0)
        );
        // Nowhere further to step.
        assert_eq!(
            skip_target(&folder, 2, step(Direction::Next, SKIP_BUDGET), false),
            None
        );
        // A folder of unreadable files must stop, not spin — especially
        // with wrap on, where there is always a next index.
        assert_eq!(
            skip_target(&folder, 1, step(Direction::Next, 1), true),
            Some(2)
        );
        assert_eq!(
            skip_target(&folder, 1, step(Direction::Next, 0), true),
            None
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn window_edges_are_grabbable() {
        // 400x300 window; the margin is 8 px.
        let at = |x, y| resize_edge_at(x, y, 400.0, 300.0);
        assert_eq!(at(200.0, 150.0), None, "the middle is not an edge");
        assert_eq!(at(0.0, 0.0), Some(SurfaceEdge::NorthWest));
        assert_eq!(at(399.0, 299.0), Some(SurfaceEdge::SouthEast));
        assert_eq!(at(2.0, 297.0), Some(SurfaceEdge::SouthWest));
        assert_eq!(at(398.0, 3.0), Some(SurfaceEdge::NorthEast));
        assert_eq!(at(200.0, 1.0), Some(SurfaceEdge::North));
        assert_eq!(at(200.0, 299.0), Some(SurfaceEdge::South));
        assert_eq!(at(1.0, 150.0), Some(SurfaceEdge::West));
        assert_eq!(at(399.0, 150.0), Some(SurfaceEdge::East));
        // Just inside the border is still the image, not a resize grab.
        assert_eq!(at(9.0, 150.0), None);
        assert_eq!(at(200.0, 291.0), None);
    }

    #[test]
    fn the_pointer_hides_with_the_overlay_but_never_over_a_resize_edge() {
        use super::cursor_name;
        // Overlay up: ordinary pointer.
        assert_eq!(cursor_name(None, true, true), None);
        // Overlay faded: gone, mpv-style.
        assert_eq!(cursor_name(None, false, true), Some("none"));
        // Opted out via config.
        assert_eq!(cursor_name(None, false, false), None);
        // An edge always wins — hiding the pointer on the resize border
        // would make a frameless window impossible to grab.
        assert_eq!(
            cursor_name(Some(SurfaceEdge::SouthEast), false, true),
            Some("se-resize")
        );
        assert_eq!(
            cursor_name(Some(SurfaceEdge::North), true, true),
            Some("n-resize")
        );
    }

    #[test]
    fn every_action_is_documented_and_reachable_by_key() {
        for (key, action) in DEFAULT_BINDS {
            assert!(
                ACTIONS.iter().any(|(name, _)| name == action),
                "default bind `{key}` names `{}`, which is not a known action",
                action.as_str()
            );
        }
        // Reached through the contextual arrow actions rather than a key
        // of their own, and still bindable by name (FR-8.2). Anything
        // else without a binding is unreachable by keyboard (FR-6.5) and
        // would show up in the cheat sheet with a blank key column.
        const REACHED_CONTEXTUALLY: &[Action] = &[Action::VolumeUp, Action::VolumeDown];

        for (action, description) in ACTIONS {
            assert!(
                DEFAULT_BINDS.iter().any(|(_, name)| name == action)
                    || REACHED_CONTEXTUALLY.contains(action),
                "action `{}` has no default binding",
                action.as_str()
            );
            assert!(
                !description.is_empty() || *action == Action::Escape,
                "action `{}` has no description for the cheat sheet",
                action.as_str()
            );
        }
    }

    #[test]
    fn configured_action_names_parse_only_at_the_boundary() {
        for (action, _) in ACTIONS {
            assert_eq!(Action::parse(action.as_str()), Some(*action));
        }
        assert_eq!(Action::parse("not-an-action"), None);
    }

    #[test]
    fn cheat_sheet_rows_align_regardless_of_markup_escaping() {
        use super::help_line;

        // What the user actually sees in the key column, entities
        // resolved back to the one character each stands for.
        let visible = |s: &str| {
            s.split("<tt>")
                .nth(1)
                .unwrap()
                .split("</tt>")
                .next()
                .unwrap()
                .replace("&lt;", "<")
                .replace("&gt;", ">")
                .replace("&amp;", "&")
                .chars()
                .count()
        };
        let plain = help_line("Right", "Next image", 22);
        let escaped = help_line("<Control>z", "Undo", 22);
        assert!(plain.ends_with("</tt> Next image"), "{plain}");
        assert!(
            escaped.contains("&lt;Control&gt;z"),
            "markup must be escaped: {escaped}"
        );
        assert_eq!(visible(&plain), 22);
        assert_eq!(
            visible(&escaped),
            22,
            "an escaped key must occupy the same column: {escaped}"
        );
    }

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
