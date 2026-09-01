//! Main window assembly: frameless surface with fade-in overlay
//! controls (FR-6), the single action layer every input goes through
//! (NFR-6.2), folder monitoring (FR-3.5), and the trash/undo/save
//! flows (FR-5).

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, SystemTime};

use gtk4 as gtk;

use gtk::gdk;
use gtk::gio;
use gtk::glib;
use gtk::glib::clone;
use gtk::prelude::*;

use crate::annotation::{MAX_SHAPES, Status as MarkupStatus, Tool as MarkupTool};
use crate::config::{self, Config, FitMode, SubtitleMode};
use crate::fileops;
use crate::folder::{Destination, FileSnapshot, Folder, Navigation, RemovalOutcome, RenameOutcome};
use crate::loader::{self, Decoded};
use crate::player::{
    self, AudioChoice, AudioSnapshot, PLAYBACK_RATES, Player, SubtitleChoice, SubtitleSnapshot,
    SubtitleTrack,
};
use crate::viewer::ImageView;

mod action;
use action::{Action, Command, Media, WorkspaceState};

const SEEK_STEP_SECONDS: f64 = 10.0;

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

#[derive(Debug, Clone, PartialEq, Eq)]
enum FsPresentation {
    Unchanged,
    Show(Destination),
    Empty,
}

#[derive(Debug)]
enum FsChange {
    Insert(FileSnapshot),
    Remove(PathBuf),
    Rename {
        old: PathBuf,
        new: PathBuf,
        snapshot: Option<FileSnapshot>,
    },
}

#[derive(Debug)]
struct PendingFsQuery {
    version: u64,
    cancellable: gio::Cancellable,
}

/// Per-path versions keep asynchronous metadata queries ordered without
/// retaining history after the last query completes.
#[derive(Debug, Default)]
struct FsQueryVersions {
    next: u64,
    paths: HashMap<PathBuf, PendingFsQuery>,
}

impl FsQueryVersions {
    fn start(&mut self, paths: &[PathBuf]) -> (u64, gio::Cancellable) {
        let version = self.next.wrapping_add(1);
        self.next = version;
        let cancellable = gio::Cancellable::new();
        for_each_unique_path(paths, |path| {
            if let Some(stale) = self.paths.insert(
                path.to_path_buf(),
                PendingFsQuery {
                    version,
                    cancellable: cancellable.clone(),
                },
            ) {
                stale.cancellable.cancel();
            }
        });
        (version, cancellable)
    }

    fn supersede(&mut self, paths: &[PathBuf]) {
        for_each_unique_path(paths, |path| {
            if let Some(stale) = self.paths.remove(path) {
                stale.cancellable.cancel();
            }
        });
    }

    fn finish(&mut self, paths: &[PathBuf], version: u64) -> bool {
        let current = paths.iter().all(|path| {
            self.paths
                .get(path)
                .is_some_and(|pending| pending.version == version)
        });
        for_each_unique_path(paths, |path| {
            if self
                .paths
                .get(path)
                .is_some_and(|pending| pending.version == version)
            {
                self.paths.remove(path);
            }
        });
        current
    }

    fn cancel_all(&mut self) {
        for pending in self.paths.values() {
            pending.cancellable.cancel();
        }
        self.paths.clear();
    }
}

