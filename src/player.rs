//! Video playback (FR-10.1): one `playbin3` pipeline rendering into a
//! `gtk4paintablesink`, whose `GdkPaintable` the viewer displays like
//! any other paintable.
//!
//! This is the resource-minimal path on this machine: preferred Intel QSV
//! decoders use the iGPU and frames reach GTK as dmabufs — no CPU pixel
//! copies. GStreamer is initialized lazily on the first video so
//! image-only sessions keep their cold-start and footprint (NFR-1.1,
//! NFR-2.1). The pipeline is reused across videos; `stop` drops it to
//! `Null`, freeing decoder state while an image is shown.
//!
//! `FocusedPlayback` owns the temporal state shared by typed commands and
//! GStreamer bus observations. `Player` remains the window-facing adapter and
//! keeps pipeline effects, lazy initialization and paintable ownership here.

use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use gstreamer as gst;
use gstreamer::prelude::*;

use gtk4::gdk;
use gtk4::glib;

use crate::config;

/// Seeks land on the exact target, not on the nearest keyframe. Keyframe
/// seeks are cheaper, but short clips are routinely encoded as a single
/// GOP — every seek then snaps back to 0:00 and the video looks stuck at
/// the start. Measured on this machine, an accurate seek costs 2–455 ms,
/// and at most one is ever in flight (see `SeekState`).
const SEEK_FLAGS: gst::SeekFlags = gst::SeekFlags::FLUSH.union(gst::SeekFlags::ACCURATE);
const VOLUME_MAX: f64 = 1.5;
pub const PLAYBACK_RATES: &[f64] = &[0.5, 0.75, 1.0, 1.25, 1.5, 2.0];
/// Hardware decoders proven on the target machine. GStreamer's libav
/// decoders rank at `Primary`, while VA-API and NVDEC can rank at
/// `Primary + 1` and these QSV factories normally rank lower. Prefer QSV
/// for streams its caps accept and leave other installed decoders as the
/// automatic fallback for streams the iGPU cannot decode (FR-10.1).
const INTEL_VIDEO_DECODERS: &[(&str, &str)] = &[
    ("qsvh264dec", "avdec_h264"),
    ("qsvh265dec", "avdec_h265"),
    ("qsvvp9dec", "avdec_vp9"),
    ("qsvjpegdec", "avdec_mjpeg"),
];
/// Safety net: if a seek never gets its `AsyncDone` (broken file, stalled
/// demuxer), stop reporting its target and fall back to real queries.
const SEEK_SETTLE: Duration = Duration::from_millis(1500);

/// Pipeline happenings the window reacts to; delivered on the main loop.
pub enum Event {
    EndOfStream,
    Error(glib::Error),
    MissingVideoDecoder(String),
    SubtitleError(String),
    AudioChanged(AudioSnapshot),
    SubtitlesChanged(SubtitleSnapshot),
    PlaybackRateError(PlaybackRateError),
}

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

#[derive(Debug)]
pub enum PlayerError {
    Init(glib::Error),
    SinkUnavailable(glib::BoolError),
    PlaybinUnavailable(glib::BoolError),
    MissingBus,
    BusWatch(glib::BoolError),
    Uri {
        path: PathBuf,
        source: glib::Error,
    },
    Playback {
        path: PathBuf,
        source: gst::StateChangeError,
    },
    SubtitleFile {
        path: PathBuf,
        source: io::Error,
    },
}

impl fmt::Display for PlayerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PlayerError::Init(source) => write!(f, "GStreamer init failed: {source}"),
            PlayerError::SinkUnavailable(source) => {
                write!(f, "gtk4paintablesink unavailable: {source}")
            }
            PlayerError::PlaybinUnavailable(source) => {
                write!(f, "playbin3 unavailable: {source}")
            }
            PlayerError::MissingBus => f.write_str("playbin has no bus"),
            PlayerError::BusWatch(source) => write!(f, "cannot watch pipeline bus: {source}"),
            PlayerError::Uri { path, source } => {
                write!(f, "cannot build uri for {}: {source}", path.display())
            }
            PlayerError::Playback { path, source } => {
                write!(f, "cannot start playback of {}: {source}", path.display())
            }
            PlayerError::SubtitleFile { path, source } => {
                write!(f, "cannot read subtitle {}: {source}", path.display())
            }
        }
    }
}

impl std::error::Error for PlayerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            PlayerError::Init(source) | PlayerError::Uri { source, .. } => Some(source),
            PlayerError::SinkUnavailable(source)
            | PlayerError::PlaybinUnavailable(source)
            | PlayerError::BusWatch(source) => Some(source),
            PlayerError::Playback { source, .. } => Some(source),
            PlayerError::SubtitleFile { source, .. } => Some(source),
            PlayerError::MissingBus => None,
        }
    }
}

#[derive(Default)]
struct StreamState {
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

#[derive(Clone, Copy, Debug, PartialEq)]
enum ResumeStage {
    Preroll,
    Seek,
}

#[derive(Debug, PartialEq)]
enum ResumeAction {
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
struct SeekRequest {
    position: f64,
    rate: f64,
}

#[derive(Default)]
struct SeekState {
    in_flight: Option<(SeekRequest, Instant)>,
    queued: Option<SeekRequest>,
}

/// All temporal facts for the active Focused playback session. Commands and
/// GStreamer observations both enter through this model, so sequencing does
/// not depend on keeping several independently borrowed cells in sync.
struct FocusedPlayback {
    generation: u64,
    current_video: Option<PathBuf>,
    playing: bool,
    /// Last rate accepted by the pipeline. A queued seek may advertise a
    /// newer requested rate without changing this until GStreamer accepts it.
    playback_rate: f64,
    /// Cached so transport updates do not query the demuxer every frame.
    duration: Option<f64>,
    seek: SeekState,
    streams: StreamState,
    resume: Option<ResumeState>,
    error_pending: bool,
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
            streams: StreamState::default(),
            resume: None,
            error_pending: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ErrorContext {
    generation: u64,
    external: Option<PathBuf>,
}

impl FocusedPlayback {
    fn reset(&mut self, subtitles_default_on: bool) {
        let generation = self.generation.wrapping_add(1);
        *self = Self {
            generation,
            streams: StreamState {
                subtitle_choice: if subtitles_default_on {
                    SubtitleChoice::Automatic
                } else {
                    SubtitleChoice::Off
                },
                ..StreamState::default()
            },
            ..Self::default()
        };
    }

    fn start_video(&mut self, path: &Path, external: Option<PathBuf>, subtitles_default_on: bool) {
        self.reset(subtitles_default_on);
        self.current_video = Some(path.to_path_buf());
        self.streams.external = external;
    }

