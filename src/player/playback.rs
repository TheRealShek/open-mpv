//! Owns the focused playback session: transport, bounded seeking, rate requests,
//! stream choices, sidecar recovery sequencing, and generations.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use gstreamer as gst;
use gstreamer::prelude::*;

use super::tracks::{
    AudioChoice, AudioSnapshot, AudioTrack, SubtitleChoice, SubtitleSnapshot, SubtitleTrack,
};

/// Seeks land on the exact target, not on the nearest keyframe. Keyframe
/// seeks are cheaper, but short clips are routinely encoded as a single
/// GOP — every seek then snaps back to 0:00 and the video looks stuck at
/// the start. Measured on this machine, an accurate seek costs 2–455 ms,
/// and at most one is ever in flight (see `SeekState`).
const SEEK_FLAGS: gst::SeekFlags = gst::SeekFlags::FLUSH.union(gst::SeekFlags::ACCURATE);
/// Safety net: if a seek never gets its `AsyncDone` (broken file, stalled
/// demuxer), stop reporting its target and fall back to real queries.
const SEEK_SETTLE: Duration = Duration::from_millis(1500);
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackRateError {
    PitchFilterUnavailable,
    PositionUnavailable,
    SeekRefused,
}

impl fmt::Display for PlaybackRateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PlaybackRateError::PitchFilterUnavailable => {
                f.write_str("playback speed requires the GStreamer scaletempo plugin")
            }
            PlaybackRateError::PositionUnavailable => {
                f.write_str("playback speed is not ready yet")
            }
            PlaybackRateError::SeekRefused => {
                f.write_str("this video cannot change playback speed")
            }
        }
    }
}

impl std::error::Error for PlaybackRateError {}

#[derive(Clone, Copy, Debug, PartialEq)]
enum ResumeStage {
    Preroll,
    Seek,
}

#[derive(Debug, PartialEq)]
pub(super) enum ResumeAction {
    Seek {
        position: f64,
        rate: f64,
        resume_playing: bool,
    },
    Finish {
        resume_playing: bool,
    },
    None,
}

pub(super) struct StreamUpdate {
    pub(super) audio_count: usize,
    pub(super) subtitle_count: usize,
    pub(super) selection: Option<Vec<String>>,
    pub(super) audio: AudioSnapshot,
    pub(super) subtitles: SubtitleSnapshot,
}

struct ResumeState {
    position: f64,
    rate: f64,
    play_after_seek: bool,
    stage: ResumeStage,
}

/// A flushing seek only answers position queries with the new position
/// once the pipeline has re-prerolled; until then it still reports where
/// it was. Two consequences the UI would otherwise wear: the seek bar
/// snaps backwards after every scrub step, and repeated `seek_by` calls
/// all compute their delta from the same stale position. `in_flight`
/// covers that gap, and `queued` coalesces the scrub positions that
/// arrive while a seek is running — issuing them all would flood the
/// pipeline with flushes and leave the picture trailing the pointer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct SeekRequest {
    position: f64,
    rate: f64,
}

impl SeekRequest {
    pub(super) fn new(position: f64, rate: f64) -> Self {
        Self { position, rate }
    }

    pub(super) fn rate(self) -> f64 {
        self.rate
    }
}

#[derive(Default)]
struct SeekState {
    in_flight: Option<(SeekRequest, Instant)>,
    queued: Option<SeekRequest>,
}

