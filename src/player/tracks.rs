//! Owns stream choices, discovery, labeling, selection, and automatic subtitle sidecars.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::config;
use gstreamer as gst;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioTrack {
    pub id: String,
    pub label: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum AudioChoice {
    #[default]
    Automatic,
    Track(String),
}

impl AudioChoice {
    pub fn action_target(&self) -> String {
        match self {
            AudioChoice::Automatic => "auto".to_string(),
            AudioChoice::Track(id) => format!("track:{id}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioSnapshot {
    pub tracks: Vec<AudioTrack>,
    pub choice: AudioChoice,
    pub active_label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubtitleTrack {
    pub id: String,
    pub label: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum SubtitleChoice {
    #[default]
    Automatic,
    Off,
    Track(String),
}

impl SubtitleChoice {
    pub fn action_target(&self) -> String {
        match self {
            SubtitleChoice::Automatic => "auto".to_string(),
            SubtitleChoice::Off => "off".to_string(),
            SubtitleChoice::Track(id) => format!("track:{id}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubtitleSnapshot {
    pub tracks: Vec<SubtitleTrack>,
    pub choice: SubtitleChoice,
    pub active_label: Option<String>,
}

#[derive(Default)]
pub(super) struct StreamState {
    collection: Option<gst::StreamCollection>,
    selected: BTreeSet<String>,
    audio_tracks: Vec<AudioTrack>,
    audio_choice: AudioChoice,
    subtitle_tracks: Vec<SubtitleTrack>,
    subtitle_choice: SubtitleChoice,
    /// The track visibility toggling should restore after `Off`.
    last_visible_subtitle_choice: SubtitleChoice,
    external: Option<PathBuf>,
}

impl StreamState {
    pub(super) fn new(subtitles_default_on: bool) -> Self {
        Self {
            subtitle_choice: if subtitles_default_on {
                SubtitleChoice::Automatic
            } else {
                SubtitleChoice::Off
            },
            ..Self::default()
        }
    }

    pub(super) fn external(&self) -> Option<&Path> {
        self.external.as_deref()
    }

    pub(super) fn set_external(&mut self, external: Option<PathBuf>) {
        self.external = external;
    }

    pub(super) fn reset_for_subtitle_rebuild(&mut self, external: Option<PathBuf>) {
        self.collection = None;
        self.selected.clear();
        self.audio_tracks.clear();
        self.subtitle_tracks.clear();
        self.subtitle_choice = SubtitleChoice::Automatic;
        self.last_visible_subtitle_choice = SubtitleChoice::Automatic;
        self.external = external;
    }

    pub(super) fn replace_collection(&mut self, collection: gst::StreamCollection) -> bool {
        replace_stream_collection(self, collection);
        self.audio_choice != AudioChoice::Automatic
            || self.subtitle_choice != SubtitleChoice::Automatic
    }

    pub(super) fn select(&mut self, selected: BTreeSet<String>) {
        self.selected = selected;
    }

    pub(super) fn snapshots(&self) -> (AudioSnapshot, SubtitleSnapshot) {
        (audio_snapshot(self), subtitle_snapshot(self))
    }

    pub(super) fn selection_ids(
        &self,
        audio_request: Option<&AudioChoice>,
        subtitle_request: Option<&SubtitleChoice>,
    ) -> Vec<String> {
        stream_selection_ids(self, audio_request, subtitle_request)
    }

    pub(super) fn track_counts(&self) -> (usize, usize) {
        (self.audio_tracks.len(), self.subtitle_tracks.len())
    }

    pub(super) fn set_default_subtitles(&mut self, enabled: bool) {
        self.subtitle_choice = if enabled {
            SubtitleChoice::Automatic
        } else {
            SubtitleChoice::Off
        };
    }

    pub(super) fn reset_subtitle_choice(&mut self) {
        self.subtitle_choice = SubtitleChoice::Automatic;
        self.last_visible_subtitle_choice = SubtitleChoice::Automatic;
    }

    pub(super) fn audio_snapshot(&self) -> AudioSnapshot {
        audio_snapshot(self)
    }

    pub(super) fn subtitle_snapshot(&self) -> SubtitleSnapshot {
        subtitle_snapshot(self)
    }

    pub(super) fn set_audio_choice(&mut self, choice: AudioChoice) {
        self.audio_choice = choice;
    }

    pub(super) fn audio_choice_available(&self, choice: &AudioChoice) -> bool {
        match choice {
            AudioChoice::Automatic => true,
            AudioChoice::Track(id) => self.audio_tracks.iter().any(|track| track.id == *id),
        }
    }

    pub(super) fn set_subtitle_choice(&mut self, choice: SubtitleChoice) {
        if choice != SubtitleChoice::Off {
            self.last_visible_subtitle_choice = choice.clone();
        }
        self.subtitle_choice = choice;
    }

    pub(super) fn subtitle_choice_available(&self, choice: &SubtitleChoice) -> bool {
        match choice {
            SubtitleChoice::Automatic | SubtitleChoice::Off => true,
            SubtitleChoice::Track(id) => self.subtitle_tracks.iter().any(|track| track.id == *id),
        }
    }

    pub(super) fn has_subtitles(&self) -> bool {
        !self.subtitle_tracks.is_empty()
    }

    pub(super) fn toggled_subtitle_choice(&self) -> SubtitleChoice {
        toggled_subtitle_choice(self)
    }

    pub(super) fn cycled_subtitle_choice(&self) -> SubtitleChoice {
        cycled_subtitle_choice(self)
    }
}

/// Find one deterministic automatic sidecar without involving the folder
/// model or GIO. Exact `video.srt` wins, then SRT over WebVTT, then lexical
/// order among language/role suffixes (FR-10.7).
pub(super) fn matching_sidecar(video: &Path) -> Option<PathBuf> {
    let stem = video.file_stem()?.to_str()?;
    let parent = video.parent()?;
    let mut matches: Vec<PathBuf> = fs::read_dir(parent)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && config::is_subtitle(path))
        .filter(|path| {
            path.file_stem()
                .and_then(|candidate| candidate.to_str())
                .is_some_and(|candidate| {
                    candidate == stem
                        || candidate
                            .strip_prefix(stem)
                            .and_then(|suffix| suffix.strip_prefix('.'))
                            .is_some_and(|components| {
                                !components.is_empty()
                                    && components.split('.').all(|component| !component.is_empty())
                            })
                })
        })
        .collect();
    matches.sort_by_key(|path| {
        let candidate = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        let exact = candidate != stem;
        let webvtt = path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("vtt"));
        (
            exact,
            webvtt,
            path.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_lowercase(),
        )
    });
    matches.into_iter().next()
}

fn refresh_stream_tracks(state: &mut StreamState) {
    let Some(collection) = state.collection.as_ref() else {
        state.audio_tracks.clear();
        state.subtitle_tracks.clear();
        return;
    };
    state.audio_tracks = streams_of_type(collection, gst::StreamType::AUDIO)
        .enumerate()
        .filter_map(|(index, stream)| {
            let id = stream.stream_id()?.to_string();
            let label = stream_tag_label(&stream).unwrap_or_else(|| format!("Audio {}", index + 1));
            Some(AudioTrack { id, label })
        })
        .collect();
    let text_streams: Vec<gst::Stream> = (0..collection.size())
        .filter_map(|index| collection.stream(index))
        .filter(|stream| stream.stream_type().contains(gst::StreamType::TEXT))
        .collect();
    // With one text stream, an active `suburi` identifies it. With embedded
    // and external streams together, playbin3 exposes no reliable source URI
    // on GstStream; assigning the filename to the first untagged stream could
    // therefore mislabel an embedded track.
    let mut external_label = (text_streams.len() == 1)
        .then_some(state.external.as_ref())
        .flatten()
        .and_then(|path| {
            path.file_name()
                .map(|name| format!("External — {}", name.to_string_lossy()))
        });
    state.subtitle_tracks = text_streams
        .into_iter()
        .enumerate()
        .filter_map(|(index, stream)| {
            let id = stream.stream_id()?.to_string();
            let label = stream_tag_label(&stream)
                .or_else(|| external_label.take())
                .unwrap_or_else(|| format!("Subtitle {}", index + 1));
            Some(SubtitleTrack { id, label })
        })
        .collect();
}

fn stream_tag_label(stream: &gst::Stream) -> Option<String> {
    let tags = stream.tags();
    tags.as_ref()
        .and_then(|tags| tags.get::<gst::tags::Title>())
        .map(|value| value.get().to_string())
        .or_else(|| {
            tags.as_ref()
                .and_then(|tags| tags.get::<gst::tags::LanguageName>())
                .map(|value| value.get().to_string())
        })
        .or_else(|| {
            tags.as_ref()
                .and_then(|tags| tags.get::<gst::tags::LanguageCode>())
                .map(|value| value.get().to_string())
        })
}

fn replace_stream_collection(state: &mut StreamState, collection: gst::StreamCollection) {
    state.collection = Some(collection);
    refresh_stream_tracks(state);
    if matches!(
        &state.audio_choice,
        AudioChoice::Track(id) if !state.audio_tracks.iter().any(|track| track.id == *id)
    ) {
        state.audio_choice = AudioChoice::Automatic;
    }
    if matches!(
        &state.subtitle_choice,
        SubtitleChoice::Track(id)
            if !state.subtitle_tracks.iter().any(|track| track.id == *id)
    ) {
        state.subtitle_choice = SubtitleChoice::Automatic;
    }
    if matches!(
        &state.last_visible_subtitle_choice,
        SubtitleChoice::Track(id)
            if !state.subtitle_tracks.iter().any(|track| track.id == *id)
    ) {
        state.last_visible_subtitle_choice = SubtitleChoice::Automatic;
    }
}

fn audio_snapshot(state: &StreamState) -> AudioSnapshot {
    let active_label = selected_stream_id(state, gst::StreamType::AUDIO).and_then(|id| {
        state
            .audio_tracks
            .iter()
            .find(|track| track.id == id)
            .map(|track| track.label.clone())
    });
    AudioSnapshot {
        tracks: state.audio_tracks.clone(),
        choice: state.audio_choice.clone(),
        active_label,
    }
}

fn subtitle_snapshot(state: &StreamState) -> SubtitleSnapshot {
    let active_label = selected_text_id(state).and_then(|id| {
        state
            .subtitle_tracks
            .iter()
            .find(|track| track.id == id)
            .map(|track| track.label.clone())
    });
    SubtitleSnapshot {
        tracks: state.subtitle_tracks.clone(),
        choice: state.subtitle_choice.clone(),
        active_label,
    }
}

fn toggled_subtitle_choice(state: &StreamState) -> SubtitleChoice {
    if state.subtitle_choice != SubtitleChoice::Off {
        return SubtitleChoice::Off;
    }
    match &state.last_visible_subtitle_choice {
        SubtitleChoice::Track(id) if state.subtitle_tracks.iter().any(|track| track.id == *id) => {
            SubtitleChoice::Track(id.clone())
        }
        SubtitleChoice::Automatic | SubtitleChoice::Track(_) | SubtitleChoice::Off => {
            SubtitleChoice::Automatic
        }
    }
}

fn cycled_subtitle_choice(state: &StreamState) -> SubtitleChoice {
    if state.subtitle_choice == SubtitleChoice::Off {
        return state
            .subtitle_tracks
            .first()
            .map_or(SubtitleChoice::Off, |track| {
                SubtitleChoice::Track(track.id.clone())
            });
    }
    let current = match &state.subtitle_choice {
        SubtitleChoice::Track(id) => Some(id.as_str()),
        SubtitleChoice::Automatic => selected_text_id(state),
        SubtitleChoice::Off => None,
    };
    current
        .and_then(|id| {
            state
                .subtitle_tracks
                .iter()
                .position(|track| track.id == id)
        })
        .and_then(|index| state.subtitle_tracks.get(index + 1))
        .map_or(SubtitleChoice::Off, |track| {
            SubtitleChoice::Track(track.id.clone())
        })
}

fn selected_text_id(state: &StreamState) -> Option<&str> {
    selected_stream_id(state, gst::StreamType::TEXT)
}

fn selected_stream_id(state: &StreamState, kind: gst::StreamType) -> Option<&str> {
    let collection = state.collection.as_ref()?;
    state
        .selected
        .iter()
        .find(|id| {
            stream_by_id(collection, id).is_some_and(|stream| stream.stream_type().contains(kind))
        })
        .map(String::as_str)
}

fn stream_selection_ids(
    state: &StreamState,
    audio_request: Option<&AudioChoice>,
    subtitle_request: Option<&SubtitleChoice>,
) -> Vec<String> {
    let Some(collection) = state.collection.as_ref() else {
        return Vec::new();
    };

    let mut selected = Vec::new();
    let video_id = selected_stream_id(state, gst::StreamType::VIDEO)
        .map(str::to_string)
        .or_else(|| default_stream_id(collection, gst::StreamType::VIDEO));
    if let Some(id) = video_id {
        selected.push(id);
    }

    let audio_choice = audio_request.unwrap_or(&state.audio_choice);
    let audio_id = match audio_choice {
        AudioChoice::Track(id) if state.audio_tracks.iter().any(|track| track.id == *id) => {
            Some(id.clone())
        }
        AudioChoice::Automatic if audio_request.is_none() => {
            selected_stream_id(state, gst::StreamType::AUDIO)
                .map(str::to_string)
                .or_else(|| default_stream_id(collection, gst::StreamType::AUDIO))
        }
        AudioChoice::Automatic | AudioChoice::Track(_) => {
            default_stream_id(collection, gst::StreamType::AUDIO)
        }
    };
    if let Some(id) = audio_id {
        selected.push(id);
    }

    let subtitle_choice = subtitle_request.unwrap_or(&state.subtitle_choice);
    let text_id = match subtitle_choice {
        SubtitleChoice::Off => None,
        SubtitleChoice::Track(id) if state.subtitle_tracks.iter().any(|track| track.id == *id) => {
            Some(id.clone())
        }
        SubtitleChoice::Automatic if subtitle_request.is_none() => selected_text_id(state)
            .map(str::to_string)
            .or_else(|| default_stream_id(collection, gst::StreamType::TEXT)),
        SubtitleChoice::Automatic | SubtitleChoice::Track(_) => {
            default_stream_id(collection, gst::StreamType::TEXT)
        }
    };
    if let Some(id) = text_id {
        selected.push(id);
    }
    selected
}

fn default_stream_id(collection: &gst::StreamCollection, kind: gst::StreamType) -> Option<String> {
    streams_of_type(collection, kind)
        .find(|stream| stream.stream_flags().contains(gst::StreamFlags::SELECT))
        .or_else(|| {
            streams_of_type(collection, kind)
                .find(|stream| !stream.stream_flags().contains(gst::StreamFlags::UNSELECT))
        })
        .and_then(|stream| stream.stream_id())
        .map(String::from)
}

fn streams_of_type(
    collection: &gst::StreamCollection,
    kind: gst::StreamType,
) -> impl Iterator<Item = gst::Stream> + '_ {
    (0..collection.size())
        .filter_map(|index| collection.stream(index))
        .filter(move |stream| stream.stream_type().contains(kind))
}

fn stream_by_id(collection: &gst::StreamCollection, id: &str) -> Option<gst::Stream> {
    (0..collection.size())
        .filter_map(|index| collection.stream(index))
        .find(|stream| stream.stream_id().as_deref() == Some(id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_srt_sidecar_wins_over_language_and_webvtt_variants() {
        let dir =
            std::env::temp_dir().join(format!("open-mpv-sidecar-exact-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["movie.mkv", "movie.en.srt", "movie.vtt", "movie.srt"] {
            std::fs::write(dir.join(name), []).unwrap();
        }

        assert_eq!(
            matching_sidecar(&dir.join("movie.mkv")),
            Some(dir.join("movie.srt"))
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn sidecar_matching_rejects_prefix_collisions() {
        let dir =
            std::env::temp_dir().join(format!("open-mpv-sidecar-prefix-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["movie.mkv", "movie2.srt", "movie..srt", "movie.en.vtt"] {
            std::fs::write(dir.join(name), []).unwrap();
        }

        assert_eq!(
            matching_sidecar(&dir.join("movie.mkv")),
            Some(dir.join("movie.en.vtt"))
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn stream_selection_preserves_chosen_audio_video_and_subtitle() {
        gst::init().unwrap();
        let video = gst::Stream::new(
            Some("video"),
            None,
            gst::StreamType::VIDEO,
            gst::StreamFlags::SELECT,
        );
        let english_audio = gst::Stream::new(
            Some("english-audio"),
            None,
            gst::StreamType::AUDIO,
            gst::StreamFlags::SELECT,
        );
        let commentary = gst::Stream::new(
            Some("commentary"),
            None,
            gst::StreamType::AUDIO,
            gst::StreamFlags::empty(),
        );
        let english = gst::Stream::new(
            Some("english"),
            None,
            gst::StreamType::TEXT,
            gst::StreamFlags::SELECT,
        );
        let hindi = gst::Stream::new(
            Some("hindi"),
            None,
            gst::StreamType::TEXT,
            gst::StreamFlags::empty(),
        );
        let collection = gst::StreamCollection::builder(None)
            .streams([video, english_audio, commentary, english, hindi])
            .build();
        let mut state = StreamState {
            collection: Some(collection),
            audio_choice: AudioChoice::Track("commentary".into()),
            subtitle_choice: SubtitleChoice::Track("hindi".into()),
            ..StreamState::default()
        };
        refresh_stream_tracks(&mut state);

        assert_eq!(
            stream_selection_ids(&state, None, None),
            ["video", "commentary", "hindi"]
        );
    }

    #[test]
    fn subtitle_change_preserves_the_active_automatic_audio_stream() {
        gst::init().unwrap();
        let stream = |id, kind, flags| gst::Stream::new(Some(id), None, kind, flags);
        let collection = gst::StreamCollection::builder(None)
            .streams([
                stream("video", gst::StreamType::VIDEO, gst::StreamFlags::SELECT),
                stream(
                    "english-audio",
                    gst::StreamType::AUDIO,
                    gst::StreamFlags::SELECT,
                ),
                stream(
                    "commentary",
                    gst::StreamType::AUDIO,
                    gst::StreamFlags::empty(),
                ),
                stream(
                    "english-text",
                    gst::StreamType::TEXT,
                    gst::StreamFlags::SELECT,
                ),
                stream(
                    "hindi-text",
                    gst::StreamType::TEXT,
                    gst::StreamFlags::empty(),
                ),
            ])
            .build();
        let mut state = StreamState {
            collection: Some(collection),
            ..StreamState::default()
        };
        refresh_stream_tracks(&mut state);
        state.selected.extend([
            "video".to_string(),
            "commentary".to_string(),
            "hindi-text".to_string(),
        ]);

        assert_eq!(
            stream_selection_ids(
                &state,
                None,
                Some(&SubtitleChoice::Track("english-text".into())),
            ),
            ["video", "commentary", "english-text"]
        );
        assert_eq!(
            stream_selection_ids(&state, Some(&AudioChoice::Automatic), None),
            ["video", "english-audio", "hindi-text"]
        );
    }

    #[test]
    fn replacement_collection_retains_valid_audio_choice_and_resets_missing_choice() {
        gst::init().unwrap();
        let collection = |audio_ids: &[&str]| {
            let video = gst::Stream::new(
                Some("video"),
                None,
                gst::StreamType::VIDEO,
                gst::StreamFlags::SELECT,
            );
            let audio = audio_ids.iter().enumerate().map(|(index, id)| {
                gst::Stream::new(
                    Some(id),
                    None,
                    gst::StreamType::AUDIO,
                    if index == 0 {
                        gst::StreamFlags::SELECT
                    } else {
                        gst::StreamFlags::empty()
                    },
                )
            });
            gst::StreamCollection::builder(None)
                .streams(std::iter::once(video).chain(audio))
                .build()
        };
        let mut state = StreamState {
            audio_choice: AudioChoice::Track("commentary".into()),
            ..StreamState::default()
        };

        replace_stream_collection(&mut state, collection(&["english", "commentary"]));
        assert_eq!(state.audio_choice, AudioChoice::Track("commentary".into()));

        replace_stream_collection(&mut state, collection(&["english", "descriptive"]));
        assert_eq!(state.audio_choice, AudioChoice::Automatic);
    }

    #[test]
    fn audio_track_labels_use_title_language_then_stable_fallback() {
        gst::init().unwrap();
        let titled = gst::Stream::new(
            Some("commentary"),
            None,
            gst::StreamType::AUDIO,
            gst::StreamFlags::SELECT,
        );
        let mut title_tags = gst::TagList::new();
        title_tags
            .get_mut()
            .unwrap()
            .add::<gst::tags::Title>(&"Director Commentary", gst::TagMergeMode::Append);
        titled.set_tags(Some(&title_tags));

        let language = gst::Stream::new(
            Some("hindi"),
            None,
            gst::StreamType::AUDIO,
            gst::StreamFlags::empty(),
        );
        let mut language_tags = gst::TagList::new();
        language_tags
            .get_mut()
            .unwrap()
            .add::<gst::tags::LanguageName>(&"Hindi", gst::TagMergeMode::Append);
        language.set_tags(Some(&language_tags));

        let untagged = gst::Stream::new(
            Some("other"),
            None,
            gst::StreamType::AUDIO,
            gst::StreamFlags::empty(),
        );
        let mut state = StreamState {
            collection: Some(
                gst::StreamCollection::builder(None)
                    .streams([titled, language, untagged])
                    .build(),
            ),
            ..StreamState::default()
        };
        refresh_stream_tracks(&mut state);

        assert_eq!(
            state
                .audio_tracks
                .iter()
                .map(|track| track.label.as_str())
                .collect::<Vec<_>>(),
            ["Director Commentary", "Hindi", "Audio 3"]
        );
    }

    #[test]
    fn subtitle_visibility_toggle_restores_the_selected_track() {
        assert_eq!(
            SubtitleChoice::Track("off".into()).action_target(),
            "track:off"
        );
        let hindi = SubtitleTrack {
            id: "hindi".into(),
            label: "Hindi".into(),
        };
        let mut state = StreamState {
            subtitle_tracks: vec![hindi],
            subtitle_choice: SubtitleChoice::Track("hindi".into()),
            last_visible_subtitle_choice: SubtitleChoice::Track("hindi".into()),
            ..StreamState::default()
        };

        assert_eq!(toggled_subtitle_choice(&state), SubtitleChoice::Off);
        state.subtitle_choice = SubtitleChoice::Off;
        assert_eq!(
            toggled_subtitle_choice(&state),
            SubtitleChoice::Track("hindi".into())
        );

        state.subtitle_tracks.clear();
        assert_eq!(toggled_subtitle_choice(&state), SubtitleChoice::Automatic);
    }

    #[test]
    fn rapid_subtitle_cycles_follow_the_requested_track_not_stale_bus_state() {
        let mut state = StreamState {
            subtitle_tracks: vec![
                SubtitleTrack {
                    id: "english".into(),
                    label: "English".into(),
                },
                SubtitleTrack {
                    id: "hindi".into(),
                    label: "Hindi".into(),
                },
            ],
            subtitle_choice: SubtitleChoice::Track("hindi".into()),
            ..StreamState::default()
        };
        state.selected.insert("english".into());

        assert_eq!(cycled_subtitle_choice(&state), SubtitleChoice::Off);
        state.subtitle_choice = SubtitleChoice::Off;
        assert_eq!(
            cycled_subtitle_choice(&state),
            SubtitleChoice::Track("english".into())
        );
    }

    #[test]
    fn external_filename_is_not_assigned_to_an_ambiguous_embedded_track() {
        gst::init().unwrap();
        let text = |id| {
            gst::Stream::new(
                Some(id),
                None,
                gst::StreamType::TEXT,
                gst::StreamFlags::SELECT,
            )
        };
        let mut state = StreamState {
            collection: Some(
                gst::StreamCollection::builder(None)
                    .streams([text("embedded"), text("external")])
                    .build(),
            ),
            external: Some(std::path::PathBuf::from("movie.en.srt")),
            ..StreamState::default()
        };

        refresh_stream_tracks(&mut state);
        assert_eq!(state.subtitle_tracks[0].label, "Subtitle 1");
        assert_eq!(state.subtitle_tracks[1].label, "Subtitle 2");

        state.collection = Some(
            gst::StreamCollection::builder(None)
                .streams([text("external")])
                .build(),
        );
        refresh_stream_tracks(&mut state);
        assert_eq!(state.subtitle_tracks[0].label, "External — movie.en.srt");
    }

    #[test]
    fn collection_changes_revalidate_stream_choices() {
        gst::init().unwrap();
        let stream = |id, kind| gst::Stream::new(Some(id), None, kind, gst::StreamFlags::SELECT);
        let mut streams = StreamState::default();
        streams.set_audio_choice(AudioChoice::Track("commentary".into()));
        streams.set_subtitle_choice(SubtitleChoice::Track("english".into()));

        streams.replace_collection(
            gst::StreamCollection::builder(None)
                .streams([
                    stream("video", gst::StreamType::VIDEO),
                    stream("commentary", gst::StreamType::AUDIO),
                    stream("english", gst::StreamType::TEXT),
                ])
                .build(),
        );
        assert_eq!(
            streams.audio_snapshot().choice,
            AudioChoice::Track("commentary".into())
        );

        streams.replace_collection(
            gst::StreamCollection::builder(None)
                .streams([
                    stream("video-2", gst::StreamType::VIDEO),
                    stream("main", gst::StreamType::AUDIO),
                ])
                .build(),
        );
        assert_eq!(streams.audio_snapshot().choice, AudioChoice::Automatic);
        assert_eq!(
            streams.subtitle_snapshot().choice,
            SubtitleChoice::Automatic
        );
    }
}