    fn playback_started(&mut self) {
        self.playing = true;
    }

    fn forget_timing(&mut self) {
        self.seek = SeekState::default();
        self.playback_rate = 1.0;
        self.duration = None;
    }

    fn requested_rate(&self) -> f64 {
        self.seek
            .pending()
            .map_or(self.playback_rate, |request| request.rate)
    }

    fn request_seek(&mut self, request: SeekRequest) -> bool {
        self.seek.request(request)
    }

    fn begin_seek(&mut self, request: SeekRequest) {
        self.seek.in_flight = Some((request, Instant::now()));
        self.seek.queued = None;
    }

    fn accept_seek(&mut self, request: SeekRequest) {
        self.playback_rate = request.rate;
    }

    fn seek_refused(&mut self) {
        self.seek.in_flight = None;
    }

    fn prepare_subtitle_rebuild(
        &mut self,
        external: Option<PathBuf>,
        position: f64,
        rate: f64,
        play_after_seek: bool,
    ) {
        self.forget_timing();
        self.streams.collection = None;
        self.streams.selected.clear();
        self.streams.audio_tracks.clear();
        self.streams.subtitle_tracks.clear();
        self.streams.subtitle_choice = SubtitleChoice::Automatic;
        self.streams.last_visible_subtitle_choice = SubtitleChoice::Automatic;
        self.streams.external = external;
        self.resume = Some(ResumeState {
            position,
            rate,
            play_after_seek,
            stage: ResumeStage::Preroll,
        });
    }

    fn observe_async_done(&mut self) -> ResumeAction {
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

    fn finish_seek(&mut self) -> Option<SeekRequest> {
        self.seek.in_flight = None;
        self.seek.queued.take()
    }

    fn replace_streams(&mut self, collection: gst::StreamCollection) -> bool {
        replace_stream_collection(&mut self.streams, collection);
        self.streams.audio_choice != AudioChoice::Automatic
            || self.streams.subtitle_choice != SubtitleChoice::Automatic
    }

    fn select_streams(&mut self, selected: BTreeSet<String>) {
        self.streams.selected = selected;
    }

    fn stream_snapshots(&self) -> (AudioSnapshot, SubtitleSnapshot) {
        (
            audio_snapshot(&self.streams),
            subtitle_snapshot(&self.streams),
        )
    }

    fn requested_streams(
        &self,
        audio_request: Option<&AudioChoice>,
        subtitle_request: Option<&SubtitleChoice>,
    ) -> Vec<String> {
        stream_selection_ids(&self.streams, audio_request, subtitle_request)
    }

    fn begin_error(&mut self) -> Option<ErrorContext> {
        if self.error_pending {
            return None;
        }
        self.error_pending = true;
        Some(self.context())
    }

    fn context(&self) -> ErrorContext {
        ErrorContext {
            generation: self.generation,
            external: self.streams.external.clone(),
        }
    }

    fn error_is_current(&self, context: &ErrorContext) -> bool {
        self.generation == context.generation && self.streams.external == context.external
    }

    fn finish_error(&mut self, context: &ErrorContext) {
        if self.generation == context.generation {
            self.error_pending = false;
        }
    }
}

/// True when the coded frame exceeds the size limit the stream itself
/// declares. Unknown levels stay on the normal hardware
/// path; this guard only acts on a demonstrable metadata contradiction.
fn h264_exceeds_declared_level(caps: &gst::CapsRef) -> bool {
    let Some(structure) = caps.iter().find(|s| s.name() == "video/x-h264") else {
        return false;
    };
    let Ok(level) = structure.get::<String>("level") else {
        return false;
    };
    let Some(max_frame_mbs) = h264_level_max_frame_mbs(&level) else {
        return false;
    };
    let (Ok(width), Ok(height)) = (
        structure.get::<i32>("width"),
        structure.get::<i32>("height"),
    ) else {
        return false;
    };
    if width <= 0 || height <= 0 {
        return false;
    }

    let (Ok(width), Ok(height)) = (u64::try_from(width), u64::try_from(height)) else {
        return false;
    };
    let frame_mbs = width.div_ceil(16) * height.div_ceil(16);
    frame_mbs > max_frame_mbs
}

/// Compact encoded-stream facts for diagnostics. Raw caps dumps are noisy and
/// unstable across GStreamer versions; these fields explain the decoder choice
/// and the common hardware-limit failures without logging from the render path.
fn video_stream_summary(caps: &gst::CapsRef) -> Option<String> {
    let structure = caps.structure(0)?;
    let mut summary = structure.name().to_string();

    for field in ["profile", "level"] {
        if let Ok(value) = structure.get::<String>(field) {
            summary.push_str(&format!(" {field}={value}"));
        }
    }
    if let (Ok(width), Ok(height)) = (
        structure.get::<i32>("width"),
        structure.get::<i32>("height"),
    ) {
        summary.push_str(&format!(" {width}x{height}"));
    }
    if let Ok(rate) = structure.get::<gst::Fraction>("framerate") {
        summary.push_str(&format!(" {}/{} fps", rate.numer(), rate.denom()));
    }

    Some(summary)
}

/// H.264 Annex A `MaxFS` in macroblocks.
fn h264_level_max_frame_mbs(level: &str) -> Option<u64> {
    Some(match level {
        "1" | "1b" => 99,
        "1.1" | "1.2" | "1.3" | "2" => 396,
        "2.1" => 792,
        "2.2" | "3" => 1_620,
        "3.1" => 3_600,
        "3.2" => 5_120,
        "4" | "4.1" => 8_192,
        "4.2" => 8_704,
        "5" => 22_080,
        "5.1" | "5.2" => 36_864,
        "6" | "6.1" | "6.2" => 139_264,
        _ => return None,
    })
}

/// The stream collection is posted synchronously before `decodebin3`
/// chooses a decoder. Temporarily lowering QSV's rank here ensures its
/// cached candidate list is built without QSV for only this malformed
/// stream. Restore the process-wide rank as soon as libav is constructed.
#[derive(Default)]
struct DecoderFallback {
    disabled_rank: Option<gst::Rank>,
}

impl DecoderFallback {
    fn bypass_qsv_h264(&mut self, caps: &gst::CapsRef) -> Option<&'static str> {
        if self.disabled_rank.is_some() || !h264_exceeds_declared_level(caps) {
            return None;
        }
        let fallback = gst::ElementFactory::find("avdec_h264")?;
        if fallback.rank() == gst::Rank::NONE {
            return None;
        }
        let factory = gst::ElementFactory::find("qsvh264dec")?;
        let rank = factory.rank();
        if rank == gst::Rank::NONE {
            return None;
        }
        factory.set_rank(gst::Rank::NONE);
        self.disabled_rank = Some(rank);
        Some("avdec_h264")
    }