/// All temporal facts for the active Focused playback session. Commands and
/// GStreamer observations both enter through this model, so sequencing does
/// not depend on keeping several independently borrowed cells in sync.
pub(super) struct FocusedPlayback {
    generation: u64,
    current_video: Option<PathBuf>,
    playing: bool,
    /// Last rate accepted by the pipeline. A queued seek may advertise a
    /// newer requested rate without changing this until GStreamer accepts it.
    playback_rate: f64,
    /// Cached so transport updates do not query the demuxer every frame.
    duration: Option<f64>,
    seek: SeekState,
    resume: Option<ResumeState>,
    error_pending: bool,
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

impl Default for FocusedPlayback {
    fn default() -> Self {
        Self {
            generation: 0,
            current_video: None,
            playing: false,
            playback_rate: 1.0,
            duration: None,
            seek: SeekState::default(),
            resume: None,
            error_pending: false,
            collection: None,
            selected: BTreeSet::new(),
            audio_tracks: Vec::new(),
            audio_choice: AudioChoice::default(),
            subtitle_tracks: Vec::new(),
            subtitle_choice: SubtitleChoice::default(),
            last_visible_subtitle_choice: SubtitleChoice::default(),
            external: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ErrorContext {
    generation: u64,
    external: Option<PathBuf>,
}

impl FocusedPlayback {
    pub(super) fn reset(&mut self, subtitles_default_on: bool) {
        let generation = self.generation.wrapping_add(1);
        *self = Self {
            generation,
            subtitle_choice: if subtitles_default_on {
                SubtitleChoice::Automatic
            } else {
                SubtitleChoice::Off
            },
            ..Self::default()
        };
    }

    pub(super) fn start_video(
        &mut self,
        path: &Path,
        external: Option<PathBuf>,
        subtitles_default_on: bool,
    ) {
        self.reset(subtitles_default_on);
        self.current_video = Some(path.to_path_buf());
        self.external = external;
    }

    pub(super) fn playback_started(&mut self) {
        self.playing = true;
    }

    pub(super) fn forget_timing(&mut self) {
        self.seek = SeekState::default();
        self.playback_rate = 1.0;
        self.duration = None;
    }

    pub(super) fn requested_rate(&self) -> f64 {
        self.seek
            .pending()
            .map_or(self.playback_rate, |request| request.rate)
    }

    pub(super) fn request_seek(&mut self, request: SeekRequest) -> bool {
        self.seek.request(request)
    }

    pub(super) fn begin_seek(&mut self, request: SeekRequest) {
        self.seek.in_flight = Some((request, Instant::now()));
        self.seek.queued = None;
    }

    pub(super) fn accept_seek(&mut self, request: SeekRequest) {
        self.playback_rate = request.rate;
    }

    pub(super) fn seek_refused(&mut self) {
        self.seek.in_flight = None;
    }

    pub(super) fn prepare_subtitle_rebuild(
        &mut self,
        position: f64,
        rate: f64,
        play_after_seek: bool,
        external: Option<PathBuf>,
    ) {
        self.forget_timing();
        self.collection = None;
        self.selected.clear();
        self.audio_tracks.clear();
        self.subtitle_tracks.clear();
        self.subtitle_choice = SubtitleChoice::Automatic;
        self.last_visible_subtitle_choice = SubtitleChoice::Automatic;
        self.external = external;
        self.resume = Some(ResumeState {
            position,
            rate,
            play_after_seek,
            stage: ResumeStage::Preroll,
        });
    }

    pub(super) fn observe_async_done(&mut self) -> ResumeAction {
        match self.resume.as_mut() {
            Some(state) if matches!(state.stage, ResumeStage::Preroll) => {
                state.stage = ResumeStage::Seek;
                ResumeAction::Seek {
                    position: state.position,
                    rate: state.rate,
                    resume_playing: state.play_after_seek,
                }
            }
            Some(state) => {
                let resume_playing = state.play_after_seek;
                self.resume = None;
                ResumeAction::Finish { resume_playing }
            }
            None => ResumeAction::None,
        }
    }

    pub(super) fn finish_seek(&mut self) -> Option<SeekRequest> {
        self.seek.in_flight = None;
        self.seek.queued.take()
    }

    pub(super) fn accepted_rate(&self) -> f64 {
        self.playback_rate
    }

    pub(super) fn cancel_resume(&mut self) {
        self.resume = None;
    }

    pub(super) fn set_playing(&mut self, playing: bool) {
        self.playing = playing;
    }

    pub(super) fn is_playing(&self) -> bool {
        self.playing
    }

    pub(super) fn invalidate_duration(&mut self) {
        self.duration = None;
    }

    pub(super) fn cached_duration(&self) -> Option<f64> {
        self.duration
    }

    pub(super) fn cache_duration(&mut self, duration: f64) {
        self.duration = Some(duration);
    }

    pub(super) fn pending_seek_position(&self) -> Option<f64> {
        self.seek.pending().map(|request| request.position)
    }

    pub(super) fn current_video(&self) -> Option<&Path> {
        self.current_video.as_deref()
    }

    pub(super) fn has_external_subtitle(&self) -> bool {
        self.external.is_some()
    }

    pub(super) fn external_subtitle(&self) -> Option<&Path> {
        self.external.as_deref()
    }

    pub(super) fn set_default_subtitles(&mut self, enabled: bool) {
        self.subtitle_choice = if enabled {
            SubtitleChoice::Automatic
        } else {
            SubtitleChoice::Off
        };
    }

    pub(super) fn observe_stream_collection(
        &mut self,
        collection: gst::StreamCollection,
    ) -> StreamUpdate {
        replace_stream_collection(self, collection);
        let selection = (self.audio_choice != AudioChoice::Automatic
            || self.subtitle_choice != SubtitleChoice::Automatic)
            .then(|| stream_selection_ids(self, None, None));
        StreamUpdate {
            audio_count: self.audio_tracks.len(),
            subtitle_count: self.subtitle_tracks.len(),
            selection,
            audio: audio_snapshot(self),
            subtitles: subtitle_snapshot(self),
        }
    }

    pub(super) fn observe_streams_selected(
        &mut self,
        selected: BTreeSet<String>,
    ) -> (AudioSnapshot, SubtitleSnapshot) {
        self.selected = selected;
        (audio_snapshot(self), subtitle_snapshot(self))
    }

    pub(super) fn audio_snapshot(&self) -> AudioSnapshot {
        audio_snapshot(self)
    }

    pub(super) fn subtitle_snapshot(&self) -> SubtitleSnapshot {
        subtitle_snapshot(self)
    }

    pub(super) fn requested_audio_choice(&self, choice: &AudioChoice) -> Option<Vec<String>> {
        audio_choice_available(self, choice).then(|| stream_selection_ids(self, Some(choice), None))
    }

    pub(super) fn set_audio_choice(&mut self, choice: AudioChoice) {
        self.audio_choice = choice;
    }

    pub(super) fn requested_subtitle_choice(&self, choice: &SubtitleChoice) -> Option<Vec<String>> {
        subtitle_choice_available(self, choice)
            .then(|| stream_selection_ids(self, None, Some(choice)))
    }

    pub(super) fn set_subtitle_choice(&mut self, choice: SubtitleChoice) {
        if choice != SubtitleChoice::Off {
            self.last_visible_subtitle_choice = choice.clone();
        }
        self.subtitle_choice = choice;
    }

    pub(super) fn reset_subtitle_choice(&mut self) {
        self.subtitle_choice = SubtitleChoice::Automatic;
        self.last_visible_subtitle_choice = SubtitleChoice::Automatic;
    }

    pub(super) fn subtitle_toggle_choice(&self) -> Option<SubtitleChoice> {
        (!self.subtitle_tracks.is_empty()).then(|| toggled_subtitle_choice(self))
    }

    pub(super) fn subtitle_cycle_choice(&self) -> Option<SubtitleChoice> {
        (!self.subtitle_tracks.is_empty()).then(|| cycled_subtitle_choice(self))
    }

    pub(super) fn resume_point(&self) -> (f64, f64, bool) {
        self.resume
            .as_ref()
            .map_or((0.0, self.playback_rate, true), |state| {
                (state.position, state.rate, state.play_after_seek)
            })
    }

    pub(super) fn begin_error(&mut self) -> Option<ErrorContext> {
        if self.error_pending {
            return None;
        }
        self.error_pending = true;
        Some(self.context())
    }

    pub(super) fn context(&self) -> ErrorContext {
        ErrorContext {
            generation: self.generation,
            external: self.external.clone(),
        }
    }

    pub(super) fn error_is_current(&self, context: &ErrorContext) -> bool {
        self.generation == context.generation
            && self.external.as_deref() == context.external.as_deref()
    }

    pub(super) fn finish_error(&mut self, context: &ErrorContext) {
        if self.generation == context.generation {
            self.error_pending = false;
        }
    }
}

fn refresh_stream_tracks(playback: &mut FocusedPlayback) {
    let Some(collection) = playback.collection.as_ref() else {
        playback.audio_tracks.clear();
        playback.subtitle_tracks.clear();
        return;
    };
    playback.audio_tracks = streams_of_type(collection, gst::StreamType::AUDIO)
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
        .then_some(playback.external.as_ref())
        .flatten()
        .and_then(|path| {
            path.file_name()
                .map(|name| format!("External — {}", name.to_string_lossy()))
        });
    playback.subtitle_tracks = text_streams
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

fn replace_stream_collection(playback: &mut FocusedPlayback, collection: gst::StreamCollection) {
    playback.collection = Some(collection);
    refresh_stream_tracks(playback);
    if matches!(
        &playback.audio_choice,
        AudioChoice::Track(id) if !playback.audio_tracks.iter().any(|track| track.id == *id)
    ) {
        playback.audio_choice = AudioChoice::Automatic;
    }
    if matches!(
        &playback.subtitle_choice,
        SubtitleChoice::Track(id)
            if !playback.subtitle_tracks.iter().any(|track| track.id == *id)
    ) {
        playback.subtitle_choice = SubtitleChoice::Automatic;
    }
    if matches!(
        &playback.last_visible_subtitle_choice,
        SubtitleChoice::Track(id)
            if !playback.subtitle_tracks.iter().any(|track| track.id == *id)
    ) {
        playback.last_visible_subtitle_choice = SubtitleChoice::Automatic;
    }
}

fn audio_snapshot(playback: &FocusedPlayback) -> AudioSnapshot {
    let active_label = selected_stream_id(playback, gst::StreamType::AUDIO).and_then(|id| {
        playback
            .audio_tracks
            .iter()
            .find(|track| track.id == id)
            .map(|track| track.label.clone())
    });
    AudioSnapshot {
        tracks: playback.audio_tracks.clone(),
        choice: playback.audio_choice.clone(),
        active_label,
    }
}

fn subtitle_snapshot(playback: &FocusedPlayback) -> SubtitleSnapshot {
    let active_label = selected_text_id(playback).and_then(|id| {
        playback
            .subtitle_tracks
            .iter()
            .find(|track| track.id == id)
            .map(|track| track.label.clone())
    });
    SubtitleSnapshot {
        tracks: playback.subtitle_tracks.clone(),
        choice: playback.subtitle_choice.clone(),
        active_label,
    }
}

fn audio_choice_available(playback: &FocusedPlayback, choice: &AudioChoice) -> bool {
    match choice {
        AudioChoice::Automatic => true,
        AudioChoice::Track(id) => playback.audio_tracks.iter().any(|track| track.id == *id),
    }
}

fn subtitle_choice_available(playback: &FocusedPlayback, choice: &SubtitleChoice) -> bool {
    match choice {
        SubtitleChoice::Automatic | SubtitleChoice::Off => true,
        SubtitleChoice::Track(id) => playback.subtitle_tracks.iter().any(|track| track.id == *id),
    }
}

fn toggled_subtitle_choice(playback: &FocusedPlayback) -> SubtitleChoice {
    if playback.subtitle_choice != SubtitleChoice::Off {
        return SubtitleChoice::Off;
    }
    match &playback.last_visible_subtitle_choice {
        SubtitleChoice::Track(id)
            if playback.subtitle_tracks.iter().any(|track| track.id == *id) =>
        {
            SubtitleChoice::Track(id.clone())
        }
        SubtitleChoice::Automatic | SubtitleChoice::Track(_) | SubtitleChoice::Off => {
            SubtitleChoice::Automatic
        }
    }
}

fn cycled_subtitle_choice(playback: &FocusedPlayback) -> SubtitleChoice {
    if playback.subtitle_choice == SubtitleChoice::Off {
        return playback
            .subtitle_tracks
            .first()
            .map_or(SubtitleChoice::Off, |track| {
                SubtitleChoice::Track(track.id.clone())
            });
    }
    let current = match &playback.subtitle_choice {
        SubtitleChoice::Track(id) => Some(id.as_str()),
        SubtitleChoice::Automatic => selected_text_id(playback),
        SubtitleChoice::Off => None,
    };
    current
        .and_then(|id| {
            playback
                .subtitle_tracks
                .iter()
                .position(|track| track.id == id)
        })
        .and_then(|index| playback.subtitle_tracks.get(index + 1))
        .map_or(SubtitleChoice::Off, |track| {
            SubtitleChoice::Track(track.id.clone())
        })
}

fn selected_text_id(playback: &FocusedPlayback) -> Option<&str> {
    selected_stream_id(playback, gst::StreamType::TEXT)
}

fn selected_stream_id(playback: &FocusedPlayback, kind: gst::StreamType) -> Option<&str> {
    let collection = playback.collection.as_ref()?;
    playback
        .selected
        .iter()
        .find(|id| {
            stream_by_id(collection, id).is_some_and(|stream| stream.stream_type().contains(kind))
        })
        .map(String::as_str)
}

fn stream_selection_ids(
    playback: &FocusedPlayback,
    audio_request: Option<&AudioChoice>,
    subtitle_request: Option<&SubtitleChoice>,
) -> Vec<String> {
    let Some(collection) = playback.collection.as_ref() else {
        return Vec::new();
    };

    let mut selected = Vec::new();
    let video_id = selected_stream_id(playback, gst::StreamType::VIDEO)
        .map(str::to_string)
        .or_else(|| default_stream_id(collection, gst::StreamType::VIDEO));
    if let Some(id) = video_id {
        selected.push(id);
    }

    let audio_choice = audio_request.unwrap_or(&playback.audio_choice);
    let audio_id = match audio_choice {
        AudioChoice::Track(id) if playback.audio_tracks.iter().any(|track| track.id == *id) => {
            Some(id.clone())
        }
        AudioChoice::Automatic if audio_request.is_none() => {
            selected_stream_id(playback, gst::StreamType::AUDIO)
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

    let subtitle_choice = subtitle_request.unwrap_or(&playback.subtitle_choice);
    let text_id = match subtitle_choice {
        SubtitleChoice::Off => None,
        SubtitleChoice::Track(id)
            if playback.subtitle_tracks.iter().any(|track| track.id == *id) =>
        {
            Some(id.clone())
        }
        SubtitleChoice::Automatic if subtitle_request.is_none() => selected_text_id(playback)
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

impl SeekState {
    /// Where playback is headed, while it is still on its way there.
    pub(super) fn pending(&self) -> Option<SeekRequest> {
        self.queued
            .or_else(|| self.running().map(|(request, _)| request))
    }

    /// The in-flight seek, unless it is old enough to count as lost.
    pub(super) fn running(&self) -> Option<(SeekRequest, Instant)> {
        self.in_flight.filter(|(_, at)| at.elapsed() < SEEK_SETTLE)
    }

    /// Record a request. Returns true when the caller
    /// must issue it, false when the running seek will pick it up.
    pub(super) fn request(&mut self, request: SeekRequest) -> bool {
        if self.running().is_some() {
            self.queued = Some(request);
            return false;
        }
        // A seek older than SEEK_SETTLE no longer owns the UI's pending
        // position and must not keep newer input queued indefinitely.
        self.in_flight = None;
        self.queued = None;
        true
    }
}

pub(super) fn same_rate(left: f64, right: f64) -> bool {
    (left - right).abs() < f64::EPSILON
}

/// Send the seek and record it as in flight. Free-standing because the bus
/// watch flushes queued scrub/rate requests without holding a `Player`.
pub(super) fn issue_seek(
    seek_target: &gst::Element,
    playback: &RefCell<FocusedPlayback>,
    request: SeekRequest,
) -> bool {
    let position = request.position.max(0.0);
    let Ok(target) = gst::ClockTime::try_from_seconds_f64(position) else {
        crate::applog!("player: refusing invalid seek target {position}");
        return false;
    };
    playback.borrow_mut().begin_seek(request);
    let sent = seek_target
        .seek(
            request.rate,
            SEEK_FLAGS,
            gst::SeekType::Set,
            target,
            gst::SeekType::None,
            gst::ClockTime::NONE,
        )
        .is_ok();
    if sent {
        playback.borrow_mut().accept_seek(request);
        crate::applog!("player: seek to {position:.1}s at {:.2}x", request.rate);
    } else {
        playback.borrow_mut().seek_refused();
        crate::applog!(
            "player: seek to {position:.1}s at {:.2}x was refused",
            request.rate
        );
    }
    sent
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Stand-in for what `issue_seek` records on the pipeline's behalf.
    fn request(position: f64, rate: f64) -> SeekRequest {
        SeekRequest { position, rate }
    }

    fn issued(state: &mut SeekState, request: SeekRequest, ago: std::time::Duration) {
        state.in_flight = Some((request, Instant::now() - ago));
        state.queued = None;
    }

    #[test]
    fn first_seek_goes_out_immediately() {
        let mut state = SeekState::default();
        assert!(state.request(request(12.0, 1.0)));
        assert_eq!(state.queued, None);
    }

    #[test]
    fn scrubbing_during_a_seek_keeps_only_the_newest_position() {
        let mut state = SeekState::default();
        issued(&mut state, request(12.0, 1.0), std::time::Duration::ZERO);
        assert!(!state.request(request(20.0, 1.0)));
        assert!(!state.request(request(31.0, 1.0)));
        assert_eq!(state.queued, Some(request(31.0, 1.0)));
        // The UI follows the pointer, not the seek still on its way.
        assert_eq!(state.pending(), Some(request(31.0, 1.0)));
    }

    #[test]
    fn speed_change_during_a_seek_is_coalesced_with_the_latest_position() {
        let mut state = SeekState::default();
        issued(&mut state, request(12.0, 1.0), std::time::Duration::ZERO);
        assert!(!state.request(request(12.0, 1.5)));
        // A later scrub keeps the requested rate while replacing only the
        // pending position.
        assert!(!state.request(request(31.0, 1.5)));
        assert_eq!(state.queued, Some(request(31.0, 1.5)));
    }

    #[test]
    fn focused_playback_trace_attaches_prerolls_seeks_and_resumes() {
        let mut playback = FocusedPlayback::default();
        playback.start_video(std::path::Path::new("movie.mkv"), None, true);
        playback.prepare_subtitle_rebuild(42.5, 1.5, true, Some("movie.srt".into()));
        assert_eq!(
            playback.external_subtitle(),
            Some(std::path::Path::new("movie.srt"))
        );

        let seek = request(42.5, 1.5);
        assert_eq!(
            playback.observe_async_done(),
            ResumeAction::Seek {
                position: 42.5,
                rate: 1.5,
                resume_playing: true,
            }
        );
        playback.begin_seek(seek);
        playback.accept_seek(seek);
        assert_eq!(
            playback.observe_async_done(),
            ResumeAction::Finish {
                resume_playing: true,
            }
        );
        assert!(playback.resume.is_none());
        assert_eq!(playback.playback_rate, 1.5);
    }

    #[test]
    fn focused_playback_trace_marks_playing_only_after_pipeline_setup() {
        let mut playback = FocusedPlayback::default();
        playback.start_video(std::path::Path::new("movie.mkv"), None, true);
        assert!(!playback.playing);

        playback.playback_started();
        assert!(playback.playing);
    }

    #[test]
    fn focused_playback_trace_coalesces_queued_seeks_and_speed_changes() {
        let mut playback = FocusedPlayback::default();
        playback.start_video(std::path::Path::new("movie.mkv"), None, true);

        let first = request(12.0, 1.0);
        assert!(playback.request_seek(first));
        playback.begin_seek(first);
        playback.accept_seek(first);
        assert!(!playback.request_seek(request(12.0, 1.5)));
        assert!(!playback.request_seek(request(31.0, 1.5)));
        assert_eq!(playback.requested_rate(), 1.5);

        let queued = playback.finish_seek().unwrap();
        assert_eq!(queued, request(31.0, 1.5));
        playback.begin_seek(queued);
        playback.accept_seek(queued);
        assert_eq!(playback.finish_seek(), None);
        assert_eq!(playback.playback_rate, 1.5);
    }

    #[test]
    fn focused_playback_trace_keeps_the_accepted_rate_when_a_seek_is_refused() {
        let mut playback = FocusedPlayback::default();
        let rate_change = request(12.0, 1.5);
        playback.begin_seek(rate_change);
        playback.seek_refused();

        assert_eq!(playback.playback_rate, 1.0);
        assert_eq!(playback.seek.pending(), None);
    }

    #[test]
    fn focused_playback_trace_rejects_stale_errors_for_a_reopened_path() {
        let mut playback = FocusedPlayback::default();
        let video = std::path::Path::new("movie.mkv");
        playback.start_video(video, Some("movie.srt".into()), true);
        let stale = playback.begin_error().unwrap();

        // Generation identity matters because path and sidecar can be equal
        // after navigation or an explicit reopen.
        playback.start_video(video, Some("movie.srt".into()), true);
        assert!(!playback.error_is_current(&stale));
        let current = playback.begin_error().unwrap();
        playback.prepare_subtitle_rebuild(0.0, 1.0, true, None);
        assert!(!playback.error_is_current(&current));
        playback.finish_error(&stale);
        assert!(playback.begin_error().is_none());
        playback.finish_error(&current);
        assert!(playback.begin_error().is_some());
    }

    #[test]
    fn focused_playback_trace_resets_stream_choices_before_recovery_collection() {
        gst::init().unwrap();
        fn stream(id: &str, kind: gst::StreamType) -> gst::Stream {
            gst::Stream::new(Some(id), None, kind, gst::StreamFlags::SELECT)
        }
        fn collection(video: &str, audio: &str, subtitle: Option<&str>) -> gst::StreamCollection {
            let mut streams = vec![
                stream(video, gst::StreamType::VIDEO),
                stream(audio, gst::StreamType::AUDIO),
            ];
            if let Some(subtitle) = subtitle {
                streams.push(stream(subtitle, gst::StreamType::TEXT));
            }
            gst::StreamCollection::builder(None)
                .streams(streams)
                .build()
        }
        let mut playback = FocusedPlayback::default();
        playback.start_video(std::path::Path::new("movie.mkv"), None, false);

        let initial =
            playback.observe_stream_collection(collection("video-1", "main", Some("english")));
        assert!(initial.selection.is_some());
        assert_eq!(initial.subtitles.choice, SubtitleChoice::Off);
        let (_, initial_subtitles) = playback.observe_streams_selected(
            ["video-1".to_string(), "main".to_string()]
                .into_iter()
                .collect(),
        );
        assert_eq!(initial_subtitles.choice, SubtitleChoice::Off);

        playback.prepare_subtitle_rebuild(12.0, 1.0, true, Some("movie.srt".into()));
        let replacement =
            playback.observe_stream_collection(collection("video-2", "main", Some("external")));
        assert!(replacement.selection.is_none());
        assert_eq!(replacement.subtitles.choice, SubtitleChoice::Automatic);
        let (_, replacement_subtitles) = playback.observe_streams_selected(
            [
                "video-2".to_string(),
                "main".to_string(),
                "external".to_string(),
            ]
            .into_iter()
            .collect(),
        );
        assert_eq!(
            replacement_subtitles.active_label.as_deref(),
            Some("External — movie.srt")
        );
        assert_eq!(
            playback.external_subtitle(),
            Some(std::path::Path::new("movie.srt"))
        );
    }

    #[test]
    fn stream_selection_preserves_chosen_audio_video_and_subtitle() {
        gst::init().unwrap();
        let stream = |id, kind, flags| gst::Stream::new(Some(id), None, kind, flags);
        let mut playback = FocusedPlayback {
            collection: Some(
                gst::StreamCollection::builder(None)
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
                        stream("english", gst::StreamType::TEXT, gst::StreamFlags::SELECT),
                        stream("hindi", gst::StreamType::TEXT, gst::StreamFlags::empty()),
                    ])
                    .build(),
            ),
            audio_choice: AudioChoice::Track("commentary".into()),
            subtitle_choice: SubtitleChoice::Track("hindi".into()),
            ..FocusedPlayback::default()
        };
        refresh_stream_tracks(&mut playback);

        assert_eq!(
            stream_selection_ids(&playback, None, None),
            ["video", "commentary", "hindi"]
        );
    }

    #[test]
    fn subtitle_change_preserves_the_active_automatic_audio_stream() {
        gst::init().unwrap();
        let stream = |id, kind, flags| gst::Stream::new(Some(id), None, kind, flags);
        let mut playback = FocusedPlayback {
            collection: Some(
                gst::StreamCollection::builder(None)
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
                    .build(),
            ),
            ..FocusedPlayback::default()
        };
        refresh_stream_tracks(&mut playback);
        playback.selected.extend([
            "video".to_string(),
            "commentary".to_string(),
            "hindi-text".to_string(),
        ]);

        assert_eq!(
            stream_selection_ids(
                &playback,
                None,
                Some(&SubtitleChoice::Track("english-text".into())),
            ),
            ["video", "commentary", "english-text"]
        );
        assert_eq!(
            stream_selection_ids(&playback, Some(&AudioChoice::Automatic), None),
            ["video", "english-audio", "hindi-text"]
        );
    }

    #[test]
    fn collection_changes_retain_valid_choices_and_reset_missing_choices() {
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
        let mut playback = FocusedPlayback {
            audio_choice: AudioChoice::Track("commentary".into()),
            ..FocusedPlayback::default()
        };

        replace_stream_collection(&mut playback, collection(&["english", "commentary"]));
        assert_eq!(
            playback.audio_choice,
            AudioChoice::Track("commentary".into())
        );

        replace_stream_collection(&mut playback, collection(&["english", "descriptive"]));
        assert_eq!(playback.audio_choice, AudioChoice::Automatic);
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
        let mut playback = FocusedPlayback {
            collection: Some(
                gst::StreamCollection::builder(None)
                    .streams([titled, language, untagged])
                    .build(),
            ),
            ..FocusedPlayback::default()
        };
        refresh_stream_tracks(&mut playback);

        assert_eq!(
            playback
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
        let mut playback = FocusedPlayback {
            subtitle_tracks: vec![SubtitleTrack {
                id: "hindi".into(),
                label: "Hindi".into(),
            }],
            subtitle_choice: SubtitleChoice::Track("hindi".into()),
            last_visible_subtitle_choice: SubtitleChoice::Track("hindi".into()),
            ..FocusedPlayback::default()
        };

        assert_eq!(toggled_subtitle_choice(&playback), SubtitleChoice::Off);
        playback.subtitle_choice = SubtitleChoice::Off;
        assert_eq!(
            toggled_subtitle_choice(&playback),
            SubtitleChoice::Track("hindi".into())
        );

        playback.subtitle_tracks.clear();
        assert_eq!(
            toggled_subtitle_choice(&playback),
            SubtitleChoice::Automatic
        );
    }

    #[test]
    fn rapid_subtitle_cycles_follow_the_requested_track_not_stale_bus_state() {
        let mut playback = FocusedPlayback {
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
            ..FocusedPlayback::default()
        };
        playback.selected.insert("english".into());

        assert_eq!(cycled_subtitle_choice(&playback), SubtitleChoice::Off);
        playback.subtitle_choice = SubtitleChoice::Off;
        assert_eq!(
            cycled_subtitle_choice(&playback),
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
        let mut playback = FocusedPlayback {
            collection: Some(
                gst::StreamCollection::builder(None)
                    .streams([text("embedded"), text("external")])
                    .build(),
            ),
            external: Some(std::path::PathBuf::from("movie.en.srt")),
            ..FocusedPlayback::default()
        };

        refresh_stream_tracks(&mut playback);
        assert_eq!(playback.subtitle_tracks[0].label, "Subtitle 1");
        assert_eq!(playback.subtitle_tracks[1].label, "Subtitle 2");

        playback.collection = Some(
            gst::StreamCollection::builder(None)
                .streams([text("external")])
                .build(),
        );
        refresh_stream_tracks(&mut playback);
        assert_eq!(playback.subtitle_tracks[0].label, "External — movie.en.srt");
    }

    #[test]
    fn a_lost_seek_stops_blocking_and_stops_being_reported() {
        let mut state = SeekState::default();
        issued(&mut state, request(12.0, 1.0), SEEK_SETTLE);
        assert_eq!(state.pending(), None);
        assert!(state.request(request(20.0, 1.0)));
    }
}