fn for_each_unique_path(paths: &[PathBuf], mut f: impl FnMut(&Path)) {
    for (index, path) in paths.iter().enumerate() {
        if !paths[..index].contains(path) {
            f(path);
        }
    }
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
    navigation: RefCell<Navigation>,
    monitor: RefCell<Option<gio::FileMonitor>>,
    fs_queries: RefCell<FsQueryVersions>,
    media: RefCell<MediaState>,
    cache: loader::Cache,
    editable_mimes: RefCell<BTreeSet<String>>,
    /// Created on the first video (lazy GStreamer init, NFR-1.1) and
    /// reused; `None` also while videos have never been opened.
    player: RefCell<Option<Rc<Player>>>,
    pending_undo: RefCell<Option<PathBuf>>,
    saving: Cell<bool>,
    presented: Cell<bool>,
    // Widgets and timers.
    status_area: gtk::Box,
    status: gtk::Label,
    info_bar: gtk::Box,
    name_label: gtk::Label,
    pos_label: gtk::Label,
    prev_btn: gtk::Button,
    next_btn: gtk::Button,
    normal_controls: gtk::Box,
    photo_controls: gtk::Box,
    markup_btn: gtk::Button,
    markup_controls: gtk::Box,
    markup_box_btn: gtk::ToggleButton,
    markup_arrow_btn: gtk::ToggleButton,
    transport: gtk::Box,
    play_btn: gtk::Button,
    mute_btn: gtk::Button,
    speed_btn: gtk::MenuButton,
    speed_label: gtk::Label,
    speed_action: gio::SimpleAction,
    subtitle_btn: gtk::MenuButton,
    markup_context_menu: gio::Menu,
    audio_menu: gio::Menu,
    audio_context_menu: gio::Menu,
    audio_action: gio::SimpleAction,
    subtitle_menu: gio::Menu,
    subtitle_context_menu: gio::Menu,
    subtitle_action: gio::SimpleAction,
    save_btn: gtk::Button,
    /// (window width, transport shown, time length) the video controls
    /// were last fitted for.
    fitted_for: Cell<Option<(i32, bool, usize)>>,
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
    /// The always-visible empty/error actions also hold the cursor, but are
    /// tracked separately so hiding that state cannot leave the hold stuck.
    pointer_on_status: Cell<bool>,
    /// A popover is outside the bar's widget bounds, but it still owns the
    /// interaction: the bar must remain visible until the menu closes.
    menu_open: Cell<bool>,
    /// Last known pointer position, so the cursor can be re-decided when
    /// the overlay fades on a timer rather than on movement.
    pointer: Cell<(f64, f64)>,
    /// Session idle-inhibit cookie; `None` when nothing is held.
    inhibit_cookie: Cell<Option<NonZeroU32>>,
    /// Set before the window is allowed to close. GTK can retain this `App`
    /// through signal closures until process exit, so `Drop` is too late to
    /// own GStreamer shutdown (FR-6.7/NFR-2.2).
    shutting_down: Cell<bool>,
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
    markup_action: gio::SimpleAction,
    markup_box_action: gio::SimpleAction,
    markup_arrow_action: gio::SimpleAction,
    markup_copy_action: gio::SimpleAction,
    markup_clear_action: gio::SimpleAction,
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

        // Empty / error state (FR-1.4/1.5). These are real buttons rather
        // than a clickable label, so both choices have distinct accessible
        // names and normal keyboard focus.
        let status = gtk::Label::new(Some("Open a file or folder…"));
        status.set_wrap(true);
        status.set_justify(gtk::Justification::Center);
        status.add_css_class("status");
        let open_file_btn = gtk::Button::with_label("Open File…");
        open_file_btn.set_action_name(Some(&Action::OpenFile.detailed_name()));
        let open_folder_btn = gtk::Button::with_label("Open Folder…");
        open_folder_btn.set_action_name(Some(&Action::OpenFolder.detailed_name()));
        let status_actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        status_actions.set_halign(gtk::Align::Center);
        status_actions.add_css_class("status-actions");
        status_actions.append(&open_file_btn);
        status_actions.append(&open_folder_btn);
        let status_area = gtk::Box::new(gtk::Orientation::Vertical, 12);
        status_area.set_halign(gtk::Align::Center);
        status_area.set_valign(gtk::Align::Center);
        status_area.append(&status);
        status_area.append(&status_actions);
        overlay.add_overlay(&status_area);

        // Navigation arrows (FR-3.1).
        let prev_btn = osd_button(
            "go-previous-symbolic",
            &Action::Previous.detailed_name(),
            "Previous image",
        );
        prev_btn.set_halign(gtk::Align::Start);
        prev_btn.set_valign(gtk::Align::Center);
        overlay.add_overlay(&prev_btn);
        let next_btn = osd_button(
            "go-next-symbolic",
            &Action::Next.detailed_name(),
            "Next image",
        );
        next_btn.set_halign(gtk::Align::End);
        next_btn.set_valign(gtk::Align::Center);
        overlay.add_overlay(&next_btn);

        // Information belongs apart from actions: the filename and folder
        // position form a quiet top-left pill (FR-3.4, FR-6.2).
        let info_bar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        info_bar.add_css_class("osd-surface");
        info_bar.add_css_class("info-bar");
        info_bar.set_halign(gtk::Align::Start);
        info_bar.set_valign(gtk::Align::Start);
        info_bar.set_visible(false);
        let name_label = gtk::Label::new(None);
        name_label.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        name_label.set_max_width_chars(28);
        info_bar.append(&name_label);
        let pos_label = gtk::Label::new(None);
        pos_label.add_css_class("position");
        info_bar.append(&pos_label);
        overlay.add_overlay(&info_bar);

        // Window controls stay together in the opposite corner rather
        // than competing with media actions in the bottom bar (FR-6.3/6.7).
        let window_controls = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        window_controls.add_css_class("osd-surface");
        window_controls.add_css_class("window-controls");
        window_controls.set_halign(gtk::Align::End);
        window_controls.set_valign(gtk::Align::Start);
        let fullscreen_btn = bar_button(
            "view-fullscreen-symbolic",
            &Action::Fullscreen.detailed_name(),
            "Fullscreen",
        );
        window_controls.append(&fullscreen_btn);
        // Close button (FR-6.7).
        // Ask for the regular icon: some third-party themes ship a white
        // `-symbolic` source that GTK recolours to transparent.
        let close_btn = bar_button("window-close", &Action::Close.detailed_name(), "Close");
        window_controls.append(&close_btn);
        overlay.add_overlay(&window_controls);

        // Bottom media controls (FR-6.2). Photo and video actions occupy
        // the same place but never appear together.
        let bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        bar.add_css_class("osd-surface");
        bar.add_css_class("osd-bar");
        bar.set_halign(gtk::Align::Center);
        bar.set_valign(gtk::Align::End);
        let normal_controls = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        bar.append(&normal_controls);
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
            &Action::PlayPause.detailed_name(),
            "Play / pause",
        );
        let mute_btn = bar_button(
            "audio-volume-high-symbolic",
            &Action::Mute.detailed_name(),
            "Mute",
        );
        let speed_menu = playback_speed_menu();
        let speed_label = gtk::Label::new(Some("1×"));
        let speed_btn = gtk::MenuButton::builder()
            .child(&speed_label)
            .menu_model(&speed_menu)
            .tooltip_text("Playback speed")
            .build();
        speed_btn.add_css_class("flat");
        let subtitle_menu = gio::Menu::new();
        let subtitle_btn = gtk::MenuButton::builder()
            .icon_name("media-view-subtitles-symbolic")
            .menu_model(&subtitle_menu)
            .tooltip_text("Subtitles")
            .build();
        subtitle_btn.add_css_class("flat");
        subtitle_btn.set_visible(false);
        let transport = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        transport.append(&play_btn);
        transport.append(&seek_bar);
        transport.append(&time_label);
        transport.append(&speed_btn);
        transport.append(&subtitle_btn);
        transport.append(&mute_btn);
        transport.set_visible(false);
        normal_controls.append(&transport);

        let photo_controls = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let rotate_ccw = bar_button(
            "object-rotate-left-symbolic",
            &Action::RotateCounterclockwise.detailed_name(),
            "Rotate left",
        );
        let rotate_cw = bar_button(
            "object-rotate-right-symbolic",
            &Action::RotateClockwise.detailed_name(),
            "Rotate right",
        );
        photo_controls.append(&rotate_ccw);
        photo_controls.append(&rotate_cw);
        let markup_btn = bar_button(
            "document-edit-symbolic",
            &Action::Markup.detailed_name(),
            "Quick Markup",
        );
        photo_controls.append(&markup_btn);
        // Save appears only once an editable image has a pending rotation;
        // an inert floppy icon reads as a broken control (FR-5.4).
        let save_btn = bar_button(
            "document-save-symbolic",
            &Action::Save.detailed_name(),
            "Save rotation to file",
        );
        save_btn.set_visible(false);
        photo_controls.append(&save_btn);
        photo_controls.set_visible(false);
        normal_controls.append(&photo_controls);

        let separator = gtk::Separator::new(gtk::Orientation::Vertical);
        normal_controls.append(&separator);
        normal_controls.append(&bar_button(
            "user-trash-symbolic",
            &Action::Trash.detailed_name(),
            "Move to trash",
        ));

        // Less frequent commands remain discoverable without making the
        // primary strip permanent or wide (FR-6.5, NFR-5.2).
        let more_menu = gio::Menu::new();
        let open_menu = open_menu_model();
        more_menu.append_section(None, &open_menu);
        more_menu.append(
            Some("Fit to Window"),
            Some(&Action::ZoomFit.detailed_name()),
        );
        more_menu.append(
            Some("Actual Size"),
            Some(&Action::ZoomActual.detailed_name()),
        );
        more_menu.append(
            Some("Rotate Left"),
            Some(&Action::RotateCounterclockwise.detailed_name()),
        );
        more_menu.append(
            Some("Rotate Right"),
            Some(&Action::RotateClockwise.detailed_name()),
        );
        more_menu.append(Some("First File"), Some(&Action::First.detailed_name()));
        more_menu.append(Some("Last File"), Some(&Action::Last.detailed_name()));
        // Filled only for decoded static raster images (FR-11.1). A disabled
        // editing item on video or animation would imply future support.
        let markup_context_menu = gio::Menu::new();
        more_menu.append_section(None, &markup_context_menu);
        // Audio-track selection is contextual and appears only when the
        // current video exposes multiple tracks (FR-10.8).
        let audio_menu = gio::Menu::new();
        let audio_context_menu = gio::Menu::new();
        more_menu.append_section(None, &audio_context_menu);
        // Filled only while a video is active. The same subtitle model is
        // shared with the CC button, so right-click and the transport never
        // disagree about available tracks (FR-10.7).
        let subtitle_context_menu = gio::Menu::new();
        more_menu.append_section(None, &subtitle_context_menu);
        more_menu.append(
            Some("Keyboard Shortcuts"),
            Some(&Action::Help.detailed_name()),
        );
        let more_btn = gtk::MenuButton::builder()
            .icon_name("view-more-symbolic")
            .menu_model(&more_menu)
            .tooltip_text("More controls")
            .build();
        more_btn.add_css_class("flat");
        normal_controls.append(&more_btn);

        // Quick Markup is a focused mode: replace the normal controls so
        // navigation and file operations are not sitting beside a drawing
        // gesture that temporarily changes what primary drag means.
        let markup_controls = gtk::Box::new(gtk::Orientation::Vertical, 4);
        markup_controls.add_css_class("markup-controls");
        let markup_tools = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        markup_tools.set_halign(gtk::Align::Center);
        let markup_decisions = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        markup_decisions.set_halign(gtk::Align::Center);
        let markup_box_btn = gtk::ToggleButton::with_label("Box");
        markup_box_btn.set_action_name(Some(&Action::MarkupBox.detailed_name()));
        markup_box_btn.set_tooltip_text(Some("Draw a box (B)"));
        markup_box_btn.add_css_class("flat");
        markup_box_btn.set_active(true);
        let markup_arrow_btn = gtk::ToggleButton::with_label("Arrow");
        markup_arrow_btn.set_group(Some(&markup_box_btn));
        markup_arrow_btn.set_action_name(Some(&Action::MarkupArrow.detailed_name()));
        markup_arrow_btn.set_tooltip_text(Some("Draw an arrow (Shift+A)"));
        markup_arrow_btn.add_css_class("flat");
        markup_tools.append(&markup_box_btn);
        markup_tools.append(&markup_arrow_btn);
        markup_tools.append(&bar_button(
            "edit-undo-symbolic",
            &Action::Undo.detailed_name(),
            "Undo last annotation (Ctrl+Z)",
        ));
        markup_decisions.append(&bar_button(
            "edit-clear-all-symbolic",
            &Action::MarkupClear.detailed_name(),
            "Clear all annotations; Ctrl+Z restores them (C)",
        ));
        markup_decisions.append(&bar_button(
            "process-stop-symbolic",
            &Action::Markup.detailed_name(),
            "Cancel Quick Markup (A or Escape)",
        ));
        let copy_markup_btn = bar_button(
            "edit-copy-symbolic",
            &Action::MarkupCopy.detailed_name(),
            "Copy annotated image (Ctrl+C)",
        );
        copy_markup_btn.remove_css_class("flat");
        copy_markup_btn.add_css_class("suggested-action");
        markup_decisions.append(&copy_markup_btn);
        markup_controls.append(&markup_tools);
        markup_controls.append(&markup_decisions);
        markup_controls.set_visible(false);
        bar.append(&markup_controls);

        // The same commands are also a contextual menu on the medium.
        // It is parented to the view so its pointing rectangle uses the
        // secondary-click coordinates directly.
        let context_menu = gtk::PopoverMenu::from_model(Some(&more_menu));
        context_menu.set_parent(&view);
        bar.set_visible(false);
        overlay.add_overlay(&bar);

        // Zoom / edge-cue indicator (FR-4.4, FR-3.3).
        let indicator = gtk::Label::new(None);
        indicator.add_css_class("indicator");
        indicator.add_css_class("invisible");
        indicator.set_halign(gtk::Align::Center);
        indicator.set_valign(gtk::Align::Start);
        indicator.set_can_target(false);
        overlay.add_overlay(&indicator);

        // Toast with undo (FR-5.2).
        let toast_label = gtk::Label::new(None);
        let toast_undo = gtk::Button::with_label("Undo");
        toast_undo.set_action_name(Some(&Action::Undo.detailed_name()));
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
            info_bar.upcast_ref(),
            window_controls.upcast_ref(),
            bar.upcast_ref(),
        ] {
            w.add_css_class("chrome");
            w.add_css_class("invisible");
            w.set_can_target(false);
        }

        let subtitle_action = gio::SimpleAction::new_stateful(
            Action::SubtitleSelect.name(),
            Some(glib::VariantTy::STRING),
            &"auto".to_variant(),
        );
        let audio_action = gio::SimpleAction::new_stateful(
            Action::AudioSelect.name(),
            Some(glib::VariantTy::STRING),
            &"auto".to_variant(),
        );
        let speed_action = gio::SimpleAction::new_stateful(
            Action::SpeedSet.name(),
            Some(glib::VariantTy::DOUBLE),
            &1.0_f64.to_variant(),
        );
        let app = Rc::new(App {
            win: win.clone(),
            view: view.clone(),
            navigation: RefCell::new(Navigation::default()),
            monitor: RefCell::new(None),
            fs_queries: RefCell::new(FsQueryVersions::default()),
            media: RefCell::new(MediaState::Empty),
            cache: loader::Cache::new(3, cache_budget_bytes(cfg.cache_budget_mb)),
            editable_mimes: RefCell::new(BTreeSet::new()),
            player: RefCell::new(None),
            pending_undo: RefCell::new(None),
            saving: Cell::new(false),
            presented: Cell::new(false),
            status_area,
            status,
            info_bar: info_bar.clone(),
            name_label,
            pos_label,
            prev_btn: prev_btn.clone(),
            next_btn: next_btn.clone(),
            normal_controls,
            photo_controls,
            markup_btn,
            markup_controls,
            markup_box_btn,
            markup_arrow_btn,
            transport,
            play_btn,
            mute_btn,
            speed_btn: speed_btn.clone(),
            speed_label,
            speed_action,
            subtitle_btn: subtitle_btn.clone(),
            markup_context_menu,
            audio_menu,
            audio_context_menu,
            audio_action,
            subtitle_menu,
            subtitle_context_menu,
            subtitle_action,
            save_btn,
            fitted_for: Cell::new(None),
            control_bar: bar.clone(),
            seek_bar,
            time_label,
            transport_tick: RefCell::new(None),
            scrubbing: Cell::new(false),
            pointer_on_chrome: Cell::new(false),
            pointer_on_status: Cell::new(false),
            menu_open: Cell::new(false),
            pointer: Cell::new((0.0, 0.0)),
            inhibit_cookie: Cell::new(None),
            shutting_down: Cell::new(false),
            sized_from_media: Cell::new(false),
            indicator,
            toast_revealer,
            toast_label,
            toast_undo,
            help_label,
            chrome: vec![
                prev_btn.upcast(),
                next_btn.upcast(),
                info_bar.upcast(),
                window_controls.upcast(),
                bar.upcast(),
            ],
            chrome_timer: TimerSlot::default(),
            indicator_timer: TimerSlot::default(),
            toast_timer: TimerSlot::default(),
            svg_timer: TimerSlot::default(),
            markup_action: gio::SimpleAction::new(Action::Markup.name(), None),
            markup_box_action: gio::SimpleAction::new(Action::MarkupBox.name(), None),
            markup_arrow_action: gio::SimpleAction::new(Action::MarkupArrow.name(), None),
            markup_copy_action: gio::SimpleAction::new(Action::MarkupCopy.name(), None),
            markup_clear_action: gio::SimpleAction::new(Action::MarkupClear.name(), None),
            save_action: gio::SimpleAction::new(Action::Save.name(), None),
            undo_action: gio::SimpleAction::new(Action::Undo.name(), None),
            cfg,
        });

        app.setup_actions(gtk_app);
        app.setup_controllers();
        app.build_help(gtk_app);

        // Every close route eventually emits close-request, including the
        // window manager and the typed close/escape actions. Tear playback
        // down while GTK's main loop and GStreamer libraries are still fully
        // alive; waiting for `Player::drop` can race process-wide destructors.
        app.win.connect_close_request(clone!(
            #[strong(rename_to = app)]
            app,
            move |_| {
                app.shutdown();
                glib::Propagation::Proceed
            }
        ));

        more_btn.connect_active_notify(clone!(
            #[strong(rename_to = app)]
            app,
            move |button| {
                app.menu_open.set(button.is_active());
                // Opening can move the pointer onto the popover surface
                // before GTK reports it active, briefly firing the window
                // leave handler. Reveal again after that race; while open,
                // menu_open blocks every later fade. Closing starts a fresh
                // timeout in the same call.
                app.show_chrome();
            }
        ));
        subtitle_btn.connect_active_notify(clone!(
            #[strong(rename_to = app)]
            app,
            move |button| {
                app.menu_open.set(button.is_active());
                app.show_chrome();
            }
        ));
        speed_btn.connect_active_notify(clone!(
            #[strong(rename_to = app)]
            app,
            move |button| {
                app.menu_open.set(button.is_active());
                app.show_chrome();
            }
        ));
        context_menu.connect_visible_notify(clone!(
            #[strong(rename_to = app)]
            app,
            move |menu| {
                app.menu_open.set(menu.is_visible());
                app.show_chrome();
            }
        ));

        let context_click = gtk::GestureClick::new();
        context_click.set_button(gdk::BUTTON_SECONDARY);
        context_click.connect_pressed(clone!(
            #[strong(rename_to = app)]
            app,
            #[weak]
            context_menu,
            move |gesture, _, x, y| {
                let showing_media = matches!(
                    *app.media.borrow(),
                    MediaState::Image { .. } | MediaState::Video(_)
                );
                if !showing_media || app.view.is_marking_up() {
                    return;
                }
                gesture.set_state(gtk::EventSequenceState::Claimed);
                context_menu.set_pointing_to(Some(&gdk::Rectangle::new(
                    window_dimension(x, 0),
                    window_dimension(y, 0),
                    1,
                    1,
                )));
                context_menu.popup();
            }
        ));
        view.add_controller(context_click);

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
        app.view.connect_annotation_changed(clone!(
            #[strong]
            app,
            move |status| app.on_markup_changed(status)
        ));
        app.view.connect_navigate(clone!(
            #[strong]
            app,
            move |dir| {
                app.dispatch_action(if dir > 0 {
                    Action::Next
                } else {
                    Action::Previous
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
                    self.install_folder(folder);
                    let idx = self.navigation.borrow().index_of(&path);
                    match idx {
                        Some(idx) => self.show_index(idx, Arrival::Direct),
                        None => self.show_error(&path, &excluded_path_message(&path)),
                    }
                }
                Err(e) => self.show_error(&path, &format!("cannot read directory: {e}")),
            }
        }
    }

    fn choose_media_file(self: &Rc<Self>) {
        let filter = supported_media_filter();
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter);

        let dialog = gtk::FileDialog::new();
        dialog.set_title("Open File");
        dialog.set_accept_label(Some("Open"));
        dialog.set_modal(true);
        dialog.set_filters(Some(&filters));
        dialog.set_default_filter(Some(&filter));
        if let Some(folder) = self.dialog_initial_folder() {
            dialog.set_initial_folder(Some(&folder));
        }
        dialog.open(
            Some(&self.win),
            gio::Cancellable::NONE,
            clone!(
                #[strong(rename_to = app)]
                self,
                move |result| match result {
                    Ok(file) => match file.path() {
                        Some(path) if config::is_supported(&path) => app.open_path(&path),
                        Some(_) =>
                            app.show_toast("Only supported media files can be opened", false),
                        None => app.show_toast("Only local media files can be opened", false),
                    },
                    Err(error) if dialog_was_cancelled(&error) => {}
                    Err(error) => {
                        app.show_toast(&format!("Cannot open file chooser: {error}"), false)
                    }
                }
            ),
        );
    }

    fn choose_folder(self: &Rc<Self>) {
        let dialog = gtk::FileDialog::new();
        dialog.set_title("Open Folder");
        dialog.set_accept_label(Some("Open"));
        dialog.set_modal(true);
        if let Some(folder) = self.dialog_initial_folder() {
            dialog.set_initial_folder(Some(&folder));
        }
        dialog.select_folder(
            Some(&self.win),
            gio::Cancellable::NONE,
            clone!(
                #[strong(rename_to = app)]
                self,
                move |result| match result {
                    Ok(folder) => match folder.path() {
                        Some(path) => app.open_path(&path),
                        None => app.show_toast("Only local folders can be opened", false),
                    },
                    Err(error) if dialog_was_cancelled(&error) => {}
                    Err(error) => {
                        app.show_toast(&format!("Cannot open folder chooser: {error}"), false)
                    }
                }
            ),
        );
    }

    /// Start from the current media's directory. With no current path the
    /// initial folder stays unset, leaving location memory to the portal.
    fn dialog_initial_folder(&self) -> Option<gio::File> {
        let current = self.current_path();
        dialog_initial_folder_path(current.as_deref()).map(gio::File::for_path)
    }

    fn open_folder(self: &Rc<Self>, dir: &Path) {
        match Folder::scan(dir, self.cfg.sort) {
            Ok(folder) if !folder.is_empty() => {
                self.install_folder(folder);
                self.show_index(0, Arrival::Direct);
            }
            Ok(folder) => {
                self.install_folder(folder);
                self.show_error(dir, "no supported media in this folder");
            }
            Err(e) => self.show_error(dir, &format!("cannot read directory: {e}")),
        }
    }

    fn install_folder(self: &Rc<Self>, folder: Folder) {
        let len = folder.len();
        self.fs_queries.borrow_mut().cancel_all();
        let directory = {
            let mut navigation = self.navigation.borrow_mut();
            navigation.install(folder);
            navigation.directory().map(Path::to_path_buf)
        };
        let Some(directory) = directory else {
            return;
        };
        crate::applog!("folder: {} with {} media files", directory.display(), len);
        let monitor = gio::File::for_path(&directory)
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
        let destination = self.navigation.borrow_mut().select(idx);
        if let Some(destination) = destination {
            self.show_destination(destination, arrival);
        }
    }

    fn show_destination(self: &Rc<Self>, destination: Destination, arrival: Arrival) {
        if self.view.cancel_markup() {
            self.update_cursor();
        }
        let Destination {
            index: idx,
            path,
            generation,
        } = destination;
        *self.media.borrow_mut() = MediaState::Loading(path.clone());
        self.update_control_mode();
        self.cache.pin(&path);
        self.set_current_name(Some(&path));
        self.update_pos_label();
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
                            if app.navigation.borrow().is_current_generation(generation) {
                                app.apply_decoded(path.clone(), decoded, mime, generation);
                            } else {
                                crate::applog!(
                                    "show: {} superseded, kept in cache",
                                    path.display()
                                );
                            }
                        }
                        Err(e) => {
                            if app.navigation.borrow().is_current_generation(generation) {
                                app.on_decode_failed(&path, &e.to_string(), arrival);
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
    fn on_decode_failed(self: &Rc<Self>, path: &Path, message: &str, arrival: Arrival) {
        let Arrival::Step { direction, budget } = arrival else {
            self.show_error(path, message);
            return;
        };
        let next = {
            let navigation = self.navigation.borrow();
            navigation
                .current_index()
                .and_then(|index| skip_target(&navigation, index, arrival, self.cfg.wrap))
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
                p.set_subtitles_default(self.cfg.subtitles == SubtitleMode::Auto);
                *self.player.borrow_mut() = Some(p.clone());
                Ok(p)
            }
            Err(e) => Err(e),
        }
    }

    fn show_video(self: &Rc<Self>, path: &Path) {
        // Reuse the shared pipeline for ordinary video navigation. External
        // text pads are the exception: playbin3 can retain their playsink
        // ownership across Null and intermittently connect the next text pad
        // before video. A fresh pipeline keeps FR-10.7 deterministic without
        // moving GStreamer initialization onto image startup.
        let replace_player =
            self.player.borrow().as_ref().is_some_and(|player| {
                player.has_external_subtitle() || Player::path_has_sidecar(path)
            });
        if replace_player && let Some(player) = self.player.borrow_mut().take() {
            player.stop();
        }
        let player = match self.player() {
            Ok(player) => player,
            Err(e) => {
                self.show_error(path, &e.to_string());
                return;
            }
        };
        self.hide_status();
        self.view.show_live_paintable(player.paintable());
        if let Err(e) = player.play(path) {
            self.show_error(path, &e.to_string());
            return;
        }
        self.update_subtitles(player.subtitle_snapshot());
        self.update_audio(player.audio_snapshot());
        self.update_playback_rate(player.playback_rate());
        *self.media.borrow_mut() = MediaState::Video(path.to_path_buf());
        self.update_control_mode();
        crate::applog!("play: {}", path.display());
        self.set_idle_inhibited(true);
        self.update_save_enabled();
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
        let discard_player = self
            .player
            .borrow()
            .as_ref()
            .is_some_and(|player| player.has_external_subtitle());
        let snapshots = if let Some(p) = self.player.borrow().as_ref() {
            p.stop();
            Some((p.audio_snapshot(), p.subtitle_snapshot()))
        } else {
            None
        };
        if let Some((audio, subtitles)) = snapshots {
            self.update_audio(audio);
            self.update_subtitles(subtitles);
        }
        if discard_player {
            self.player.borrow_mut().take();
        }
        self.set_idle_inhibited(false);
        // Hiding the bar mid-drag means no button release reaches it.
        self.scrubbing.set(false);
        self.stop_transport_tick();
    }

    /// Finish session-owned work before GTK destroys the last window.
    ///
    /// `App` participates in GTK signal cycles, so its `Player` is not
    /// guaranteed to drop before C library finalizers run. An active
    /// GStreamer streaming thread racing those finalizers aborts in GLib or
    /// a GPU driver. Close-request is the last deterministic point at which
    /// the GTK main loop, bus watch and pipeline are all still usable.
    fn shutdown(&self) {
        if self.shutting_down.replace(true) {
            return;
        }

        let started = std::time::Instant::now();
        crate::applog!("shutdown: started");

        // Invalidate every outstanding media result before releasing its
        // sources, then prevent monitors and timers from scheduling more UI
        // work while the close request proceeds.
        self.navigation.borrow_mut().supersede();
        self.monitor.borrow_mut().take();
        self.fs_queries.borrow_mut().cancel_all();
        self.chrome_timer.cancel();
        self.indicator_timer.cancel();
        self.toast_timer.cancel();
        self.svg_timer.cancel();
        self.stop_transport_tick();
        self.pending_undo.borrow_mut().take();
        self.set_idle_inhibited(false);

        // Taking the player also drops its bus-watch guard after stop reaches
        // Null, so no streaming callback can outlive process shutdown.
        if let Some(player) = self.player.borrow_mut().take() {
            player.stop();
        }

        crate::applog!(
            "shutdown: complete in {:.1} ms",
            started.elapsed().as_secs_f64() * 1000.0
        );
    }

    fn attach_subtitle(self: &Rc<Self>, path: &Path) {
        if !self.is_video_showing() {
            self.show_toast("Open a video before adding subtitles", false);
            return;
        }
        let Some(player) = self.player.borrow().clone() else {
            self.show_toast("Video player is unavailable", false);
            return;
        };
        match player.attach_subtitle(path) {
            Ok(()) => {
                self.update_subtitles(player.subtitle_snapshot());
                let name = path.file_name().unwrap_or_default().to_string_lossy();
                self.flash(&format!("Subtitles: {name}"));
            }
            Err(error) => self.show_toast(&error.to_string(), false),
        }
    }

    fn choose_external_subtitle(self: &Rc<Self>) {
        let video = match &*self.media.borrow() {
            MediaState::Video(path) => path.clone(),
            _ => {
                self.show_toast("Open a video before adding subtitles", false);
                return;
            }
        };
        let filter = gtk::FileFilter::new();
        filter.set_name(Some("SRT and WebVTT subtitles"));
        filter.add_suffix("srt");
        filter.add_suffix("vtt");
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter);

        let dialog = gtk::FileDialog::new();
        dialog.set_title("Add External Subtitle");
        dialog.set_accept_label(Some("Add Subtitle"));
        dialog.set_modal(true);
        dialog.set_filters(Some(&filters));
        dialog.set_default_filter(Some(&filter));
        if let Some(parent) = video.parent() {
            dialog.set_initial_folder(Some(&gio::File::for_path(parent)));
        }
        dialog.open(
            Some(&self.win),
            gio::Cancellable::NONE,
            clone!(
                #[strong(rename_to = app)]
                self,
                move |result| match result {
                    Ok(file) => match file.path() {
                        Some(path) if config::is_subtitle(&path) => {
                            if !app.is_video_showing()
                                || app.current_path().as_deref() != Some(video.as_path())
                            {
                                app.show_toast(
                                    "The video changed; choose its subtitle again",
                                    false,
                                );
                                return;
                            }
                            app.attach_subtitle(&path);
                        }
                        Some(_) =>
                            app.show_toast("Only SRT and WebVTT subtitles are supported", false,),
                        None => app.show_toast("Only local subtitle files are supported", false),
                    },
                    Err(error) if dialog_was_cancelled(&error) => {}
                    Err(error) => app.show_toast(&format!("Cannot open subtitle: {error}"), false),
                }
            ),
        );
    }

    fn handle_dropped_files(self: &Rc<Self>, files: Vec<gio::File>) -> bool {
        if files.len() != 1 {
            self.show_toast("Drop one file at a time", false);
            return true;
        }
        let Some(path) = files[0].path() else {
            self.show_toast("Only local files can be dropped", false);
            return true;
        };
        if config::is_subtitle(&path) {
            self.attach_subtitle(&path);
        } else if looks_like_subtitle(&path) {
            self.show_toast("Only SRT and WebVTT subtitles are supported", false);
        } else {
            self.open_path(&path);
        }
        true
    }

    // ----- video transport (FR-10.5) ------------------------------------

    /// Keep video controls inside the window. The seek bar gives up width
    /// first; on very narrow windows the duplicated time and mute controls
    /// and the CC/speed menus yield before play, Trash, or the seek target
    /// (FR-10.5/10.7).
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
        );
        if self.fitted_for.get() == Some(key) {
            return;
        }
        self.fitted_for.set(Some(key));
        let (width, video, ..) = key;
        if !video {
            return;
        }

        // Measure from all video controls restored, with the seek bar
        // contributing nothing, so `others` is everything around it.
        self.seek_bar.set_size_request(0, -1);
        self.time_label.set_visible(true);
        self.mute_btn.set_visible(true);
        self.speed_btn.set_visible(true);
        self.subtitle_btn.set_visible(true);
        let others = |s: &Self| s.control_bar.measure(gtk::Orientation::Horizontal, -1).0;

        let mut room = width - others(self);
        if room < SEEK_BAR_MIN_WIDTH {
            self.time_label.set_visible(false);
            room = width - others(self);
        }
        if room < SEEK_BAR_MIN_WIDTH {
            self.mute_btn.set_visible(false);
            room = width - others(self);
        }
        if room < SEEK_BAR_MIN_WIDTH {
            self.subtitle_btn.set_visible(false);
            room = width - others(self);
        }
        if room < SEEK_BAR_MIN_WIDTH {
            self.speed_btn.set_visible(false);
            room = width - others(self);
        }
        self.seek_bar
            .set_size_request(room.clamp(0, SEEK_BAR_MAX_WIDTH), -1);
    }

    fn update_subtitles(&self, snapshot: SubtitleSnapshot) {
        rebuild_subtitle_menu(&self.subtitle_menu, &snapshot);
        self.subtitle_action
            .set_state(&snapshot.choice.action_target().to_variant());
        self.fitted_for.set(None);
    }

    fn update_audio(&self, snapshot: AudioSnapshot) {
        rebuild_audio_menu(&self.audio_menu, &snapshot);
        rebuild_audio_context(
            &self.audio_context_menu,
            &self.audio_menu,
            self.is_video_showing(),
            snapshot.tracks.len(),
        );
        self.audio_action
            .set_state(&snapshot.choice.action_target().to_variant());
    }

    fn flash_audio_choice(self: &Rc<Self>, snapshot: &AudioSnapshot) {
        let label = match &snapshot.choice {
            AudioChoice::Automatic => snapshot.active_label.as_deref().unwrap_or("Automatic"),
            AudioChoice::Track(id) => snapshot
                .tracks
                .iter()
                .find(|track| track.id == *id)
                .map_or("Audio changed", |track| track.label.as_str()),
        };
        self.flash(&format!("Audio: {label}"));
    }

    fn flash_subtitle_choice(self: &Rc<Self>, snapshot: &SubtitleSnapshot) {
        let text = match &snapshot.choice {
            SubtitleChoice::Off => "Subtitles off".to_string(),
            SubtitleChoice::Automatic => snapshot.active_label.as_ref().map_or_else(
                || "Subtitles: Automatic".to_string(),
                |label| format!("Subtitles: {label}"),
            ),
            SubtitleChoice::Track(id) => snapshot
                .tracks
                .iter()
                .find(|track| track.id == *id)
                .map_or_else(
                    || "Subtitles changed".to_string(),
                    |track| format!("Subtitles: {}", track.label),
                ),
        };
        self.flash(&text);
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
            self.update_playback_rate(p.playback_rate());
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
        resize_edge_at(
            x,
            y,
            f64::from(self.win.width()),
            f64::from(self.win.height()),
        )
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
            self.view.is_marking_up()
                && self.view.contains_image_point(x, y)
                && !self.pointer_on_chrome.get()
                && !self.pointer_on_status.get(),
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

    fn markup_available(&self) -> bool {
        matches!(
            &*self.media.borrow(),
            MediaState::Image { decoded, .. } if matches!(&**decoded, Decoded::Static { .. })
        )
    }

    /// Only controls relevant to the current medium occupy the bottom
    /// strip. Loading and error states leave the image unobstructed.
    fn update_control_mode(&self) {
        let media = self.media.borrow();
        let photo = matches!(*media, MediaState::Image { .. });
        let video = matches!(*media, MediaState::Video(_));
        drop(media);

        let marking = self.view.is_marking_up();
        let markup_available = self.markup_available();
        let status = self.view.markup_status();
        let has_shapes = status.is_some_and(|status| status.shape_count > 0);
        let can_copy = has_shapes && !self.view.markup_has_draft();

        self.normal_controls.set_visible(!marking);
        self.photo_controls.set_visible(photo && !marking);
        self.transport.set_visible(video && !marking);
        self.subtitle_btn.set_visible(video && !marking);
        self.markup_btn.set_visible(markup_available);
        self.markup_controls.set_visible(marking);
        self.toast_revealer
            .set_margin_bottom(if marking { 100 } else { 56 });
        self.prev_btn.set_visible(!marking);
        self.next_btn.set_visible(!marking);
        rebuild_markup_context(&self.markup_context_menu, markup_available);
        let audio_track_count = self
            .with_video(Player::audio_snapshot)
            .map_or(0, |snapshot| snapshot.tracks.len());
        rebuild_audio_context(
            &self.audio_context_menu,
            &self.audio_menu,
            video,
            audio_track_count,
        );
        rebuild_subtitle_context(&self.subtitle_context_menu, &self.subtitle_menu, video);
        self.control_bar.set_visible(photo || video || marking);
        self.markup_action.set_enabled(markup_available || marking);
        self.markup_box_action.set_enabled(marking);
        self.markup_arrow_action.set_enabled(marking);
        self.markup_copy_action.set_enabled(marking && can_copy);
        self.markup_clear_action.set_enabled(marking && has_shapes);
        if let Some(status) = status {
            self.markup_box_btn
                .set_active(status.tool == MarkupTool::Box);
            self.markup_arrow_btn
                .set_active(status.tool == MarkupTool::Arrow);
        }
        self.update_undo_enabled();
        self.fitted_for.set(None);
    }

    fn toggle_markup(self: &Rc<Self>) {
        if self.view.cancel_markup() {
            self.update_control_mode();
            self.show_chrome();
            self.update_cursor();
            return;
        }
        if !self.markup_available() || !self.view.start_markup() {
            self.show_toast("Quick Markup is available for static images only", false);
            return;
        }
        // A previous trash toast describes another workflow. Keep its
        // timed restoration state, but remove the visual competition with
        // the focused markup toolbar.
        self.toast_revealer.set_reveal_child(false);
        self.toast_undo.set_visible(false);
        self.update_control_mode();
        self.show_chrome();
        self.update_cursor();
    }

    fn on_markup_changed(self: &Rc<Self>, status: MarkupStatus) {
        self.update_control_mode();
        self.show_chrome();
        if status.shape_count == MAX_SHAPES {
            self.show_toast("Quick Markup limit reached; undo or clear a shape", false);
        }
    }

    fn copy_markup(self: &Rc<Self>) {
        let started = std::time::Instant::now();
        match self.view.annotated_texture() {
            Ok(texture) => {
                let (width, height) = (texture.width(), texture.height());
                self.win.clipboard().set_texture(&texture);
                self.view.cancel_markup();
                self.update_control_mode();
                self.update_cursor();
                crate::applog!(
                    "markup: copied {}x{} in {:.1} ms",
                    width,
                    height,
                    started.elapsed().as_secs_f64() * 1000.0
                );
                self.show_toast("Annotated image copied", false);
            }
            Err(error) => self.show_toast(&error.to_string(), false),
        }
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
            player::Event::SubtitleError(description) => {
                if self.is_video_showing() {
                    self.show_toast(&description, false);
                }
            }
            player::Event::AudioChanged(snapshot) => {
                if self.is_video_showing() {
                    self.update_audio(snapshot);
                }
            }
            player::Event::SubtitlesChanged(snapshot) => {
                if self.is_video_showing() {
                    self.update_subtitles(snapshot);
                }
            }
            player::Event::PlaybackRateError(error) => {
                if self.is_video_showing() {
                    if let Some(rate) = self.with_video(Player::playback_rate) {
                        self.update_playback_rate(rate);
                    }
                    self.show_toast(&error.to_string(), false);
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
        self.hide_status();
        *self.media.borrow_mut() = MediaState::Image {
            path,
            decoded: decoded.clone(),
            mime,
        };
        self.update_control_mode();
        let texture = decoded.first_texture();
        let size = match &*decoded {
            Decoded::Svg { nominal, .. } => {
                self.view.show_texture(texture, Some(*nominal));
                *nominal
            }
            _ => {
                self.view.show_texture(texture.clone(), None);
                (f64::from(texture.width()), f64::from(texture.height()))
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
                        if !app.navigation.borrow().is_current_generation(generation) {
                            return;
                        }
                    }
                    let Ok(frame) = image.next_frame().await else {
                        break;
                    };
                    if !app.navigation.borrow().is_current_generation(generation) {
                        break;
                    }
                    app.view.update_texture(frame.texture());
                    let delay = frame.delay().unwrap_or(Duration::from_millis(100));
                    glib::timeout_future(delay).await;
                    if !app.navigation.borrow().is_current_generation(generation) {
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
        let generation = self.navigation.borrow().generation();
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
                    let w = svg_render_dimension(nominal.0 * zoom);
                    let h = svg_render_dimension(nominal.1 * zoom);
                    glib::spawn_future_local(clone!(
                        #[strong]
                        app,
                        async move {
                            let Decoded::Svg { image, .. } = &*decoded else {
                                return;
                            };
                            let started = std::time::Instant::now();
                            let request = glycin::FrameRequest::new().scale(w, h);
                            if let Ok(frame) = image.specific_frame(request).await
                                && app.navigation.borrow().is_current_generation(generation)
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
            let navigation = self.navigation.borrow();
            [idx.checked_sub(1), Some(idx + 1)]
                .into_iter()
                .flatten()
                .filter_map(|i| navigation.get(i))
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
        self.navigation.borrow_mut().supersede();
        self.stop_video();
        self.view.clear();
        *self.media.borrow_mut() = MediaState::Empty;
        self.update_control_mode();
    }

    /// Put a message where the image would be (FR-1.4).
    fn show_status(&self, text: &str) {
        self.status.set_text(text);
        self.status_area.set_visible(true);
        self.update_save_enabled();
    }

    fn hide_status(&self) {
        self.status_area.set_visible(false);
        self.pointer_on_status.set(false);
    }

    fn show_error(self: &Rc<Self>, path: &Path, message: &str) {
        eprintln!("open-mpv: error: {}: {message}", path.display());
        let belongs_to_destination = self.navigation.borrow().current_path() == Some(path);
        self.clear_media();
        if !belongs_to_destination {
            self.navigation.borrow_mut().clear_current();
        }
        // MediaState retains direct error paths for display and chooser
        // context; Navigation retains its position only when this path is
        // the selected destination.
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
        self.navigation.borrow_mut().clear_current();
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
            (mw, mh) = (f64::from(geo.width()), f64::from(geo.height()));
        }
        let (cap_w, cap_h) = (mw * 0.85, mh * 0.85);
        let s = (cap_w / size.0).min(cap_h / size.1).min(1.0);
        self.win.set_default_size(
            window_dimension(size.0 * s, 200),
            window_dimension(size.1 * s, 150),
        );
        self.sized_from_media.set(true);
        crate::applog!("window sized to media {}x{}", size.0, size.1);
    }

    // ----- navigation ---------------------------------------------------

    fn current_path(&self) -> Option<PathBuf> {
        let navigation_path = self
            .navigation
            .borrow()
            .current_path()
            .map(Path::to_path_buf);
        navigation_path.or_else(|| self.media.borrow().path().map(Path::to_path_buf))
    }

    fn current_index(&self) -> Option<usize> {
        self.navigation.borrow().current_index()
    }

    fn navigate(self: &Rc<Self>, direction: Direction) {
        let target = {
            let navigation = self.navigation.borrow();
            match direction {
                Direction::Next => navigation.next(self.cfg.wrap),
                Direction::Previous => navigation.prev(self.cfg.wrap),
            }
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
        self.info_bar.set_visible(name.is_some());
        self.win
            .set_title(Some(name.as_deref().unwrap_or("open-mpv")));
    }

    fn update_pos_label(&self) {
        let text = {
            let navigation = self.navigation.borrow();
            navigation
                .current_index()
                .filter(|_| !navigation.is_empty())
                .map_or_else(String::new, |index| position_text(index, navigation.len()))
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
        match event {
            E::Created | E::MovedIn => {
                let Some(path) = file.path().filter(|path| config::is_supported(path)) else {
                    return;
                };
                self.query_fs_snapshot(file.clone(), vec![path], move |app, snapshot| {
                    if let Some(snapshot) = snapshot {
                        app.apply_fs_change(FsChange::Insert(snapshot), event);
                    }
                });
            }
            E::Deleted | E::MovedOut => {
                let Some(path) = file.path() else {
                    return;
                };
                self.fs_queries
                    .borrow_mut()
                    .supersede(std::slice::from_ref(&path));
                self.apply_fs_change(FsChange::Remove(path), event);
            }
            E::Renamed => {
                let Some((old, new_file, new)) = file.path().and_then(|old| {
                    let new_file = other?.clone();
                    let new = new_file.path()?;
                    Some((old, new_file, new))
                }) else {
                    return;
                };
                let paths = vec![old.clone(), new.clone()];
                if config::is_supported(&new) {
                    self.query_fs_snapshot(new_file, paths, move |app, snapshot| {
                        app.apply_fs_change(FsChange::Rename { old, new, snapshot }, event);
                    });
                } else {
                    self.fs_queries.borrow_mut().supersede(&paths);
                    self.apply_fs_change(
                        FsChange::Rename {
                            old,
                            new,
                            snapshot: None,
                        },
                        event,
                    );
                }
            }
            _ => {}
        }
    }

    fn query_fs_snapshot(
        self: &Rc<Self>,
        file: gio::File,
        paths: Vec<PathBuf>,
        apply: impl FnOnce(&Rc<Self>, Option<FileSnapshot>) + 'static,
    ) {
        let Some(directory) = self.navigation.borrow().directory().map(Path::to_path_buf) else {
            return;
        };
        let Some(snapshot_path) = file.path() else {
            return;
        };
        let (version, cancellable) = self.fs_queries.borrow_mut().start(&paths);
        file.query_info_async(
            "standard::type,time::modified,time::modified-nsec",
            gio::FileQueryInfoFlags::NONE,
            glib::Priority::DEFAULT,
            Some(&cancellable),
            clone!(
                #[strong(rename_to = app)]
                self,
                move |result| {
                    let snapshot = result
                        .ok()
                        .and_then(|info| file_snapshot_from_info(snapshot_path, &info));
                    let current = app.fs_queries.borrow_mut().finish(&paths, version);
                    let same_directory = app.navigation.borrow().directory() == Some(&directory);
                    if current && same_directory && !app.shutting_down.get() {
                        apply(&app, snapshot);
                    }
                }
            ),
        );
    }

    fn apply_fs_change(self: &Rc<Self>, change: FsChange, event: gio::FileMonitorEvent) {
        let path = match &change {
            FsChange::Insert(snapshot) => snapshot.path(),
            FsChange::Remove(path) | FsChange::Rename { old: path, .. } => path,
        }
        .to_path_buf();
        let presentation = apply_fs_change(&mut self.navigation.borrow_mut(), change);
        let current_changed = !matches!(presentation, FsPresentation::Unchanged);
        crate::applog!(
            "fs event: {event:?} {}{}",
            path.display(),
            if current_changed {
                " (current destination changed)"
            } else {
                ""
            }
        );
        match presentation {
            FsPresentation::Show(destination) => {
                self.show_destination(destination, Arrival::Direct)
            }
            FsPresentation::Empty => self.empty_state("No media left in this folder"),
            FsPresentation::Unchanged => {}
        }
        self.update_pos_label();
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
                        app.fs_queries
                            .borrow_mut()
                            .supersede(std::slice::from_ref(&path));
                        let outcome = app.navigation.borrow_mut().remove(&path);
                        *app.pending_undo.borrow_mut() = Some(path);
                        match outcome {
                            RemovalOutcome::CurrentRemoved(Some(destination)) => {
                                app.show_destination(destination, Arrival::Direct)
                            }
                            RemovalOutcome::CurrentRemoved(None) => {
                                app.empty_state("No media left in this folder")
                            }
                            RemovalOutcome::NotFound | RemovalOutcome::CurrentPreserved => {}
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
                let result: Result<FileSnapshot, String> = match gio::spawn_blocking(move || {
                    fileops::restore(&restore_path).map_err(|error| error.to_string())?;
                    let metadata =
                        std::fs::metadata(&restore_path).map_err(|error| error.to_string())?;
                    if !metadata.is_file() {
                        return Err(format!(
                            "restored path is not a regular file: {}",
                            restore_path.display()
                        ));
                    }
                    let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                    Ok(FileSnapshot::new(restore_path, modified))
                })
                .await
                {
                    Ok(result) => result,
                    Err(_) => Err(format!(
                        "could not restore {}: restore worker failed",
                        path.display()
                    )),
                };
                match result {
                    Ok(snapshot) => {
                        crate::applog!("restore: {}", path.display());
                        let idx = {
                            let mut navigation = app.navigation.borrow_mut();
                            // insert() returns None if the monitor already
                            // re-added it (gotcha: dedup).
                            navigation
                                .insert(snapshot)
                                .or_else(|| navigation.index_of(&path))
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
        self.saving.set(true);
        self.update_save_enabled();
        glib::spawn_future_local(clone!(
            #[strong(rename_to = app)]
            self,
            async move {
                let started = std::time::Instant::now();
                let result = fileops::save_rotation(&path, rotation).await;
                app.saving.set(false);
                match result {
                    Ok(()) => {
                        crate::applog!(
                            "save-rotation: {} ({}°) in {:.1} ms",
                            path.display(),
                            u32::from(rotation) * 90,
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
                    }
                }
                app.update_save_enabled();
            }
        ));
    }

    /// Return the domain-derived availability and explanation for Rotate
    /// Save. The GIO action mirrors this result; it is never its source.
    fn save_availability(&self) -> (bool, String) {
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
        drop(media);
        let enabled = !self.saving.get()
            && !self.view.is_marking_up()
            && save_control_visible(reason.is_none(), self.view.rotation());
        let tooltip = match reason {
            Some(why) => why,
            None if self.saving.get() => "A rotation is already being saved".into(),
            None if enabled => "Save rotation to file".into(),
            // Editable, but nothing has been rotated yet.
            None => "Rotate the image first".into(),
        };
        (enabled, tooltip)
    }

    /// Rotate-save is offered only where it is safe and meaningful:
    /// still raster images in formats the sandboxed editor can rewrite;
    /// SVG and animations stay view-only (FR-5.4).
    fn update_save_enabled(&self) {
        let (enabled, tooltip) = self.save_availability();
        // `set_enabled` can emit into application code; never hold a
        // RefCell borrow across that framework boundary.
        self.save_action.set_enabled(enabled);
        self.save_btn.set_visible(enabled);
        self.save_btn.set_tooltip_text(Some(&tooltip));
        self.sync_action_enabled();
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
        if self.view.is_marking_up()
            || chrome_is_held(
                self.scrubbing.get(),
                self.pointer_on_chrome.get() || self.pointer_on_status.get(),
                self.menu_open.get(),
            )
        {
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

    fn update_playback_rate(&self, rate: f64) {
        let label = format_playback_rate(rate);
        if self.speed_label.text() != label {
            self.speed_label.set_text(&label);
            self.speed_action.set_state(&rate.to_variant());
            self.fitted_for.set(None);
        }
    }

    fn set_playback_rate(self: &Rc<Self>, rate: f64) {
        match self.with_video(|player| player.set_playback_rate(rate)) {
            Some(Ok(rate)) => {
                self.update_playback_rate(rate);
                self.flash(&format_playback_rate(rate));
            }
            Some(Err(error)) => self.show_toast(&error.to_string(), false),
            None => {}
        }
    }

    fn step_playback_rate(self: &Rc<Self>, direction: i32) {
        let Some(current) = self.with_video(Player::playback_rate) else {
            return;
        };
        self.set_playback_rate(adjacent_playback_rate(current, direction));
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
        self.update_undo_enabled();
        self.toast_revealer.set_reveal_child(true);
        reset_timer(
            &self.toast_timer,
            TOAST_TIMEOUT,
            clone!(
                #[strong(rename_to = app)]
                self,
                move || {
                    // The undo window lapses; the file stays in trash.
                    *app.pending_undo.borrow_mut() = None;
                    app.hide_toast();
                }
            ),
        );
    }

    fn hide_toast(&self) {
        self.toast_revealer.set_reveal_child(false);
        self.update_undo_enabled();
    }

    fn update_undo_enabled(&self) {
        let markup_undo = self
            .view
            .markup_status()
            .is_some_and(|status| status.can_undo);
        let trash_undo = !self.view.is_marking_up() && self.pending_undo.borrow().is_some();
        self.undo_action.set_enabled(markup_undo || trash_undo);
        self.sync_action_enabled();
    }

    // ----- actions and input -------------------------------------------

    fn workspace_state(&self) -> WorkspaceState {
        let media = match &*self.media.borrow() {
            MediaState::Empty => Media::Empty,
            MediaState::Loading(_) => Media::Loading,
            MediaState::Image { decoded, .. } => Media::Image {
                markup_available: matches!(&**decoded, Decoded::Static { .. }),
            },
            MediaState::Video(_) => Media::Video,
            MediaState::Error(_) => Media::Error,
        };
        let markup = self.view.markup_status();
        WorkspaceState {
            media,
            has_navigation: !self.navigation.borrow().is_empty(),
            pannable: self.view.is_pannable(),
            marking: self.view.is_marking_up(),
            markup_draft: self.view.markup_has_draft(),
            markup_shapes: markup.is_some_and(|status| status.shape_count > 0),
            markup_can_copy: markup.is_some_and(|status| status.shape_count > 0)
                && !self.view.markup_has_draft(),
            markup_can_undo: markup.is_some_and(|status| status.can_undo),
            can_save: self.save_availability().0,
            can_undo_trash: self.pending_undo.borrow().is_some(),
            help_visible: self.help_label.is_visible(),
            fullscreen: self.win.is_fullscreen(),
        }
    }

    fn sync_action_enabled(&self) {
        let state = self.workspace_state();
        for action in Action::all() {
            let Some(registered) = self.win.lookup_action(action.name()) else {
                continue;
            };
            let Ok(simple) = registered.downcast::<gio::SimpleAction>() else {
                continue;
            };
            simple.set_enabled(action.enabled(state));
        }
    }

    fn dispatch_action(self: &Rc<Self>, action: Action) {
        let Some(command) = action.resolve(self.workspace_state()) else {
            return;
        };
        self.execute_command(command);
    }

    fn dispatch_subtitle(self: &Rc<Self>, choice: SubtitleChoice) {
        if let Some(command) =
            Action::SubtitleSelect.resolve_subtitle(self.workspace_state(), choice)
        {
            self.execute_command(command);
        }
    }

    fn dispatch_audio(self: &Rc<Self>, choice: AudioChoice) {
        if let Some(command) = Action::AudioSelect.resolve_audio(self.workspace_state(), choice) {
            self.execute_command(command);
        }
    }

    fn dispatch_speed(self: &Rc<Self>, rate: f64) {
        if let Some(command) = Action::SpeedSet.resolve_speed(self.workspace_state(), rate) {
            self.execute_command(command);
        }
    }

    fn execute_command(self: &Rc<Self>, command: Command) {
        match command {
            Command::OpenFile => self.choose_media_file(),
            Command::OpenFolder => self.choose_folder(),
            Command::Pan(dx, dy) => self
                .view
                .pan_by(-f64::from(dx) * PAN_STEP, -f64::from(dy) * PAN_STEP),
            Command::Next => self.navigate(Direction::Next),
            Command::Previous => self.navigate(Direction::Previous),
            Command::First => self.show_index(0, Arrival::Direct),
            Command::Last => self.show_index(self.navigation.borrow().len() - 1, Arrival::Direct),
            Command::TogglePlayback => match self.with_video(Player::toggle_pause) {
                Some(true) => {
                    self.set_idle_inhibited(true);
                    self.flash("Play");
                }
                Some(false) => {
                    self.set_idle_inhibited(false);
                    self.flash("Paused");
                }
                None => {}
            },
            Command::Seek(direction) => {
                self.with_video(|player| player.seek_by(f64::from(direction) * SEEK_STEP_SECONDS));
                self.flash_progress();
            }
            Command::StepSpeed(direction) => self.step_playback_rate(direction.into()),
            Command::ResetSpeed => self.set_playback_rate(1.0),
            Command::ToggleMute => match self.with_video(Player::toggle_mute) {
                Some(true) => self.flash("Muted"),
                Some(false) => self.flash("Sound on"),
                None => {}
            },
            Command::OpenSubtitle => self.choose_external_subtitle(),
            Command::ToggleSubtitles => {
                let snapshot = self.with_video(Player::toggle_subtitles);
                if let Some(snapshot) = snapshot
                    && !snapshot.tracks.is_empty()
                {
                    self.update_subtitles(snapshot.clone());
                    self.flash_subtitle_choice(&snapshot);
                }
            }
            Command::CycleSubtitles => {
                let snapshot = self.with_video(Player::cycle_subtitles);
                if let Some(snapshot) = snapshot
                    && !snapshot.tracks.is_empty()
                {
                    self.update_subtitles(snapshot.clone());
                    self.flash_subtitle_choice(&snapshot);
                }
            }
            Command::SelectSubtitle(choice) => {
                let Some(changed) = self.with_video(|player| player.choose_subtitle(choice)) else {
                    return;
                };
                if changed && let Some(snapshot) = self.with_video(Player::subtitle_snapshot) {
                    self.update_subtitles(snapshot.clone());
                    self.flash_subtitle_choice(&snapshot);
                }
            }
            Command::SelectAudio(choice) => {
                let Some(changed) = self.with_video(|player| player.choose_audio(choice)) else {
                    return;
                };
                if changed && let Some(snapshot) = self.with_video(Player::audio_snapshot) {
                    self.update_audio(snapshot.clone());
                    self.flash_audio_choice(&snapshot);
                }
            }
            Command::SetSpeed(rate) => self.set_playback_rate(rate),
            Command::ChangeVolume(direction) => self.change_volume(f64::from(direction) * 0.1),
            Command::ZoomIn => self.view.zoom_by(1.25, None),
            Command::ZoomOut => self.view.zoom_by(0.8, None),
            Command::ZoomFit => self.view.zoom_fit(),
            Command::ZoomActual => self.view.zoom_to(1.0, None),
            Command::ZoomToggle => self.view.toggle_fit_actual(),
            Command::Rotate(turns) => self.view.rotate_view(turns),
            Command::ToggleMarkup => self.toggle_markup(),
            Command::MarkupBox => self.view.set_markup_tool(MarkupTool::Box),
            Command::MarkupArrow => self.view.set_markup_tool(MarkupTool::Arrow),
            Command::CopyMarkup => self.copy_markup(),
            Command::ClearMarkup => {
                self.view.clear_markup();
            }
            Command::Save => self.save_rotation(),
            Command::Trash => self.trash_current(),
            Command::MarkupUndo => {
                self.view.undo_markup();
                self.update_undo_enabled();
            }
            Command::TrashUndo => self.undo_trash(),
            Command::ToggleFullscreen => {
                if self.win.is_fullscreen() {
                    self.win.unfullscreen();
                } else {
                    self.win.fullscreen();
                }
            }
            Command::ToggleHelp => self.help_label.set_visible(!self.help_label.is_visible()),
            Command::CancelMarkupDraft => {
                self.view.cancel_markup_draft();
                self.update_undo_enabled();
            }
            Command::CancelMarkup => {
                self.view.cancel_markup();
                self.update_control_mode();
                self.show_chrome();
                self.update_cursor();
            }
            Command::HideHelp => self.help_label.set_visible(false),
            Command::LeaveFullscreen => self.win.unfullscreen(),
            Command::Close => self.win.close(),
        }
    }

    fn setup_actions(self: &Rc<Self>, gtk_app: &gtk::Application) {
        const MANAGED_ACTIONS: &[Action] = &[
            Action::Markup,
            Action::MarkupBox,
            Action::MarkupArrow,
            Action::MarkupCopy,
            Action::MarkupClear,
            Action::Save,
            Action::Undo,
        ];
        for (name, _) in Action::CONFIGURABLE {
            if MANAGED_ACTIONS.contains(name) {
                continue;
            }
            let action = gio::SimpleAction::new(name.name(), None);
            let app = self.clone();
            let name = *name;
            action.connect_activate(move |_, _| app.dispatch_action(name));
            self.win.add_action(&action);
        }

        let app = self.clone();
        self.markup_action
            .connect_activate(move |_, _| app.dispatch_action(Action::Markup));
        self.win.add_action(&self.markup_action);

        let app = self.clone();
        self.markup_box_action.connect_activate(move |_, _| {
            app.dispatch_action(Action::MarkupBox);
        });
        self.win.add_action(&self.markup_box_action);

        let app = self.clone();
        self.markup_arrow_action.connect_activate(move |_, _| {
            app.dispatch_action(Action::MarkupArrow);
        });
        self.win.add_action(&self.markup_arrow_action);

        let app = self.clone();
        self.markup_copy_action
            .connect_activate(move |_, _| app.dispatch_action(Action::MarkupCopy));
        self.win.add_action(&self.markup_copy_action);

        let app = self.clone();
        self.markup_clear_action.connect_activate(move |_, _| {
            app.dispatch_action(Action::MarkupClear);
        });
        self.win.add_action(&self.markup_clear_action);

        let app = self.clone();
        self.save_action.set_enabled(false);
        self.save_action
            .connect_activate(move |_, _| app.dispatch_action(Action::Save));
        self.win.add_action(&self.save_action);

        let app = self.clone();
        self.undo_action.set_enabled(false);
        self.undo_action.connect_activate(move |_, _| {
            app.dispatch_action(Action::Undo);
        });
        self.win.add_action(&self.undo_action);

        let app = self.clone();
        self.subtitle_action.connect_activate(move |_, parameter| {
            let Some(target) = parameter.and_then(glib::Variant::str) else {
                return;
            };
            let choice = match target {
                "auto" => SubtitleChoice::Automatic,
                "off" => SubtitleChoice::Off,
                target => {
                    let Some(id) = target.strip_prefix("track:") else {
                        return;
                    };
                    SubtitleChoice::Track(id.to_string())
                }
            };
            app.dispatch_subtitle(choice);
        });
        self.win.add_action(&self.subtitle_action);

        let app = self.clone();
        self.audio_action.connect_activate(move |_, parameter| {
            let Some(target) = parameter.and_then(glib::Variant::str) else {
                return;
            };
            let choice = match target {
                "auto" => AudioChoice::Automatic,
                target => {
                    let Some(id) = target.strip_prefix("track:") else {
                        return;
                    };
                    AudioChoice::Track(id.to_string())
                }
            };
            app.dispatch_audio(choice);
        });
        self.win.add_action(&self.audio_action);

        let app = self.clone();
        self.speed_action.connect_activate(move |_, parameter| {
            let Some(rate) = parameter.and_then(glib::Variant::get::<f64>) else {
                return;
            };
            app.dispatch_speed(rate);
        });
        self.win.add_action(&self.speed_action);

        // Defaults merged with user binds (FR-8.2); a user bind takes
        // the key over from the default action.
        let mut key_to_action: BTreeMap<String, Action> = Action::DEFAULT_BINDS
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
            gtk_app.set_accels_for_action(&action.detailed_name(), keys);
        }
        self.update_control_mode();
    }

    fn build_help(&self, gtk_app: &gtk::Application) {
        let mut rows: Vec<(String, &str)> = Vec::new();
        for (action, description) in Action::CONFIGURABLE {
            if description.is_empty() {
                continue;
            }
            let accels = gtk_app.accels_for_action(&action.detailed_name());
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
        lines.push(
            "<tt>Escape</tt> cancels Quick Markup, leaves fullscreen, then closes".to_string(),
        );
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
                    i32::try_from(gdk::BUTTON_PRIMARY).unwrap_or(1),
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
                move |_, _, _| {
                    app.pointer_on_chrome.set(true);
                    app.update_cursor();
                }
            ));
            hover.connect_leave(clone!(
                #[strong(rename_to = app)]
                self,
                move |_| {
                    app.pointer_on_chrome.set(false);
                    app.show_chrome();
                    app.update_cursor();
                }
            ));
            w.add_controller(hover);
        }
        // The status actions do not fade with media chrome, but resting the
        // pointer on one must still keep the cursor visible until it leaves.
        let status_hover = gtk::EventControllerMotion::new();
        status_hover.connect_enter(clone!(
            #[strong(rename_to = app)]
            self,
            move |_, _, _| app.pointer_on_status.set(true)
        ));
        status_hover.connect_leave(clone!(
            #[strong(rename_to = app)]
            self,
            move |_| {
                app.pointer_on_status.set(false);
                app.show_chrome();
            }
        ));
        self.status_area.add_controller(status_hover);

        // Double-click: fullscreen. Middle-click: fit/100% toggle (FR-4.3).
        let click = gtk::GestureClick::new();
        click.set_button(gdk::BUTTON_PRIMARY);
        click.connect_pressed(clone!(
            #[strong(rename_to = app)]
            self,
            move |_, n_press, _, _| {
                if n_press == 2 {
                    WidgetExt::activate_action(&app.win, &Action::Fullscreen.detailed_name(), None)
                        .ok();
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
                if app.view.is_marking_up() {
                    return;
                }
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
                    i32::try_from(gdk::BUTTON_PRIMARY).unwrap_or(1),
                    sx,
                    sy,
                    gesture.current_event_time(),
                );
            }
        ));
        self.win.add_controller(drag);

        // Nautilus sends `GdkFileList`, even for one file. Accept that
        // native payload for real Files drags; keep GioFile as a fallback
        // for sources that advertise only a single file (FR-1.5/10.7).
        let file_list_drop =
            gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
        file_list_drop.connect_drop(clone!(
            #[strong(rename_to = app)]
            self,
            move |_, value, _, _| {
                if let Ok(files) = value.get::<gdk::FileList>() {
                    return app.handle_dropped_files(files.files());
                }
                false
            }
        ));
        self.win.add_controller(file_list_drop);

        let single_file_drop =
            gtk::DropTarget::new(gio::File::static_type(), gdk::DragAction::COPY);
        single_file_drop.connect_drop(clone!(
            #[strong(rename_to = app)]
            self,
            move |_, value, _, _| value
                .get::<gio::File>()
                .is_ok_and(|file| app.handle_dropped_files(vec![file]))
        ));
        self.win.add_controller(single_file_drop);
    }
}

// ----- helpers ----------------------------------------------------------

fn cache_budget_bytes(mebibytes: u32) -> usize {
    usize::try_from(mebibytes)
        .unwrap_or(usize::MAX)
        .saturating_mul(1024 * 1024)
}

fn svg_render_dimension(value: f64) -> u32 {
    if !value.is_finite() {
        return 1;
    }
    // The value is finite, positive and within u32 before crossing the
    // glycin FFI boundary.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let dimension = value.round().clamp(1.0, f64::from(loader::SVG_RENDER_MAX)) as u32;
    dimension
}

fn window_dimension(value: f64, minimum: i32) -> i32 {
    if !value.is_finite() {
        return minimum;
    }
    // GTK accepts i32 logical dimensions. Validate the floating-point
    // calculation before the final FFI conversion.
    #[allow(clippy::cast_possible_truncation)]
    let dimension = value.round().clamp(f64::from(minimum), f64::from(i32::MAX)) as i32;
    dimension
}

fn osd_button(icon: &str, action: &str, tooltip: &str) -> gtk::Button {
    let b = gtk::Button::from_icon_name(icon);
    b.set_action_name(Some(action));
    b.set_tooltip_text(Some(tooltip));
    b.add_css_class("flat");
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

fn format_playback_rate(rate: f64) -> String {
    let value = format!("{rate:.2}")
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_string();
    format!("{value}×")
}

fn adjacent_playback_rate(current: f64, direction: i32) -> f64 {
    let index = PLAYBACK_RATES
        .iter()
        .position(|rate| (*rate - current).abs() < f64::EPSILON)
        .unwrap_or_else(|| {
            PLAYBACK_RATES
                .iter()
                .enumerate()
                .min_by(|(_, left), (_, right)| {
                    (**left - current)
                        .abs()
                        .total_cmp(&(**right - current).abs())
                })
                .map_or(2, |(index, _)| index)
        });
    let next = if direction < 0 {
        index.saturating_sub(1)
    } else {
        (index + 1).min(PLAYBACK_RATES.len() - 1)
    };
    PLAYBACK_RATES[next]
}

fn playback_speed_menu() -> gio::Menu {
    let menu = gio::Menu::new();
    for rate in PLAYBACK_RATES {
        let item = gio::MenuItem::new(Some(&format_playback_rate(*rate)), None);
        item.set_action_and_target_value(
            Some(&Action::SpeedSet.detailed_name()),
            Some(&rate.to_variant()),
        );
        menu.append_item(&item);
    }
    menu
}

fn append_choice_item(menu: &gio::Menu, action: &str, label: &str, target: &str) {
    let item = gio::MenuItem::new(Some(label), None);
    item.set_action_and_target_value(Some(action), Some(&target.to_variant()));
    menu.append_item(&item);
}

fn rebuild_audio_menu(menu: &gio::Menu, snapshot: &AudioSnapshot) {
    menu.remove_all();
    append_choice_item(
        menu,
        &Action::AudioSelect.detailed_name(),
        "Automatic",
        "auto",
    );
    for track in &snapshot.tracks {
        let target = AudioChoice::Track(track.id.clone()).action_target();
        append_choice_item(
            menu,
            &Action::AudioSelect.detailed_name(),
            &track.label,
            &target,
        );
    }
}

fn rebuild_audio_context(context: &gio::Menu, audio: &gio::Menu, video: bool, track_count: usize) {
    context.remove_all();
    if video && track_count > 1 {
        context.append_submenu(Some("Audio Track"), audio);
    }
}

fn append_subtitle_item(menu: &gio::Menu, label: &str, target: &str) {
    append_choice_item(menu, &Action::SubtitleSelect.detailed_name(), label, target);
}

fn append_subtitle_track(menu: &gio::Menu, track: &SubtitleTrack) {
    let target = SubtitleChoice::Track(track.id.clone()).action_target();
    append_subtitle_item(menu, &track.label, &target);
}

fn rebuild_subtitle_menu(menu: &gio::Menu, snapshot: &SubtitleSnapshot) {
    menu.remove_all();
    menu.append(
        Some("Add External Subtitle…"),
        Some(&Action::SubtitleOpen.detailed_name()),
    );
    if snapshot.tracks.is_empty() {
        return;
    }
    let choices = gio::Menu::new();
    append_subtitle_item(&choices, "Automatic", "auto");
    append_subtitle_item(&choices, "Off", "off");
    for track in &snapshot.tracks {
        append_subtitle_track(&choices, track);
    }
    menu.append_section(None, &choices);
}

fn rebuild_subtitle_context(context: &gio::Menu, subtitles: &gio::Menu, video: bool) {
    context.remove_all();
    if video {
        context.append_submenu(Some("Subtitles"), subtitles);
    }
}

fn rebuild_markup_context(context: &gio::Menu, available: bool) {
    context.remove_all();
    if available {
        context.append(Some("Quick Markup"), Some(&Action::Markup.detailed_name()));
    }
}

/// Recognise common subtitle extensions that are deliberately outside the
/// supported SRT/WebVTT boundary, so dropping one reports non-modally rather
/// than replacing the current video with an unsupported-file error.
fn looks_like_subtitle(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            ["ass", "ssa", "sub", "smi", "sami", "ttml"]
                .iter()
                .any(|known| known.eq_ignore_ascii_case(extension))
        })
}

fn apply_css(background: &str) {
    let css = format!(
        r#"
        window {{ background-color: {background}; }}
        .chrome {{ transition: opacity 200ms ease; opacity: 1; }}
        .chrome.invisible {{ opacity: 0; }}
        .osd-surface, .osd-btn {{ background-color: rgba(0, 0, 0, 0.62);
            color: #eeeeee; border-radius: 9px; }}
        .info-bar {{ padding: 7px 11px; margin: 12px; }}
        .info-bar .position {{ opacity: 0.68; }}
        .window-controls {{ padding: 2px; margin: 12px; }}
        .window-controls button, .osd-bar button, .osd-bar menubutton > button {{
            min-width: 36px; min-height: 36px; padding: 0; border-radius: 7px; }}
        .osd-bar menubutton > button {{ background-color: transparent;
            background-image: none; border: none; box-shadow: none; }}
        .window-controls button:hover, .osd-bar button:hover,
        .osd-bar menubutton > button:hover {{
            background-color: rgba(255, 255, 255, 0.14); }}
        .indicator {{ transition: opacity 200ms ease; opacity: 1;
            background-color: rgba(0, 0, 0, 0.55); color: #eeeeee;
            padding: 4px 12px; margin: 12px; border-radius: 6px; }}
        .indicator.invisible {{ opacity: 0; }}
        .osd-btn {{ background-image: none; border: none; box-shadow: none; }}
        .osd-bar {{ padding: 4px 6px; margin: 12px; }}
        .markup-controls button:checked {{ background-color: rgba(255, 255, 255, 0.22); }}
        .osd-bar separator {{ min-width: 1px; margin: 5px 2px;
            background-color: rgba(255, 255, 255, 0.2); }}
        .toast {{ background-color: rgba(25, 25, 25, 0.92); color: #eeeeee;
            padding: 8px 14px; border-radius: 8px; }}
        .status {{ color: #999999; font-size: 1.1em; }}
        .status-actions button {{ background-color: rgba(25, 25, 25, 0.92);
            color: #eeeeee; border-radius: 7px; }}
        .status-actions button:hover {{ background-color: rgba(55, 55, 55, 0.96); }}
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

/// One authoritative chooser filter for every extension that the folder
/// model accepts. `add_suffix` matches case-insensitively, like config's
/// extension checks, and avoids a runtime loader query on startup.
fn supported_media_filter() -> gtk::FileFilter {
    let filter = gtk::FileFilter::new();
    filter.set_name(Some("Supported Media"));
    for extension in supported_media_extensions() {
        filter.add_suffix(extension);
    }
    filter
}

fn supported_media_extensions() -> impl Iterator<Item = &'static str> {
    config::IMAGE_EXTENSIONS
        .iter()
        .chain(config::VIDEO_EXTENSIONS)
        .copied()
}

fn dialog_was_cancelled(error: &glib::Error) -> bool {
    error.matches(gtk::DialogError::Cancelled)
        || error.matches(gtk::DialogError::Dismissed)
        || error.matches(gio::IOErrorEnum::Cancelled)
}

fn dialog_initial_folder_path(current: Option<&Path>) -> Option<PathBuf> {
    let path = current?;
    if path.is_dir() {
        Some(path.to_path_buf())
    } else {
        path.parent().map(Path::to_path_buf)
    }
}

fn open_menu_model() -> gio::Menu {
    let menu = gio::Menu::new();
    menu.append(Some("Open File…"), Some(&Action::OpenFile.detailed_name()));
    menu.append(
        Some("Open Folder…"),
        Some(&Action::OpenFolder.detailed_name()),
    );
    menu
}

fn file_snapshot_from_info(path: PathBuf, info: &gio::FileInfo) -> Option<FileSnapshot> {
    if info.file_type() != gio::FileType::Regular {
        return None;
    }
    let timestamp = Duration::from_secs(info.attribute_uint64("time::modified")).saturating_add(
        Duration::from_nanos(u64::from(info.attribute_uint32("time::modified-nsec"))),
    );
    let modified = SystemTime::UNIX_EPOCH
        .checked_add(timestamp)
        .unwrap_or(SystemTime::UNIX_EPOCH);
    Some(FileSnapshot::new(path, modified))
}

/// Apply one already-prepared filesystem mutation to the plain-Rust owner and
/// return only the presentation work the window adapter must perform.
fn apply_fs_change(navigation: &mut Navigation, change: FsChange) -> FsPresentation {
    match change {
        FsChange::Insert(snapshot) => {
            navigation.insert(snapshot);
            FsPresentation::Unchanged
        }
        FsChange::Remove(path) => match navigation.remove(&path) {
            RemovalOutcome::CurrentRemoved(Some(destination)) => FsPresentation::Show(destination),
            RemovalOutcome::CurrentRemoved(None) => FsPresentation::Empty,
            RemovalOutcome::NotFound | RemovalOutcome::CurrentPreserved => {
                FsPresentation::Unchanged
            }
        },
        FsChange::Rename { old, new, snapshot } => match navigation.rename(&old, &new, snapshot) {
            RenameOutcome::Renamed(destination) | RenameOutcome::Removed(Some(destination)) => {
                FsPresentation::Show(destination)
            }
            RenameOutcome::Removed(None) => FsPresentation::Empty,
            RenameOutcome::Preserved => FsPresentation::Unchanged,
        },
    }
}

/// Where a decode failure at `idx` sends us: onward in the direction of
/// travel, or `None` to stop and show the error (FR-2.5).
fn skip_target(navigation: &Navigation, idx: usize, arrival: Arrival, wrap: bool) -> Option<usize> {
    let Arrival::Step { direction, budget } = arrival else {
        return None;
    };
    if budget == 0 {
        return None;
    }
    match direction {
        Direction::Next => navigation.next_from(idx, wrap),
        Direction::Previous => navigation.prev_from(idx, wrap),
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
/// grab, then Quick Markup uses a crosshair over the image, and otherwise
/// the pointer goes away with the controls (mpv-style).
fn cursor_name(
    edge: Option<gdk::SurfaceEdge>,
    marking_up: bool,
    chrome_visible: bool,
    hide_cursor: bool,
) -> Option<&'static str> {
    match edge {
        Some(edge) => Some(edge_cursor_name(edge)),
        None if marking_up => Some("crosshair"),
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

fn position_text(index: usize, total: usize) -> String {
    format!("{} of {total}", index + 1)
}

const fn chrome_is_held(scrubbing: bool, pointer_on_chrome: bool, menu_open: bool) -> bool {
    scrubbing || pointer_on_chrome || menu_open
}

const fn save_control_visible(can_save: bool, rotation: u8) -> bool {
    can_save && rotation != 0
}

/// `M:SS`, or `H:MM:SS` from the first hour (FR-10.5).
fn format_time(secs: f64) -> String {
    let s = Duration::try_from_secs_f64(secs.max(0.0).round()).map_or(0, |value| value.as_secs());
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

impl TimerSlot {
    fn cancel(&self) {
        if let Some(id) = self.0.borrow_mut().take() {
            id.remove();
        }
    }
}

fn reset_timer(slot: &TimerSlot, after: Duration, f: impl FnOnce() + 'static) {
    slot.cancel();
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
        Action, Arrival, Direction, FsChange, FsPresentation, FsQueryVersions, SEEK_STEP_SECONDS,
        SKIP_BUDGET, adjacent_playback_rate, apply_fs_change, cache_budget_bytes, chrome_is_held,
        dialog_initial_folder_path, dialog_was_cancelled, excluded_path_message,
        file_snapshot_from_info, format_playback_rate, format_time, looks_like_subtitle,
        open_menu_model, playback_speed_menu, position_text, rebuild_audio_context,
        rebuild_audio_menu, rebuild_markup_context, rebuild_subtitle_context,
        rebuild_subtitle_menu, resize_edge_at, save_control_visible, skip_target,
        supported_media_extensions, svg_render_dimension, window_dimension,
    };

    use crate::config::{Sort, SortOrder};
    use crate::folder::{FileSnapshot, Folder, Navigation};
    use crate::loader;
    use crate::player::{
        AudioChoice, AudioSnapshot, AudioTrack, SubtitleChoice, SubtitleSnapshot, SubtitleTrack,
    };
    use gtk4::gdk::SurfaceEdge;
    use gtk4::gio::prelude::CancellableExt;
    use gtk4::glib::value::ToValue;
    use gtk4::prelude::{FileExt, MenuModelExt};
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

    fn navigation_of(name: &str, files: &[&str]) -> (std::path::PathBuf, Navigation) {
        let (dir, folder) = folder_of(name, files);
        let mut navigation = Navigation::default();
        navigation.install(folder);
        (dir, navigation)
    }

    #[test]
    fn arrows_work_when_nothing_is_on_screen() {
        // Opening an unsupported file loads the folder but never lands on
        // an image, leaving the media state empty. The arrows used to go dead:
        // navigate() bailed out on the missing current index.
        let (dir, mut navigation) = navigation_of("nav", &["a.jpg", "b.jpg", "c.jpg"]);
        assert_eq!(
            navigation.next(false),
            Some(0),
            "right enters at the first image"
        );
        assert_eq!(navigation.prev(false), Some(2), "left enters at the last");
        // Normal stepping is unchanged.
        navigation.select(0).unwrap();
        assert_eq!(navigation.next(false), Some(1));
        navigation.select(2).unwrap();
        assert_eq!(navigation.next(false), None);
        assert_eq!(navigation.next(true), Some(0));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn external_removal_events_present_the_model_outcome() {
        for (name, selected, removed, expected_index, expected_name) in [
            ("delete-middle", 1, "b.jpg", 1, "c.jpg"),
            ("move-out-last", 2, "c.jpg", 1, "b.jpg"),
        ] {
            let (dir, mut navigation) = navigation_of(name, &["a.jpg", "b.jpg", "c.jpg"]);
            navigation.select(selected).unwrap();
            let presentation =
                apply_fs_change(&mut navigation, FsChange::Remove(dir.join(removed)));
            let FsPresentation::Show(destination) = presentation else {
                panic!("expected a replacement destination, got {presentation:?}");
            };
            assert_eq!(destination.index, expected_index);
            assert_eq!(destination.path.file_name().unwrap(), expected_name);
            std::fs::remove_dir_all(&dir).unwrap();
        }
    }

    #[test]
    fn external_rename_to_unsupported_presents_the_nearest_item() {
        let (dir, mut navigation) =
            navigation_of("rename-unsupported-event", &["a.jpg", "b.jpg", "c.jpg"]);
        navigation.select(1).unwrap();
        let old = dir.join("b.jpg");
        let new = dir.join("b.txt");
        std::fs::rename(&old, &new).unwrap();

        let presentation = apply_fs_change(
            &mut navigation,
            FsChange::Rename {
                old,
                new,
                snapshot: None,
            },
        );
        let FsPresentation::Show(destination) = presentation else {
            panic!("expected a replacement destination, got {presentation:?}");
        };
        assert_eq!(destination.index, 1);
        assert_eq!(destination.path, dir.join("c.jpg"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn external_removal_of_the_only_item_presents_empty_state() {
        let (dir, mut navigation) = navigation_of("delete-only", &["only.jpg"]);
        navigation.select(0).unwrap();
        assert_eq!(
            apply_fs_change(&mut navigation, FsChange::Remove(dir.join("only.jpg"))),
            FsPresentation::Empty
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn stale_filesystem_queries_are_rejected_and_released() {
        let path = PathBuf::from("a.jpg");
        let other = PathBuf::from("b.jpg");
        let mut versions = FsQueryVersions::default();

        let (stale, stale_query) = versions.start(std::slice::from_ref(&path));
        let (current, _) = versions.start(std::slice::from_ref(&path));
        let (unrelated, _) = versions.start(std::slice::from_ref(&other));
        assert!(stale_query.is_cancelled());
        assert!(!versions.finish(std::slice::from_ref(&path), stale));
        assert!(versions.finish(std::slice::from_ref(&path), current));
        assert!(versions.finish(std::slice::from_ref(&other), unrelated));
        assert!(versions.paths.is_empty());

        let (stale, stale_query) = versions.start(std::slice::from_ref(&path));
        versions.supersede(std::slice::from_ref(&path));
        assert!(stale_query.is_cancelled());
        assert!(!versions.finish(std::slice::from_ref(&path), stale));
        assert!(versions.paths.is_empty());

        let (stale, stale_query) = versions.start(std::slice::from_ref(&path));
        versions.cancel_all();
        assert!(stale_query.is_cancelled());
        assert!(!versions.finish(std::slice::from_ref(&path), stale));
        assert!(versions.paths.is_empty());
    }

    #[test]
    fn gio_metadata_becomes_a_regular_file_snapshot() {
        let path = PathBuf::from("a.jpg");
        let info = gtk4::gio::FileInfo::new();
        info.set_file_type(gtk4::gio::FileType::Regular);
        info.set_attribute_uint64("time::modified", 42);
        info.set_attribute_uint32("time::modified-nsec", 123);
        assert_eq!(
            file_snapshot_from_info(path.clone(), &info),
            Some(FileSnapshot::new(
                path,
                std::time::SystemTime::UNIX_EPOCH + std::time::Duration::new(42, 123),
            ))
        );

        info.set_file_type(gtk4::gio::FileType::Directory);
        assert_eq!(
            file_snapshot_from_info(PathBuf::from("dir.jpg"), &info),
            None
        );
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
    fn unsupported_subtitle_drops_are_kept_non_modal() {
        assert!(looks_like_subtitle(&PathBuf::from("movie.ASS")));
        assert!(looks_like_subtitle(&PathBuf::from("movie.ssa")));
        assert!(!looks_like_subtitle(&PathBuf::from("movie.mkv")));
    }

    #[test]
    fn files_drag_payload_decodes_as_gdk_file_list() {
        let file = gtk4::gio::File::for_path("/tmp/subtitle.srt");
        let value = gtk4::gdk::FileList::from_array(&[file]).to_value();
        let files = value.get::<gtk4::gdk::FileList>().unwrap().files();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path(), Some(PathBuf::from("/tmp/subtitle.srt")));
    }

    #[test]
    fn open_choosers_start_from_the_current_folder() {
        let (dir, _) = folder_of("chooser-folder", &["photo.jpg"]);
        assert_eq!(
            dialog_initial_folder_path(Some(&dir.join("photo.jpg"))),
            Some(dir.clone())
        );
        assert_eq!(
            dialog_initial_folder_path(Some(&dir)),
            Some(dir.clone()),
            "a folder error should reopen that folder, not its parent"
        );
        assert_eq!(dialog_initial_folder_path(None), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn closing_or_cancelling_a_dialog_is_not_an_error() {
        for error in [
            gtk4::glib::Error::new(gtk4::DialogError::Dismissed, "closed"),
            gtk4::glib::Error::new(gtk4::DialogError::Cancelled, "cancelled"),
            gtk4::glib::Error::new(gtk4::gio::IOErrorEnum::Cancelled, "cancelled"),
        ] {
            assert!(dialog_was_cancelled(&error));
        }
        assert!(!dialog_was_cancelled(&gtk4::glib::Error::new(
            gtk4::DialogError::Failed,
            "failed"
        )));
    }

    #[test]
    fn open_file_filter_receives_every_supported_extension() {
        let actual: Vec<_> = supported_media_extensions().collect();
        let expected: Vec<_> = crate::config::IMAGE_EXTENSIONS
            .iter()
            .chain(crate::config::VIDEO_EXTENSIONS)
            .copied()
            .collect();

        assert_eq!(actual, expected);
        for extension in actual {
            assert!(crate::config::is_supported(&PathBuf::from(format!(
                "sample.{extension}"
            ))));
        }
        assert!(!crate::config::is_supported(&PathBuf::from(
            "photo.jpeg.exe"
        )));
    }

    #[test]
    fn both_open_actions_share_the_more_and_context_menu_model() {
        let menu = open_menu_model();
        assert_eq!(menu.n_items(), 2);
        let action = |index| {
            menu.item_attribute_value(index, "action", Some(gtk4::glib::VariantTy::STRING))
                .and_then(|value| value.str().map(str::to_owned))
        };
        assert_eq!(action(0), Some("win.open-file".to_string()));
        assert_eq!(action(1), Some("win.open-folder".to_string()));
    }

    #[test]
    fn subtitle_menu_always_offers_add_and_right_click_uses_it_for_video() {
        let subtitles = gtk4::gio::Menu::new();
        let empty = SubtitleSnapshot {
            tracks: Vec::new(),
            choice: SubtitleChoice::Automatic,
            active_label: None,
        };
        rebuild_subtitle_menu(&subtitles, &empty);
        assert_eq!(subtitles.n_items(), 1);
        assert_eq!(
            subtitles
                .item_attribute_value(0, "action", Some(gtk4::glib::VariantTy::STRING))
                .and_then(|value| value.str().map(str::to_owned)),
            Some("win.subtitle-open".to_string())
        );

        let with_track = SubtitleSnapshot {
            tracks: vec![SubtitleTrack {
                id: "english".into(),
                label: "English".into(),
            }],
            choice: SubtitleChoice::Automatic,
            active_label: Some("English".into()),
        };
        rebuild_subtitle_menu(&subtitles, &with_track);
        assert_eq!(subtitles.n_items(), 2);
        let choices = subtitles
            .item_link(1, gtk4::gio::MENU_LINK_SECTION.as_str())
            .unwrap();
        assert_eq!(choices.n_items(), 3);

        let context = gtk4::gio::Menu::new();
        rebuild_subtitle_context(&context, &subtitles, false);
        assert_eq!(context.n_items(), 0);
        rebuild_subtitle_context(&context, &subtitles, true);
        assert_eq!(context.n_items(), 1);
        assert!(
            context
                .item_link(0, gtk4::gio::MENU_LINK_SUBMENU.as_str())
                .is_some()
        );
    }

    #[test]
    fn audio_menu_appears_only_for_multiple_tracks() {
        let audio = gtk4::gio::Menu::new();
        let context = gtk4::gio::Menu::new();
        let snapshot = |tracks| AudioSnapshot {
            tracks,
            choice: AudioChoice::Automatic,
            active_label: None,
        };
        let single = snapshot(vec![AudioTrack {
            id: "english".into(),
            label: "English".into(),
        }]);
        rebuild_audio_menu(&audio, &single);
        rebuild_audio_context(&context, &audio, true, single.tracks.len());
        assert_eq!(context.n_items(), 0);

        let multiple = snapshot(vec![
            single.tracks[0].clone(),
            AudioTrack {
                id: "commentary".into(),
                label: "Director Commentary".into(),
            },
        ]);
        rebuild_audio_menu(&audio, &multiple);
        rebuild_audio_context(&context, &audio, true, multiple.tracks.len());
        assert_eq!(context.n_items(), 1);
        let choices = context
            .item_link(0, gtk4::gio::MENU_LINK_SUBMENU.as_str())
            .unwrap();
        assert_eq!(choices.n_items(), 3);
        assert_eq!(
            choices
                .item_attribute_value(2, "action", Some(gtk4::glib::VariantTy::STRING))
                .and_then(|value| value.str().map(str::to_owned)),
            Some("win.audio".to_string())
        );
    }

    #[test]
    fn quick_markup_is_absent_instead_of_disabled_for_unsupported_media() {
        let context = gtk4::gio::Menu::new();
        rebuild_markup_context(&context, false);
        assert_eq!(context.n_items(), 0);
        rebuild_markup_context(&context, true);
        assert_eq!(context.n_items(), 1);
        assert_eq!(
            context
                .item_attribute_value(0, "action", Some(gtk4::glib::VariantTy::STRING))
                .and_then(|value| value.str().map(str::to_owned)),
            Some("win.markup".to_string())
        );
    }

    #[test]
    fn an_empty_folder_has_nowhere_to_enter() {
        let (dir, navigation) = navigation_of("empty", &["notes.txt"]);
        assert_eq!(navigation.next(false), None);
        assert_eq!(navigation.prev(true), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn undecodable_files_are_stepped_over_only_while_navigating() {
        let (dir, navigation) = navigation_of("skip", &["a.jpg", "b.jpg", "c.jpg"]);
        let step = |direction, budget| Arrival::Step { direction, budget };

        // Opened directly: the error is the answer (FR-2.5).
        assert_eq!(skip_target(&navigation, 1, Arrival::Direct, false), None);
        // Stepping: carry on the way the user was already going.
        assert_eq!(
            skip_target(&navigation, 1, step(Direction::Next, SKIP_BUDGET), false),
            Some(2)
        );
        assert_eq!(
            skip_target(
                &navigation,
                1,
                step(Direction::Previous, SKIP_BUDGET),
                false,
            ),
            Some(0)
        );
        // Nowhere further to step.
        assert_eq!(
            skip_target(&navigation, 2, step(Direction::Next, SKIP_BUDGET), false),
            None
        );
        // A folder of unreadable files must stop, not spin — especially
        // with wrap on, where there is always a next index.
        assert_eq!(
            skip_target(&navigation, 1, step(Direction::Next, 1), true),
            Some(2)
        );
        assert_eq!(
            skip_target(&navigation, 1, step(Direction::Next, 0), true),
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
        assert_eq!(cursor_name(None, false, true, true), None);
        // Overlay faded: gone, mpv-style.
        assert_eq!(cursor_name(None, false, false, true), Some("none"));
        // Opted out via config.
        assert_eq!(cursor_name(None, false, false, false), None);
        // Drawing uses a crosshair even if ordinary chrome would hide it.
        assert_eq!(cursor_name(None, true, false, true), Some("crosshair"));
        // An edge always wins — hiding the pointer on the resize border
        // would make a frameless window impossible to grab.
        assert_eq!(
            cursor_name(Some(SurfaceEdge::SouthEast), true, false, true),
            Some("se-resize")
        );
        assert_eq!(
            cursor_name(Some(SurfaceEdge::North), true, true, true),
            Some("n-resize")
        );
    }

    #[test]
    fn every_action_is_documented_and_reachable_by_key() {
        for (key, action) in Action::DEFAULT_BINDS {
            assert!(
                Action::CONFIGURABLE.iter().any(|(name, _)| name == action),
                "default bind `{key}` names `{}`, which is not a known action",
                action.name()
            );
        }
        // Reached through the contextual arrow actions rather than a key
        // of their own, and still bindable by name (FR-8.2). Anything
        // else without a binding is unreachable by keyboard (FR-6.5) and
        // would show up in the cheat sheet with a blank key column.
        const REACHED_CONTEXTUALLY: &[Action] =
            &[Action::VolumeUp, Action::VolumeDown, Action::SubtitleOpen];

        for (action, description) in Action::CONFIGURABLE {
            assert!(
                Action::DEFAULT_BINDS.iter().any(|(_, name)| name == action)
                    || REACHED_CONTEXTUALLY.contains(action),
                "action `{}` has no default binding",
                action.name()
            );
            assert!(
                !description.is_empty() || *action == Action::Escape,
                "action `{}` has no description for the cheat sheet",
                action.name()
            );
        }
    }

    #[test]
    fn open_actions_use_distinct_configurable_shortcuts() {
        assert!(Action::DEFAULT_BINDS.contains(&("<Control>o", Action::OpenFile)));
        assert!(Action::DEFAULT_BINDS.contains(&("<Control><Shift>o", Action::OpenFolder)));
        assert_eq!(Action::parse("open-file"), Some(Action::OpenFile));
        assert_eq!(Action::parse("open-folder"), Some(Action::OpenFolder));
    }

    #[test]
    fn quick_markup_tools_and_copy_use_configurable_actions() {
        for (key, action) in [
            ("a", Action::Markup),
            ("b", Action::MarkupBox),
            ("<Shift>a", Action::MarkupArrow),
            ("<Control>c", Action::MarkupCopy),
            ("c", Action::MarkupClear),
        ] {
            assert!(Action::DEFAULT_BINDS.contains(&(key, action)));
            assert_eq!(Action::parse(action.name()), Some(action));
        }
        assert!(Action::DEFAULT_BINDS.contains(&("<Control>z", Action::Undo)));
    }

    #[test]
    fn video_seek_defaults_are_ten_seconds_with_shift_arrow_aliases() {
        assert_eq!(SEEK_STEP_SECONDS, 10.0);
        assert!(
            Action::DEFAULT_BINDS.contains(&("<Shift>Left", Action::SeekBack)),
            "Shift+Left must seek without taking plain Left away from folder navigation"
        );
        assert!(
            Action::DEFAULT_BINDS.contains(&("<Shift>Right", Action::SeekForward)),
            "Shift+Right must seek without taking plain Right away from folder navigation"
        );
    }

    #[test]
    fn video_speed_presets_are_bounded_rebindable_and_use_typed_targets() {
        assert_eq!(adjacent_playback_rate(1.0, -1), 0.75);
        assert_eq!(adjacent_playback_rate(1.0, 1), 1.25);
        assert_eq!(adjacent_playback_rate(0.5, -1), 0.5);
        assert_eq!(adjacent_playback_rate(2.0, 1), 2.0);
        assert_eq!(format_playback_rate(0.75), "0.75×");
        assert_eq!(format_playback_rate(1.0), "1×");

        assert!(Action::DEFAULT_BINDS.contains(&("bracketleft", Action::SpeedDown)));
        assert!(Action::DEFAULT_BINDS.contains(&("bracketright", Action::SpeedUp)));
        assert!(Action::DEFAULT_BINDS.contains(&("backslash", Action::SpeedReset)));

        let menu = playback_speed_menu();
        assert_eq!(menu.n_items(), crate::player::PLAYBACK_RATES.len() as i32);
        for (index, rate) in crate::player::PLAYBACK_RATES.iter().enumerate() {
            assert_eq!(
                menu.item_attribute_value(
                    index as i32,
                    "target",
                    Some(gtk4::glib::VariantTy::DOUBLE),
                )
                .and_then(|value| value.get::<f64>()),
                Some(*rate)
            );
        }
    }

    #[test]
    fn subtitle_keys_keep_mpv_visibility_without_stealing_seek() {
        assert!(Action::DEFAULT_BINDS.contains(&("v", Action::SubtitleToggle)));
        assert!(Action::DEFAULT_BINDS.contains(&("<Shift>v", Action::SubtitleCycle)));
        assert!(Action::DEFAULT_BINDS.contains(&("j", Action::SeekBack)));
        assert!(Action::DEFAULT_BINDS.contains(&("l", Action::SeekForward)));
    }

    #[test]
    fn overlay_text_and_conditional_save_match_the_media_state() {
        assert_eq!(position_text(54, 67), "55 of 67");
        assert!(!save_control_visible(true, 0));
        assert!(save_control_visible(true, 1));
        assert!(!save_control_visible(false, 1));
        assert!(chrome_is_held(false, false, true));
        assert!(!chrome_is_held(false, false, false));
    }

    #[test]
    fn configured_action_names_parse_only_at_the_boundary() {
        for (action, _) in Action::CONFIGURABLE {
            assert_eq!(Action::parse(action.name()), Some(*action));
        }
        assert_eq!(Action::parse("not-an-action"), None);
    }

    #[test]
    fn configuration_guide_lists_every_action() {
        let guide = include_str!("../docs/CONFIGURATION.md");
        for (action, _) in Action::CONFIGURABLE {
            let name = action.name();
            assert!(
                guide.contains(&format!("`{name}`")),
                "configuration guide does not document `{name}`"
            );
        }
        assert!(guide.contains("`none`"));
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

    #[test]
    fn cache_budget_conversion_is_checked() {
        assert_eq!(cache_budget_bytes(0), 0);
        assert_eq!(cache_budget_bytes(256), 256 * 1024 * 1024);
    }

    #[test]
    fn foreign_dimensions_are_bounded_before_conversion() {
        assert_eq!(svg_render_dimension(f64::NAN), 1);
        assert_eq!(svg_render_dimension(f64::INFINITY), 1);
        assert_eq!(svg_render_dimension(0.4), 1);
        assert_eq!(svg_render_dimension(f64::MAX), loader::SVG_RENDER_MAX);

        assert_eq!(window_dimension(f64::NAN, 200), 200);
        assert_eq!(window_dimension(199.4, 200), 200);
        assert_eq!(window_dimension(f64::MAX, 200), i32::MAX);
    }
}