    fn restore(&mut self) {
        let Some(rank) = self.disabled_rank.take() else {
            return;
        };
        if let Some(factory) = gst::ElementFactory::find("qsvh264dec") {
            factory.set_rank(rank);
        }
    }
}

fn lock_decoder_fallback(state: &Mutex<DecoderFallback>) -> MutexGuard<'_, DecoderFallback> {
    state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl SeekState {
    /// Where playback is headed, while it is still on its way there.
    fn pending(&self) -> Option<SeekRequest> {
        self.queued
            .or_else(|| self.running().map(|(request, _)| request))
    }

    /// The in-flight seek, unless it is old enough to count as lost.
    fn running(&self) -> Option<(SeekRequest, Instant)> {
        self.in_flight.filter(|(_, at)| at.elapsed() < SEEK_SETTLE)
    }

    /// Record a request. Returns true when the caller
    /// must issue it, false when the running seek will pick it up.
    fn request(&mut self, request: SeekRequest) -> bool {
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

pub struct Player {
    playbin: gst::Element,
    seek_target: gst::Element,
    paintable: gdk::Paintable,
    playback: Rc<RefCell<FocusedPlayback>>,
    pitch_preserving: bool,
    subtitles_default_on: Cell<bool>,
    decoder_fallback: Arc<Mutex<DecoderFallback>>,
    /// Keeps the bus watch alive; dropping it detaches the watch.
    _bus_watch: gst::bus::BusWatchGuard,
}

impl Player {
    /// Build the pipeline. `on_event` fires on the GTK main loop.
    pub fn new(on_event: impl Fn(Event) + 'static) -> Result<Player, PlayerError> {
        gst::init().map_err(PlayerError::Init)?;
        prefer_intel_video_decoders();
        let on_event: Rc<dyn Fn(Event)> = Rc::new(on_event);
        let sink = gst::ElementFactory::make("gtk4paintablesink")
            .build()
            .map_err(PlayerError::SinkUnavailable)?;
        // The sink's paintable must be pulled from the main thread.
        let paintable = sink.property::<gdk::Paintable>("paintable");
        let playbin = gst::ElementFactory::make("playbin3")
            .property("video-sink", &sink)
            .build()
            .map_err(PlayerError::PlaybinUnavailable)?;
        // Player construction is already the lazy GStreamer boundary. Keep
        // image-only startup untouched, and make non-1x playback unavailable
        // rather than changing voice pitch when the Fedora Good plug-ins are
        // missing.
        let pitch_preserving = match gst::ElementFactory::make("scaletempo").build() {
            Ok(filter) => {
                playbin.set_property("audio-filter", &filter);
                crate::applog!("player: pitch-preserving scaletempo enabled");
                true
            }
            Err(error) => {
                crate::applog!("player: scaletempo unavailable: {error}");
                false
            }
        };

        let playback = Rc::new(RefCell::new(FocusedPlayback::default()));
        let decoder_fallback = Arc::new(Mutex::new(DecoderFallback::default()));
        playbin.connect_closure(
            "element-setup",
            false,
            glib::closure!(
                #[strong]
                decoder_fallback,
                move |_playbin: gst::Element, element: gst::Element| {
                    let Some(factory) = element.factory() else {
                        return;
                    };
                    if factory.name() == "avdec_h264" {
                        lock_decoder_fallback(&decoder_fallback).restore();
                    }
                    if factory.has_type(
                        gst::ElementFactoryType::DECODER | gst::ElementFactoryType::MEDIA_VIDEO,
                    ) {
                        let kind = if factory.has_type(gst::ElementFactoryType::HARDWARE) {
                            "hardware"
                        } else {
                            "software"
                        };
                        crate::applog!(
                            "player: selected decoder {}: {} ({kind}, rank {})",
                            factory.name(),
                            factory.longname(),
                            factory.rank()
                        );
                    }
                }
            ),
        );

        let bus = playbin.bus().ok_or(PlayerError::MissingBus)?;
        bus.set_sync_handler({
            let decoder_fallback = decoder_fallback.clone();
            move |_bus, msg| {
                if let gst::MessageView::StreamCollection(streams) = msg.view() {
                    let collection = streams.stream_collection();
                    for index in 0..collection.size() {
                        let Some(stream) = collection.stream(index) else {
                            continue;
                        };
                        if !stream.stream_type().contains(gst::StreamType::VIDEO) {
                            continue;
                        }
                        let Some(caps) = stream.caps() else {
                            continue;
                        };
                        if let Some(summary) = video_stream_summary(&caps) {
                            crate::applog!("player: video stream {summary}");
                        }
                        if let Some(fallback) =
                            lock_decoder_fallback(&decoder_fallback).bypass_qsv_h264(&caps)
                        {
                            crate::applog!(
                                "player: stream exceeds its H.264 level; bypassing qsvh264dec for {fallback}"
                            );
                            break;
                        }
                    }
                }
                gst::BusSyncReply::Pass
            }
        });
        let bus_watch = bus
            .add_watch_local({
                // The pipeline holds the bus, which holds this closure —
                // the guard below detaches the watch on drop, so the
                // cycle ends with the `Player`.
                let playbin = playbin.clone();
                let seek_target = sink.clone();
                let playback = playback.clone();
                let decoder_fallback = decoder_fallback.clone();
                let on_event = on_event.clone();
                move |_bus, msg| {
                    match msg.view() {
                        gst::MessageView::Eos(_) => on_event(Event::EndOfStream),
                        gst::MessageView::Error(e) => {
                            crate::applog!(
                                "player: error from {:?}: {} ({:?})",
                                e.src().map(|s| s.path_string()),
                                e.error(),
                                e.debug()
                            );
                            lock_decoder_fallback(&decoder_fallback).restore();
                            let Some(error_context) = playback.borrow_mut().begin_error() else {
                                crate::applog!(
                                    "player: ignoring error queued behind pending recovery"
                                );
                                return glib::ControlFlow::Continue;
                            };
                            let error = e.error();
                            let playbin = playbin.clone();
                            let playback = playback.clone();
                            let on_event = on_event.clone();
                            // Returning from the bus watch before changing
                            // state is mandatory: tearing a failed pipeline
                            // down from inside its Error callback can wait on
                            // the streaming thread that posted this message.
                            glib::idle_add_local_once(move || {
                                let still_current =
                                    playback.borrow().error_is_current(&error_context);
                                if !still_current {
                                    crate::applog!("player: stale pipeline error superseded");
                                    playback.borrow_mut().finish_error(&error_context);
                                    return;
                                }
                                let recovered = recover_without_external(
                                    &playbin,
                                    &playback,
                                );
                                playback.borrow_mut().finish_error(&error_context);
                                if recovered {
                                    on_event(Event::SubtitleError(error.to_string()));
                                } else {
                                    on_event(Event::Error(error));
                                }
                            });
                        }
                        gst::MessageView::Element(e) => {
                            let structure = e.structure();
                            if let Some(description) = structure.and_then(missing_video_decoder) {
                                crate::applog!("player: missing video decoder: {description}");
                                let context = playback.borrow().context();
                                let playback = playback.clone();
                                let on_event = on_event.clone();
                                glib::idle_add_local_once(move || {
                                    if playback.borrow().error_is_current(&context) {
                                        on_event(Event::MissingVideoDecoder(description));
                                    }
                                });
                            } else if let Some(description) =
                                structure.and_then(missing_subtitle_decoder)
                            {
                                crate::applog!("player: missing subtitle decoder: {description}");
                                on_event(Event::SubtitleError(format!(
                                    "subtitle decoder unavailable: {description}"
                                )));
                            }
                        }
                        gst::MessageView::AsyncDone(_) => {
                            // Replacing an external sidecar rebuilds the
                            // same URI, then restores position and
                            // the former playback state without blocking the
                            // GTK main loop (FR-10.7).
                            let resume_action = playback.borrow_mut().observe_async_done();
                            match resume_action {
                                ResumeAction::Seek {
                                    position,
                                    rate,
                                    resume_playing,
                                } => {
                                    let needs_seek = position > f64::EPSILON
                                        || !same_rate(rate, playback.borrow().playback_rate);
                                    if !needs_seek
                                        || !issue_seek(
                                            &seek_target,
                                            &playback,
                                            SeekRequest { position, rate },
                                        )
                                    {
                                        if needs_seek
                                            && !same_rate(rate, playback.borrow().playback_rate)
                                        {
                                            on_event(Event::PlaybackRateError(
                                                PlaybackRateError::SeekRefused,
                                            ));
                                        }
                                        playback.borrow_mut().resume = None;
                                        let target = if resume_playing {
                                            gst::State::Playing
                                        } else {
                                            gst::State::Paused
                                        };
                                        let _ = playbin.set_state(target);
                                        playback.borrow_mut().playing = resume_playing;
                                    }
                                    return glib::ControlFlow::Continue;
                                }
                                ResumeAction::Finish { resume_playing } => {
                                    let target = if resume_playing {
                                        gst::State::Playing
                                    } else {
                                        gst::State::Paused
                                    };
                                    let _ = playbin.set_state(target);
                                    playback.borrow_mut().playing = resume_playing;
                                }
                                ResumeAction::None => {}
                            }

                            // The seek landed: real positions are truthful
                            // again, and the newest scrub position that piled
                            // up behind it can go out now.
                            let next = {
                                playback.borrow_mut().finish_seek()
                            };
                            if let Some(request) = next {
                                let rate_change =
                                    !same_rate(request.rate, playback.borrow().playback_rate);
                                if !issue_seek(&seek_target, &playback, request)
                                    && rate_change
                                {
                                    on_event(Event::PlaybackRateError(
                                        PlaybackRateError::SeekRefused,
                                    ));
                                }
                            }
                        }
                        gst::MessageView::StreamCollection(streams) => {
                            let collection = streams.stream_collection();
                            let should_apply = {
                                playback.borrow_mut().replace_streams(collection)
                            };
                            let (audio_count, subtitle_count) = {
                                let state = playback.borrow();
                                (
                                    state.streams.audio_tracks.len(),
                                    state.streams.subtitle_tracks.len(),
                                )
                            };
                            crate::applog!(
                                "player: discovered {audio_count} audio and {subtitle_count} subtitle track(s)"
                            );
                            if should_apply {
                                apply_stream_choices(&playbin, &playback, None, None);
                            }
                            let (audio, subtitles) = {
                                playback.borrow().stream_snapshots()
                            };
                            on_event(Event::AudioChanged(audio));
                            on_event(Event::SubtitlesChanged(subtitles));
                        }
                        gst::MessageView::StreamsSelected(streams) => {
                            let selected = streams
                                .streams()
                                .filter_map(|stream| stream.stream_id().map(|id| id.to_string()))
                                .collect();
                            playback.borrow_mut().select_streams(selected);
                            let (audio, subtitles) = {
                                playback.borrow().stream_snapshots()
                            };
                            on_event(Event::AudioChanged(audio));
                            on_event(Event::SubtitlesChanged(subtitles));
                        }
                        gst::MessageView::DurationChanged(_) => playback.borrow_mut().duration = None,
                        _ => {}
                    }
                    glib::ControlFlow::Continue
                }
            })
            .map_err(PlayerError::BusWatch)?;

        Ok(Player {
            playbin,
            seek_target: sink,
            paintable,
            playback,
            pitch_preserving,
            subtitles_default_on: Cell::new(true),
            decoder_fallback,
            _bus_watch: bus_watch,
        })
    }

    pub fn paintable(&self) -> gdk::Paintable {
        self.paintable.clone()
    }

    pub fn has_external_subtitle(&self) -> bool {
        self.playback.borrow().streams.external.is_some()
    }

    pub fn path_has_sidecar(path: &Path) -> bool {
        matching_sidecar(path).is_some()
    }

    /// Start playing `path` from the beginning, replacing any current
    /// video. The pipeline object is reused; only its state cycles.
    pub fn play(&self, path: &Path) -> Result<(), PlayerError> {
        let subtitle = matching_sidecar(path);
        if let Some(subtitle) = subtitle.as_ref() {
            crate::applog!("player: matched subtitle {}", subtitle.display());
        }
        let uri = file_uri(path)?;
        let suburi = subtitle.as_deref().map(file_uri).transpose()?;
        let _ = teardown_pipeline(&self.playbin);
        lock_decoder_fallback(&self.decoder_fallback).restore();
        self.playback
            .borrow_mut()
            .start_video(path, subtitle, self.subtitles_default_on.get());
        configure_uris(&self.playbin, &uri, suburi.as_deref());
        self.playbin
            .set_state(gst::State::Playing)
            .map_err(|source| {
                lock_decoder_fallback(&self.decoder_fallback).restore();
                PlayerError::Playback {
                    path: path.to_path_buf(),
                    source,
                }
            })?;
        self.playback.borrow_mut().playback_started();
        Ok(())
    }

    /// Drop to `Null`: stops playback and frees decoder state.
    pub fn stop(&self) {
        let (_, current, _) = self.playbin.state(gst::ClockTime::ZERO);
        let _ = teardown_pipeline(&self.playbin);
        self.forget_stream();
        if current != gst::State::Null {
            crate::applog!("player: stopped, pipeline released");
        }
    }

    /// Drop everything that describes the outgoing stream so the next
    /// video never reports the previous one's duration or seek target.
    fn forget_stream(&self) {
        lock_decoder_fallback(&self.decoder_fallback).restore();
        self.playback
            .borrow_mut()
            .reset(self.subtitles_default_on.get());
    }

    /// Set the initial subtitle policy applied independently to every
    /// newly opened video (FR-8.2/10.7).
    pub fn set_subtitles_default(&self, enabled: bool) {
        self.subtitles_default_on.set(enabled);
        if self.playback.borrow().current_video.is_none() {
            self.playback.borrow_mut().streams.subtitle_choice = if enabled {
                SubtitleChoice::Automatic
            } else {
                SubtitleChoice::Off
            };
        }
    }

    /// Attach a local SRT/WebVTT file to the current video. `playbin3`
    /// consumes one external `suburi`, so a later drop replaces it. The
    /// pipeline is re-prerolled asynchronously and resumes at the same
    /// position and play/pause state (FR-10.7).
    pub fn attach_subtitle(&self, path: &Path) -> Result<(), PlayerError> {
        fs::File::open(path).map_err(|source| PlayerError::SubtitleFile {
            path: path.to_path_buf(),
            source,
        })?;
        let video = self
            .playback
            .borrow()
            .current_video
            .clone()
            .ok_or_else(|| PlayerError::SubtitleFile {
                path: path.to_path_buf(),
                source: io::Error::new(io::ErrorKind::InvalidInput, "no video is playing"),
            })?;
        let already_attached = self.playback.borrow().streams.external.as_deref() == Some(path);
        if already_attached {
            crate::applog!(
                "player: subtitle {} already attached; selecting existing track",
                path.display()
            );
            if !self.choose_subtitle(SubtitleChoice::Automatic) {
                let mut state = self.playback.borrow_mut();
                state.streams.subtitle_choice = SubtitleChoice::Automatic;
                state.streams.last_visible_subtitle_choice = SubtitleChoice::Automatic;
            }
            return Ok(());
        }
        let uri = file_uri(&video)?;
        let suburi = file_uri(path)?;
        let position = self.progress().map_or_else(
            || {
                self.playbin
                    .query_position::<gst::ClockTime>()
                    .map_or(0.0, gst::ClockTime::seconds_f64)
            },
            |(position, _)| position,
        );
        let play_after_seek = self.is_playing();
        let rate = self.playback_rate();

        crate::applog!(
            "player: replacing external subtitle at {position:.1}s ({})",
            if play_after_seek { "playing" } else { "paused" }
        );
        // A full teardown matters here. READY can retain the old playsink
        // pads, so the new text pad may reach it before the replacement
        // video pad and fail with "Have text pad but no video pad".
        teardown_pipeline(&self.playbin).map_err(|source| {
            lock_decoder_fallback(&self.decoder_fallback).restore();
            PlayerError::Playback {
                path: video.clone(),
                source,
            }
        })?;
        crate::applog!("player: subtitle rebuild reached null");
        lock_decoder_fallback(&self.decoder_fallback).restore();
        self.playback.borrow_mut().prepare_subtitle_rebuild(
            Some(path.to_path_buf()),
            position,
            rate,
            play_after_seek,
        );
        configure_uris(&self.playbin, &uri, Some(&suburi));
        crate::applog!("player: attached subtitle {}", path.display());
        if let Err(source) = self.playbin.set_state(gst::State::Playing) {
            self.playback.borrow_mut().resume = None;
            let _ = recover_without_external(&self.playbin, &self.playback);
            return Err(PlayerError::Playback {
                path: video,
                source,
            });
        }
        Ok(())
    }

    pub fn subtitle_snapshot(&self) -> SubtitleSnapshot {
        subtitle_snapshot(&self.playback.borrow().streams)
    }

    pub fn audio_snapshot(&self) -> AudioSnapshot {
        audio_snapshot(&self.playback.borrow().streams)
    }

    pub fn choose_audio(&self, choice: AudioChoice) -> bool {
        if let AudioChoice::Track(id) = &choice
            && !self
                .playback
                .borrow()
                .streams
                .audio_tracks
                .iter()
                .any(|track| track.id == *id)
        {
            return false;
        }
        let sent = apply_stream_choices(&self.playbin, &self.playback, Some(&choice), None);
        if sent {
            crate::applog!("player: audio selection {}", choice.action_target());
            self.playback.borrow_mut().streams.audio_choice = choice;
        }
        sent
    }

    pub fn choose_subtitle(&self, choice: SubtitleChoice) -> bool {
        if let SubtitleChoice::Track(id) = &choice
            && !self
                .playback
                .borrow()
                .streams
                .subtitle_tracks
                .iter()
                .any(|track| track.id == *id)
        {
            return false;
        }
        let sent = apply_stream_choices(&self.playbin, &self.playback, None, Some(&choice));
        if sent {
            crate::applog!("player: subtitle selection {}", choice.action_target());
            let mut state = self.playback.borrow_mut();
            if choice != SubtitleChoice::Off {
                state.streams.last_visible_subtitle_choice = choice.clone();
            }
            state.streams.subtitle_choice = choice;
        }
        sent
    }

    pub fn toggle_subtitles(&self) -> SubtitleSnapshot {
        if self.playback.borrow().streams.subtitle_tracks.is_empty() {
            return self.subtitle_snapshot();
        }
        let choice = toggled_subtitle_choice(&self.playback.borrow().streams);
        self.choose_subtitle(choice);
        self.subtitle_snapshot()
    }

    pub fn cycle_subtitles(&self) -> SubtitleSnapshot {
        let state = self.playback.borrow();
        if state.streams.subtitle_tracks.is_empty() {
            return subtitle_snapshot(&state.streams);
        }
        let choice = cycled_subtitle_choice(&state.streams);
        drop(state);
        self.choose_subtitle(choice);
        self.subtitle_snapshot()
    }

    /// True when playback is running or the user has asked it to run.
    pub fn is_playing(&self) -> bool {
        self.playback.borrow().playing
    }

    pub fn is_muted(&self) -> bool {
        self.playbin.property::<bool>("mute")
    }

    /// Toggle pause; returns true when now playing.
    pub fn toggle_pause(&self) -> bool {
        if self.playback.borrow().playing {
            let _ = self.playbin.set_state(gst::State::Paused);
            self.playback.borrow_mut().playing = false;
            crate::applog!("player: paused");
            false
        } else {
            let _ = self.playbin.set_state(gst::State::Playing);
            self.playback.borrow_mut().playing = true;
            crate::applog!("player: playing");
            true
        }
    }

    /// Position and duration in seconds, once the pipeline knows them.
    /// While a seek is in flight the position is its target, so the UI
    /// tracks the user instead of the pipeline's catch-up.
    pub fn progress(&self) -> Option<(f64, f64)> {
        let dur = self.duration()?;
        let pos = match self.playback.borrow().seek.pending() {
            Some(request) => request.position,
            None => self
                .playbin
                .query_position::<gst::ClockTime>()?
                .seconds_f64(),
        };
        Some((pos.clamp(0.0, dur), dur))
    }

    /// Stream duration in seconds, cached once the demuxer reports it.
    fn duration(&self) -> Option<f64> {
        if let Some(dur) = self.playback.borrow().duration {
            return Some(dur);
        }
        let dur = self
            .playbin
            .query_duration::<gst::ClockTime>()?
            .seconds_f64();
        if dur <= 0.0 {
            return None;
        }
        self.playback.borrow_mut().duration = Some(dur);
        Some(dur)
    }

    /// Seek by `delta` seconds, clamped to the stream. Deltas stack: they
    /// are measured from the pending target, so hammering the seek key
    /// moves the full distance instead of repeating one 5-second jump.
    pub fn seek_by(&self, delta: f64) {
        let Some((pos, dur)) = self.progress() else {
            return;
        };
        self.seek_to((pos + delta).clamp(0.0, dur));
    }

    /// Seek to `fraction` (0..1) of the duration.
    pub fn seek_fraction(&self, fraction: f64) {
        let Some(dur) = self.duration() else {
            return;
        };
        self.seek_to(dur * fraction.clamp(0.0, 1.0));
    }

    /// Keeps at most one seek in flight; a request that arrives during
    /// one supersedes any other waiting request (see `SeekState`).
    fn seek_to(&self, secs: f64) {
        let request = SeekRequest {
            position: secs,
            rate: self.playback_rate(),
        };
        let issue_now = self.playback.borrow_mut().request_seek(request);
        if issue_now {
            issue_seek(&self.seek_target, &self.playback, request);
        }
    }

    /// The requested playback rate, including a coalesced change waiting
    /// behind an accurate seek.
    pub fn playback_rate(&self) -> f64 {
        self.playback.borrow().requested_rate()
    }

    /// Change playback rate without moving the visible position. This uses
    /// the same bounded flushing-seek queue as scrubbing. GStreamer 1.28's
    /// playbin3/scaletempo path accepts an instant-rate request but leaves its
    /// audio segment unable to handle the next time-format seek, so that path
    /// is not compatible with the required seeking and looping behavior.
    pub fn set_playback_rate(&self, rate: f64) -> Result<f64, PlaybackRateError> {
        if !PLAYBACK_RATES
            .iter()
            .any(|candidate| same_rate(*candidate, rate))
        {
            return Err(PlaybackRateError::SeekRefused);
        }
        if same_rate(rate, self.playback_rate()) {
            return Ok(rate);
        }
        if !self.pitch_preserving && !same_rate(rate, 1.0) {
            return Err(PlaybackRateError::PitchFilterUnavailable);
        }

        let position = self
            .progress()
            .map(|(position, _)| position)
            .or_else(|| {
                self.playbin
                    .query_position::<gst::ClockTime>()
                    .map(gst::ClockTime::seconds_f64)
            })
            .ok_or(PlaybackRateError::PositionUnavailable)?;
        let request = SeekRequest { position, rate };
        let issue_now = self.playback.borrow_mut().request_seek(request);
        if !issue_now {
            return Ok(rate);
        }

        if issue_seek(&self.seek_target, &self.playback, request) {
            Ok(rate)
        } else {
            Err(PlaybackRateError::SeekRefused)
        }
    }

    /// Restart from the beginning (EOS loop, FR-10.3).
    pub fn rewind(&self) {
        self.seek_to(0.0);
        let _ = self.playbin.set_state(gst::State::Playing);
        self.playback.borrow_mut().playing = true;
    }

    /// Set the starting volume from config (FR-8.2). The pipeline is
    /// reused across videos, so this only needs applying once.
    pub fn set_volume(&self, volume: f64) {
        let volume = volume.clamp(0.0, VOLUME_MAX);
        self.playbin.set_property("volume", volume);
        crate::applog!("player: volume {:.0}%", volume * 100.0);
    }

    /// Change volume by `delta`; returns the new volume (0..=1.5).
    pub fn add_volume(&self, delta: f64) -> f64 {
        let vol = (self.playbin.property::<f64>("volume") + delta).clamp(0.0, VOLUME_MAX);
        self.playbin.set_property("volume", vol);
        vol
    }

    /// Toggle mute; returns true when now muted.
    pub fn toggle_mute(&self) -> bool {
        let muted = !self.playbin.property::<bool>("mute");
        self.playbin.set_property("mute", muted);
        crate::applog!("player: mute {}", muted);
        muted
    }
}

fn file_uri(path: &Path) -> Result<String, PlayerError> {
    glib::filename_to_uri(path, None)
        .map(String::from)
        .map_err(|source| PlayerError::Uri {
            path: path.to_path_buf(),
            source,
        })
}

/// `uri` and `suburi` are "next media" properties on playbin3. Clear both
/// before reusing the same pipeline so replaying identical paths is still a
/// fresh pair rather than a stale subtitle source attached to new video pads.
fn configure_uris(playbin: &gst::Element, uri: &str, suburi: Option<&str>) {
    playbin.set_property("suburi", Option::<&str>::None);
    playbin.set_property("uri", uri);
    playbin.set_property("suburi", suburi);
}

/// Wait for the downward transition before reusing `uri`/`suburi`. Although
/// `set_state(Null)` usually completes synchronously, playbin3 can still be
/// removing its old text/video pads; immediately setting the same pair again
/// then intermittently connects text to playsink before video (FR-10.7).
fn teardown_pipeline(playbin: &gst::Element) -> Result<(), gst::StateChangeError> {
    // GstPipeline flushes its bus while entering Null, so ordinary bus
    // observations from the outgoing URI cannot cross this completed
    // boundary. The error callbacks explicitly deferred to GLib idle carry a
    // FocusedPlayback generation because they have already left the bus.
    playbin.set_state(gst::State::Null)?;
    let (transition, current, pending) = playbin.state(gst::ClockTime::from_seconds(1));
    transition?;
    if current == gst::State::Null && pending == gst::State::VoidPending {
        Ok(())
    } else {
        Err(gst::StateChangeError)
    }
}

/// If an external subtitle makes an otherwise-working pipeline fail, remove
/// only that auxiliary URI and asynchronously restore the video. The external
/// marker is cleared before retrying, so a genuine video failure on the retry
/// follows the normal fatal path instead of looping (FR-10.7).
fn recover_without_external(playbin: &gst::Element, playback: &RefCell<FocusedPlayback>) -> bool {
    let had_external = playback.borrow().streams.external.is_some();
    if !had_external {
        return false;
    }
    let Some(video) = playback.borrow().current_video.clone() else {
        return false;
    };
    let Ok(uri) = glib::filename_to_uri(&video, None) else {
        return false;
    };
    // Never query a failed pipeline here. Some sinks answer position/state
    // synchronously by waiting on the streaming thread that just errored.
    // Replacement already owns an exact resume point; an automatic sidecar
    // failing during initial playback safely falls back to the beginning.
    let (position, rate, play_after_seek) = {
        let playback = playback.borrow();
        playback
            .resume
            .as_ref()
            .map_or((0.0, playback.playback_rate, true), |state| {
                (state.position, state.rate, state.play_after_seek)
            })
    };

    crate::applog!("player: subtitle recovery tearing pipeline down");
    if teardown_pipeline(playbin).is_err() {
        crate::applog!("player: subtitle recovery could not reach null");
        return false;
    }
    playback
        .borrow_mut()
        .prepare_subtitle_rebuild(None, position, rate, play_after_seek);
    configure_uris(playbin, &uri, None);
    if playbin.set_state(gst::State::Playing).is_err() {
        playback.borrow_mut().resume = None;
        crate::applog!("player: subtitle recovery could not restart video");
        return false;
    }
    crate::applog!("player: external subtitle failed; restoring video without it");
    true
}

/// Find one deterministic automatic sidecar without involving the folder
/// model or GIO. Exact `video.srt` wins, then SRT over WebVTT, then lexical
/// order among language/role suffixes (FR-10.7).
fn matching_sidecar(video: &Path) -> Option<PathBuf> {
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

fn apply_stream_choices(
    playbin: &gst::Element,
    playback: &RefCell<FocusedPlayback>,
    audio_request: Option<&AudioChoice>,
    subtitle_request: Option<&SubtitleChoice>,
) -> bool {
    let selected = playback
        .borrow()
        .requested_streams(audio_request, subtitle_request);
    if selected.is_empty() {
        return false;
    }
    let event = gst::event::SelectStreams::new(selected.iter().map(String::as_str));
    playbin.send_event(event)
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

/// Give the verified Intel decoder priority over standard `Primary + 1`
/// hardware and `Primary` software decoders. `None` means preserve an
/// explicit disable (`Rank::None`) or a choice already ranked above ours.
fn preferred_hardware_rank(current: gst::Rank) -> Option<gst::Rank> {
    let preferred = gst::Rank::PRIMARY + 2;
    (current != gst::Rank::NONE && current < preferred).then_some(preferred)
}

/// Change only this process's registry. Missing factories are expected on
/// other Intel generations, and an explicitly disabled factory stays off.
/// This remains after lazy `gst::init` so image-only startup does not load
/// GStreamer (NFR-1.1).
fn prefer_intel_video_decoders() {
    for (name, _) in INTEL_VIDEO_DECODERS {
        let Some(factory) = gst::ElementFactory::find(name) else {
            continue;
        };
        let current = factory.rank();
        let Some(preferred) = preferred_hardware_rank(current) else {
            continue;
        };
        factory.set_rank(preferred);
        crate::applog!("player: prefer {name} ({current} -> {preferred})");
    }
}

/// GStreamer can keep playing the audio half of a file when its video
/// decoder is missing. That is a successful pipeline state rather than
/// an `Error` message, so recognise the accompanying `missing-plugin`
/// element message and surface the broken video playback (FR-10.6).
/// Missing audio and subtitle decoders do not make the picture unusable.
fn missing_video_decoder(structure: &gst::StructureRef) -> Option<String> {
    missing_decoder_matching(structure, |media_type| media_type.starts_with("video/"))
}

fn missing_subtitle_decoder(structure: &gst::StructureRef) -> Option<String> {
    missing_decoder_matching(structure, |media_type| {
        media_type.starts_with("text/")
            || media_type.starts_with("subpicture/")
            || media_type.starts_with("closedcaption/")
            || media_type.starts_with("application/x-subtitle")
    })
}

fn missing_decoder_matching(
    structure: &gst::StructureRef,
    matches_media_type: impl Fn(&str) -> bool,
) -> Option<String> {
    if structure.name() != "missing-plugin"
        || structure.get::<String>("type").ok().as_deref() != Some("decoder")
    {
        return None;
    }

    let caps = structure.get::<gst::Caps>("detail").ok()?;
    if !caps
        .iter()
        .any(|candidate| matches_media_type(candidate.name().as_str()))
    {
        return None;
    }

    Some(
        structure
            .get::<String>("name")
            .ok()
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| caps.to_string()),
    )
}

fn same_rate(left: f64, right: f64) -> bool {
    (left - right).abs() < f64::EPSILON
}

/// Send the seek and record it as in flight. Free-standing because the bus
/// watch flushes queued scrub/rate requests without holding a `Player`.
fn issue_seek(
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

impl Drop for Player {
    fn drop(&mut self) {
        // NFR-2.2: nothing keeps running once the window is gone.
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use gstreamer::prelude::PluginFeatureExtManual;

    use super::{
        AudioChoice, DecoderFallback, FocusedPlayback, INTEL_VIDEO_DECODERS, Instant, ResumeAction,
        SEEK_SETTLE, SeekRequest, SeekState, StreamState, SubtitleChoice, SubtitleTrack,
        cycled_subtitle_choice, gst, h264_exceeds_declared_level, matching_sidecar,
        missing_subtitle_decoder, missing_video_decoder, prefer_intel_video_decoders,
        preferred_hardware_rank, refresh_stream_tracks, replace_stream_collection,
        stream_selection_ids, toggled_subtitle_choice, video_stream_summary,
    };

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
        playback.prepare_subtitle_rebuild(Some("movie.en.srt".into()), 42.5, 1.5, true);

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
        playback.finish_error(&stale);
        assert!(playback.begin_error().is_none());
        playback.finish_error(&current);
        assert!(playback.begin_error().is_some());
    }

    #[test]
    fn focused_playback_trace_revalidates_choices_on_collection_changes() {
        gst::init().unwrap();
        let stream = |id, kind| gst::Stream::new(Some(id), None, kind, gst::StreamFlags::SELECT);
        let mut playback = FocusedPlayback::default();
        playback.streams.audio_choice = AudioChoice::Track("commentary".into());
        playback.streams.subtitle_choice = SubtitleChoice::Track("english".into());

        playback.replace_streams(
            gst::StreamCollection::builder(None)
                .streams([
                    stream("video", gst::StreamType::VIDEO),
                    stream("commentary", gst::StreamType::AUDIO),
                    stream("english", gst::StreamType::TEXT),
                ])
                .build(),
        );
        assert_eq!(
            playback.streams.audio_choice,
            AudioChoice::Track("commentary".into())
        );

        playback.replace_streams(
            gst::StreamCollection::builder(None)
                .streams([
                    stream("video-2", gst::StreamType::VIDEO),
                    stream("main", gst::StreamType::AUDIO),
                ])
                .build(),
        );
        assert_eq!(playback.streams.audio_choice, AudioChoice::Automatic);
        assert_eq!(playback.streams.subtitle_choice, SubtitleChoice::Automatic);
    }

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
    fn a_lost_seek_stops_blocking_and_stops_being_reported() {
        let mut state = SeekState::default();
        issued(&mut state, request(12.0, 1.0), SEEK_SETTLE);
        assert_eq!(state.pending(), None);
        assert!(state.request(request(20.0, 1.0)));
    }

    #[test]
    fn hardware_preference_preserves_explicit_rank_choices() {
        assert_eq!(
            preferred_hardware_rank(gst::Rank::MARGINAL),
            Some(gst::Rank::PRIMARY + 2)
        );
        assert_eq!(
            preferred_hardware_rank(gst::Rank::PRIMARY + 1),
            Some(gst::Rank::PRIMARY + 2)
        );
        assert_eq!(preferred_hardware_rank(gst::Rank::NONE), None);
        assert_eq!(preferred_hardware_rank(gst::Rank::PRIMARY + 3), None);
    }

    #[test]
    fn installed_intel_video_decoders_outrank_standard_hardware() {
        gst::init().unwrap();
        prefer_intel_video_decoders();

        for (name, _) in INTEL_VIDEO_DECODERS {
            let Some(factory) = gst::ElementFactory::find(name) else {
                continue;
            };
            if factory.rank() != gst::Rank::NONE {
                assert!(factory.rank() > gst::Rank::PRIMARY + 1, "{name}");
            }
        }
    }

    #[test]
    fn invalid_h264_level_temporarily_disables_qsv_h264() {
        gst::init().unwrap();
        prefer_intel_video_decoders();
        let Some(factory) = gst::ElementFactory::find("qsvh264dec") else {
            return;
        };
        let Some(fallback) = gst::ElementFactory::find("avdec_h264") else {
            return;
        };
        if fallback.rank() == gst::Rank::NONE {
            return;
        }
        let caps = gst::Caps::builder("video/x-h264")
            .field("level", "4")
            .field("width", 4_382i32)
            .field("height", 3_500i32)
            .field("framerate", gst::Fraction::new(30, 1))
            .build();
        let original = factory.rank();
        let mut fallback = DecoderFallback::default();

        assert!(h264_exceeds_declared_level(&caps));
        assert_eq!(fallback.bypass_qsv_h264(&caps), Some("avdec_h264"));
        assert_eq!(factory.rank(), gst::Rank::NONE);
        assert_eq!(fallback.bypass_qsv_h264(&caps), None);
        fallback.restore();
        assert_eq!(factory.rank(), original);
    }

    #[test]
    fn frame_within_h264_level_keeps_hardware_at_high_framerates() {
        gst::init().unwrap();
        let caps = gst::Caps::builder("video/x-h264")
            .field("level", "4")
            .field("width", 1_120i32)
            .field("height", 1_632i32)
            .field("framerate", gst::Fraction::new(60, 1))
            .build();

        assert!(!h264_exceeds_declared_level(&caps));
    }

    #[test]
    fn video_diagnostics_include_decoder_relevant_stream_facts() {
        gst::init().unwrap();
        let caps = gst::Caps::builder("video/x-h264")
            .field("profile", "high")
            .field("level", "4.1")
            .field("width", 1_920i32)
            .field("height", 1_080i32)
            .field("framerate", gst::Fraction::new(30, 1))
            .build();

        assert_eq!(
            video_stream_summary(&caps),
            Some("video/x-h264 profile=high level=4.1 1920x1080 30/1 fps".to_string())
        );
        assert_eq!(
            video_stream_summary(&gst::Caps::builder("video/x-vp9").build()),
            Some("video/x-vp9".to_string())
        );
    }

    #[test]
    fn a_missing_video_decoder_is_not_mistaken_for_successful_playback() {
        gst::init().unwrap();
        let message = gst::Structure::builder("missing-plugin")
            .field("type", "decoder")
            .field("detail", gst::Caps::builder("video/x-h264").build())
            .field("name", "H.264 High 10 decoder")
            .build();

        assert_eq!(
            missing_video_decoder(&message),
            Some("H.264 High 10 decoder".to_owned())
        );
    }

    #[test]
    fn missing_non_video_decoders_do_not_hide_a_playable_picture() {
        gst::init().unwrap();
        for media_type in ["audio/mpeg", "text/x-raw"] {
            let message = gst::Structure::builder("missing-plugin")
                .field("type", "decoder")
                .field("detail", gst::Caps::builder(media_type).build())
                .field("name", "optional stream decoder")
                .build();

            assert_eq!(missing_video_decoder(&message), None);
        }
    }

    #[test]
    fn missing_text_decoder_is_reported_as_a_subtitle_failure() {
        gst::init().unwrap();
        let message = gst::Structure::builder("missing-plugin")
            .field("type", "decoder")
            .field("detail", gst::Caps::builder("text/x-raw").build())
            .field("name", "subtitle decoder")
            .build();

        assert_eq!(
            missing_subtitle_decoder(&message),
            Some("subtitle decoder".to_string())
        );
        assert_eq!(missing_video_decoder(&message), None);
    }
}
