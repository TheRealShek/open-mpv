//! Typed Workspace command vocabulary and context policy.
//!
//! GTK actions, accelerators, buttons and menus are adapters. They name an
//! [`Action`]; this module decides whether it is currently available and
//! resolves contextual actions to one concrete [`Command`] without touching
//! widgets. Keeping this layer pure makes Viewer and future Explorer policy
//! testable without constructing a GTK window.

use crate::player::{AudioChoice, SubtitleChoice};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Action {
    OpenFile,
    OpenFolder,
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
    SpeedDown,
    SpeedUp,
    SpeedReset,
    Mute,
    SubtitleOpen,
    SubtitleToggle,
    SubtitleCycle,
    VolumeUp,
    VolumeDown,
    ZoomIn,
    ZoomOut,
    ZoomFit,
    ZoomActual,
    ZoomToggle,
    RotateClockwise,
    RotateCounterclockwise,
    Markup,
    MarkupBox,
    MarkupArrow,
    MarkupCopy,
    MarkupClear,
    Save,
    Trash,
    Undo,
    Fullscreen,
    Help,
    Close,
    Escape,
    SubtitleSelect,
    AudioSelect,
    SpeedSet,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Media {
    Empty,
    Loading,
    Image { markup_available: bool },
    Video,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct WorkspaceState {
    pub media: Media,
    pub has_navigation: bool,
    pub pannable: bool,
    pub marking: bool,
    pub markup_draft: bool,
    pub markup_shapes: bool,
    pub markup_can_copy: bool,
    pub markup_can_undo: bool,
    pub can_save: bool,
    pub can_undo_trash: bool,
    pub help_visible: bool,
    pub fullscreen: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) enum Command {
    OpenFile,
    OpenFolder,
    Pan(i32, i32),
    Next,
    Previous,
    First,
    Last,
    TogglePlayback,
    Seek(i8),
    StepSpeed(i8),
    ResetSpeed,
    ToggleMute,
    OpenSubtitle,
    ToggleSubtitles,
    CycleSubtitles,
    SelectSubtitle(SubtitleChoice),
    SelectAudio(AudioChoice),
    SetSpeed(f64),
    ChangeVolume(i8),
    ZoomIn,
    ZoomOut,
    ZoomFit,
    ZoomActual,
    ZoomToggle,
    Rotate(i8),
    ToggleMarkup,
    MarkupBox,
    MarkupArrow,
    CopyMarkup,
    ClearMarkup,
    Save,
    Trash,
    MarkupUndo,
    TrashUndo,
    ToggleFullscreen,
    ToggleHelp,
    CancelMarkupDraft,
    CancelMarkup,
    HideHelp,
    LeaveFullscreen,
    Close,
}

impl Action {
    pub const CONFIGURABLE: &'static [(Action, &'static str)] = &[
        (Self::OpenFile, "Open a file"),
        (Self::OpenFolder, "Open a folder"),
        (Self::Right, "Next image, or pan when zoomed in"),
        (Self::Left, "Previous image, or pan when zoomed in"),
        (Self::Up, "Volume up, or pan when zoomed in"),
        (Self::Down, "Volume down, or pan when zoomed in"),
        (Self::Next, "Next image"),
        (Self::Previous, "Previous image"),
        (Self::First, "First image"),
        (Self::Last, "Last image"),
        (Self::PlayPause, "Pause video, or next image"),
        (Self::SeekBack, "Seek back 10 seconds"),
        (Self::SeekForward, "Seek forward 10 seconds"),
        (Self::SpeedDown, "Slower video playback"),
        (Self::SpeedUp, "Faster video playback"),
        (Self::SpeedReset, "Reset video speed to 1x"),
        (Self::Mute, "Mute audio"),
        (Self::SubtitleOpen, "Add an external subtitle"),
        (Self::SubtitleToggle, "Show or hide subtitles"),
        (Self::SubtitleCycle, "Next subtitle track"),
        (Self::VolumeUp, "Volume up"),
        (Self::VolumeDown, "Volume down"),
        (Self::ZoomIn, "Zoom in"),
        (Self::ZoomOut, "Zoom out"),
        (Self::ZoomFit, "Fit to window"),
        (Self::ZoomActual, "Actual size, 100%"),
        (Self::ZoomToggle, "Toggle fit and 100%"),
        (Self::RotateClockwise, "Rotate right"),
        (Self::RotateCounterclockwise, "Rotate left"),
        (Self::Markup, "Start or cancel Quick Markup"),
        (Self::MarkupBox, "Quick Markup box tool"),
        (Self::MarkupArrow, "Quick Markup arrow tool"),
        (Self::MarkupCopy, "Copy the annotated image"),
        (Self::MarkupClear, "Clear all annotations"),
        (Self::Save, "Save rotation to the file"),
        (Self::Trash, "Move to trash"),
        (Self::Undo, "Undo delete or last annotation"),
        (Self::Fullscreen, "Fullscreen"),
        (Self::Help, "This list"),
        (Self::Close, "Quit"),
        (Self::Escape, ""),
    ];

    pub const DEFAULT_BINDS: &'static [(&'static str, Action)] = &[
        ("<Control>o", Self::OpenFile),
        ("<Control><Shift>o", Self::OpenFolder),
        ("Right", Self::Right),
        ("Left", Self::Left),
        ("Up", Self::Up),
        ("Down", Self::Down),
        ("space", Self::PlayPause),
        ("Page_Down", Self::Next),
        ("BackSpace", Self::Previous),
        ("Page_Up", Self::Previous),
        ("Home", Self::First),
        ("End", Self::Last),
        ("plus", Self::ZoomIn),
        ("equal", Self::ZoomIn),
        ("KP_Add", Self::ZoomIn),
        ("minus", Self::ZoomOut),
        ("KP_Subtract", Self::ZoomOut),
        ("0", Self::ZoomFit),
        ("1", Self::ZoomActual),
        ("z", Self::ZoomToggle),
        ("r", Self::RotateClockwise),
        ("<Shift>r", Self::RotateCounterclockwise),
        ("a", Self::Markup),
        ("b", Self::MarkupBox),
        ("<Shift>a", Self::MarkupArrow),
        ("<Control>c", Self::MarkupCopy),
        ("c", Self::MarkupClear),
        ("s", Self::Save),
        ("j", Self::SeekBack),
        ("l", Self::SeekForward),
        ("<Shift>Left", Self::SeekBack),
        ("<Shift>Right", Self::SeekForward),
        ("bracketleft", Self::SpeedDown),
        ("bracketright", Self::SpeedUp),
        ("backslash", Self::SpeedReset),
        ("m", Self::Mute),
        ("v", Self::SubtitleToggle),
        ("<Shift>v", Self::SubtitleCycle),
        ("Delete", Self::Trash),
        ("KP_Delete", Self::Trash),
        ("<Control>z", Self::Undo),
        ("f", Self::Fullscreen),
        ("F11", Self::Fullscreen),
        ("q", Self::Close),
        ("question", Self::Help),
        ("Escape", Self::Escape),
    ];

    /// Stateful, menu-only GActions use the same vocabulary and policy but
    /// are not configurable keybinding names or help rows.
    pub const PARAMETERIZED: &'static [Action] =
        &[Self::SubtitleSelect, Self::AudioSelect, Self::SpeedSet];

    pub const fn name(self) -> &'static str {
        match self {
            Self::OpenFile => "open-file",
            Self::OpenFolder => "open-folder",
            Self::Right => "right",
            Self::Left => "left",
            Self::Up => "up",
            Self::Down => "down",
            Self::Next => "next",
            Self::Previous => "prev",
            Self::First => "first",
            Self::Last => "last",
            Self::PlayPause => "play-pause",
            Self::SeekBack => "seek-back",
            Self::SeekForward => "seek-forward",
            Self::SpeedDown => "speed-down",
            Self::SpeedUp => "speed-up",
            Self::SpeedReset => "speed-reset",
            Self::Mute => "mute",
            Self::SubtitleOpen => "subtitle-open",
            Self::SubtitleToggle => "subtitle-toggle",
            Self::SubtitleCycle => "subtitle-cycle",
            Self::VolumeUp => "volume-up",
            Self::VolumeDown => "volume-down",
            Self::ZoomIn => "zoom-in",
            Self::ZoomOut => "zoom-out",
            Self::ZoomFit => "zoom-fit",
            Self::ZoomActual => "zoom-actual",
            Self::ZoomToggle => "zoom-toggle",
            Self::RotateClockwise => "rotate-cw",
            Self::RotateCounterclockwise => "rotate-ccw",
            Self::Markup => "markup",
            Self::MarkupBox => "markup-box",
            Self::MarkupArrow => "markup-arrow",
            Self::MarkupCopy => "markup-copy",
            Self::MarkupClear => "markup-clear",
            Self::Save => "save",
            Self::Trash => "trash",
            Self::Undo => "undo",
            Self::Fullscreen => "fullscreen",
            Self::Help => "help",
            Self::Close => "close",
            Self::Escape => "escape",
            Self::SubtitleSelect => "subtitle",
            Self::AudioSelect => "audio",
            Self::SpeedSet => "speed",
        }
    }

    pub fn detailed_name(self) -> String {
        format!("win.{}", self.name())
    }

    pub fn parse(name: &str) -> Option<Self> {
        Self::CONFIGURABLE
            .iter()
            .find_map(|(action, _)| (action.name() == name).then_some(*action))
    }

    pub fn all() -> impl Iterator<Item = Self> {
        Self::CONFIGURABLE
            .iter()
            .map(|(action, _)| *action)
            .chain(Self::PARAMETERIZED.iter().copied())
    }

    pub fn resolve(self, state: WorkspaceState) -> Option<Command> {
        use Action as A;
        use Command as C;
        let media = state.media;
        let viewing = matches!(media, Media::Image { .. } | Media::Video);
        let video = media == Media::Video;
        let image = matches!(media, Media::Image { .. });
        let normal = !state.marking;
        Some(match self {
            A::OpenFile if normal => C::OpenFile,
            A::OpenFolder if normal => C::OpenFolder,
            A::Right if state.pannable => C::Pan(1, 0),
            A::Left if state.pannable => C::Pan(-1, 0),
            A::Up if state.pannable => C::Pan(0, -1),
            A::Down if state.pannable => C::Pan(0, 1),
            A::Right if normal && state.has_navigation => C::Next,
            A::Left if normal && state.has_navigation => C::Previous,
            A::Up if normal && video => C::ChangeVolume(1),
            A::Down if normal && video => C::ChangeVolume(-1),
            A::Next if normal && state.has_navigation => C::Next,
            A::Previous if normal && state.has_navigation => C::Previous,
            A::First if normal && state.has_navigation => C::First,
            A::Last if normal && state.has_navigation => C::Last,
            A::PlayPause if video => C::TogglePlayback,
            A::PlayPause if normal && state.has_navigation => C::Next,
            A::SeekBack if video && normal => C::Seek(-1),
            A::SeekForward if video && normal => C::Seek(1),
            A::SpeedDown if video && normal => C::StepSpeed(-1),
            A::SpeedUp if video && normal => C::StepSpeed(1),
            A::SpeedReset if video && normal => C::ResetSpeed,
            A::Mute if video && normal => C::ToggleMute,
            A::SubtitleOpen if video && normal => C::OpenSubtitle,
            A::SubtitleToggle if video && normal => C::ToggleSubtitles,
            A::SubtitleCycle if video && normal => C::CycleSubtitles,
            A::VolumeUp if video && normal => C::ChangeVolume(1),
            A::VolumeDown if video && normal => C::ChangeVolume(-1),
            A::ZoomIn if viewing => C::ZoomIn,
            A::ZoomOut if viewing => C::ZoomOut,
            A::ZoomFit if viewing => C::ZoomFit,
            A::ZoomActual if viewing => C::ZoomActual,
            A::ZoomToggle if viewing => C::ZoomToggle,
            A::RotateClockwise if image => C::Rotate(1),
            A::RotateCounterclockwise if image => C::Rotate(-1),
            A::Markup
                if matches!(
                    media,
                    Media::Image {
                        markup_available: true
                    }
                ) || state.marking =>
            {
                C::ToggleMarkup
            }
            A::MarkupBox if state.marking => C::MarkupBox,
            A::MarkupArrow if state.marking => C::MarkupArrow,
            A::MarkupCopy if state.markup_can_copy => C::CopyMarkup,
            A::MarkupClear if state.marking && state.markup_shapes => C::ClearMarkup,
            A::Save if image && state.can_save && normal => C::Save,
            A::Trash if !matches!(media, Media::Empty) && normal => C::Trash,
            A::Undo if state.markup_can_undo => C::MarkupUndo,
            A::Undo if normal && state.can_undo_trash => C::TrashUndo,
            A::Fullscreen => C::ToggleFullscreen,
            A::Help => C::ToggleHelp,
            A::Close => C::Close,
            A::Escape if state.markup_draft => C::CancelMarkupDraft,
            A::Escape if state.marking => C::CancelMarkup,
            A::Escape if state.help_visible => C::HideHelp,
            A::Escape if state.fullscreen => C::LeaveFullscreen,
            A::Escape => C::Close,
            _ => return None,
        })
    }

    pub fn resolve_subtitle(
        self,
        state: WorkspaceState,
        choice: SubtitleChoice,
    ) -> Option<Command> {
        (self == Self::SubtitleSelect && self.parameterized_available(state) == Some(true))
            .then_some(Command::SelectSubtitle(choice))
    }

    pub fn resolve_audio(self, state: WorkspaceState, choice: AudioChoice) -> Option<Command> {
        (self == Self::AudioSelect && self.parameterized_available(state) == Some(true))
            .then_some(Command::SelectAudio(choice))
    }

    pub fn resolve_speed(self, state: WorkspaceState, rate: f64) -> Option<Command> {
        (self == Self::SpeedSet
            && self.parameterized_available(state) == Some(true)
            && crate::player::PLAYBACK_RATES.contains(&rate))
        .then_some(Command::SetSpeed(rate))
    }

    pub fn enabled(self, state: WorkspaceState) -> bool {
        self.parameterized_available(state)
            .unwrap_or_else(|| self.resolve(state).is_some())
    }

    fn parameterized_available(self, state: WorkspaceState) -> Option<bool> {
        matches!(
            self,
            Self::SubtitleSelect | Self::AudioSelect | Self::SpeedSet
        )
        .then_some(state.media == Media::Video && !state.marking)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(media: Media) -> WorkspaceState {
        WorkspaceState {
            media,
            has_navigation: false,
            pannable: false,
            marking: false,
            markup_draft: false,
            markup_shapes: false,
            markup_can_copy: false,
            markup_can_undo: false,
            can_save: false,
            can_undo_trash: false,
            help_visible: false,
            fullscreen: false,
        }
    }

    #[test]
    fn contextual_commands_resolve_from_workspace_state() {
        let mut image = state(Media::Image {
            markup_available: true,
        });
        image.has_navigation = true;
        assert_eq!(Action::Right.resolve(image), Some(Command::Next));
        image.pannable = true;
        assert_eq!(Action::Right.resolve(image), Some(Command::Pan(1, 0)));
        image.marking = true;
        image.pannable = false;
        assert_eq!(Action::Right.resolve(image), None);

        let video = state(Media::Video);
        assert_eq!(
            Action::PlayPause.resolve(video),
            Some(Command::TogglePlayback)
        );
        assert_eq!(Action::Up.resolve(video), Some(Command::ChangeVolume(1)));
    }

    #[test]
    fn escape_and_undo_follow_explicit_context_precedence() {
        let mut current = state(Media::Image {
            markup_available: true,
        });
        current.marking = true;
        current.markup_draft = true;
        current.markup_can_undo = true;
        current.can_undo_trash = true;
        assert_eq!(
            Action::Escape.resolve(current),
            Some(Command::CancelMarkupDraft)
        );
        assert_eq!(Action::Undo.resolve(current), Some(Command::MarkupUndo));
        current.markup_draft = false;
        assert_eq!(Action::Escape.resolve(current), Some(Command::CancelMarkup));
    }

    #[test]
    fn unavailable_commands_are_disabled_by_the_same_policy() {
        let empty = state(Media::Empty);
        assert_eq!(Action::Trash.resolve(empty), None);
        assert_eq!(Action::ZoomIn.resolve(empty), None);
        assert!(Action::OpenFile.resolve(empty).is_some());
        assert!(Action::Escape.resolve(empty).is_some());
    }

    #[test]
    fn parameterized_video_actions_share_context_policy() {
        let video = state(Media::Video);
        assert_eq!(
            Action::SubtitleSelect.resolve_subtitle(video, SubtitleChoice::Off),
            Some(Command::SelectSubtitle(SubtitleChoice::Off))
        );
        assert_eq!(
            Action::AudioSelect.resolve_audio(video, AudioChoice::Automatic),
            Some(Command::SelectAudio(AudioChoice::Automatic))
        );
        assert_eq!(
            Action::SpeedSet.resolve_speed(video, 1.25),
            Some(Command::SetSpeed(1.25))
        );
        assert_eq!(
            Action::AudioSelect.resolve_subtitle(video, SubtitleChoice::Off),
            None
        );
        assert_eq!(Action::SpeedSet.resolve_speed(video, 3.0), None);
        assert!(!Action::SpeedSet.enabled(state(Media::Empty)));
    }

    #[test]
    fn registered_action_names_are_unique_and_config_boundary_stays_compatible() {
        let mut names = std::collections::BTreeSet::new();
        for action in Action::all() {
            assert!(names.insert(action.name()), "duplicate action name");
        }
        for action in Action::PARAMETERIZED {
            assert_eq!(Action::parse(action.name()), None);
        }
    }

    #[test]
    fn rotation_remains_available_during_markup() {
        let mut image = state(Media::Image {
            markup_available: true,
        });
        image.marking = true;
        assert_eq!(
            Action::RotateClockwise.resolve(image),
            Some(Command::Rotate(1))
        );
    }
}
