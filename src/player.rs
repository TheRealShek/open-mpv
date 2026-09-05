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
//! Private child modules own decoder policy, focused playback state, and
//! stream choices. `Player` remains the window-facing adapter and keeps
//! pipeline effects, lazy initialization and paintable ownership here.

use std::cell::{Cell, RefCell};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use gstreamer as gst;
use gstreamer::prelude::*;

use gtk4::gdk;
use gtk4::glib;

mod decoder;
mod playback;
mod tracks;
use decoder::{
    DecoderFallback, lock_decoder_fallback, missing_subtitle_decoder, missing_video_decoder,
    prefer_intel_video_decoders, video_stream_summary,
};
pub use playback::PlaybackRateError;
use playback::{FocusedPlayback, ResumeAction, SeekRequest, issue_seek, same_rate};
#[allow(unused_imports)]
pub use tracks::AudioTrack;
pub use tracks::{AudioChoice, AudioSnapshot, SubtitleChoice, SubtitleSnapshot, SubtitleTrack};
use tracks::{StreamState, matching_sidecar};

const VOLUME_MAX: f64 = 1.5;
pub const PLAYBACK_RATES: &[f64] = &[0.5, 0.75, 1.0, 1.25, 1.5, 2.0];
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

pub struct Player {
    playbin: gst::Element,
    seek_target: gst::Element,
    paintable: gdk::Paintable,
    playback: Rc<RefCell<FocusedPlayback>>,
    streams: Rc<RefCell<StreamState>>,
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
        let streams = Rc::new(RefCell::new(StreamState::default()));
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
                let streams = streams.clone();
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
                            let external = streams.borrow().external().map(Path::to_path_buf);
                            let Some(error_context) =
                                playback.borrow_mut().begin_error(external)
                            else {
                                crate::applog!(
                                    "player: ignoring error queued behind pending recovery"
                                );
                                return glib::ControlFlow::Continue;
                            };
                            let error = e.error();
                            let playbin = playbin.clone();
                            let playback = playback.clone();
                            let streams = streams.clone();
                            let on_event = on_event.clone();
                            // Returning from the bus watch before changing
                            // state is mandatory: tearing a failed pipeline
                            // down from inside its Error callback can wait on
                            // the streaming thread that posted this message.
                            glib::idle_add_local_once(move || {
                                let still_current = playback.borrow().error_is_current(
                                    &error_context,
                                    streams.borrow().external(),
                                );
                                if !still_current {
                                    crate::applog!("player: stale pipeline error superseded");
                                    playback.borrow_mut().finish_error(&error_context);
                                    return;
                                }
                                let recovered =
                                    recover_without_external(&playbin, &playback, &streams);
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
                                let context = playback.borrow().context(
                                    streams.borrow().external().map(Path::to_path_buf),
                                );
                                let playback = playback.clone();
                                let streams = streams.clone();
                                let on_event = on_event.clone();
                                glib::idle_add_local_once(move || {
                                    if playback
                                        .borrow()
                                        .error_is_current(&context, streams.borrow().external())
                                    {
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
                                        || !same_rate(rate, playback.borrow().accepted_rate());
                                    if !needs_seek
                                        || !issue_seek(
                                            &seek_target,
                                            &playback,
                                            SeekRequest::new(position, rate),
                                        )
                                    {
                                        if needs_seek
                                            && !same_rate(rate, playback.borrow().accepted_rate())
                                        {
                                            on_event(Event::PlaybackRateError(
                                                PlaybackRateError::SeekRefused,
                                            ));
                                        }
                                        playback.borrow_mut().cancel_resume();
                                        let target = if resume_playing {
                                            gst::State::Playing
                                        } else {
                                            gst::State::Paused
                                        };
                                        let _ = playbin.set_state(target);
                                        playback.borrow_mut().set_playing(resume_playing);
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
                                    playback.borrow_mut().set_playing(resume_playing);
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
                                    !same_rate(request.rate(), playback.borrow().accepted_rate());
                                if !issue_seek(&seek_target, &playback, request)
                                    && rate_change
                                {
                                    on_event(Event::PlaybackRateError(
                                        PlaybackRateError::SeekRefused,
                                    ));
                                }
                            }
                        }
                        gst::MessageView::StreamCollection(message) => {
                            let collection = message.stream_collection();
                            let should_apply = streams.borrow_mut().replace_collection(collection);
                            let (audio_count, subtitle_count) = streams.borrow().track_counts();
                            crate::applog!(
                                "player: discovered {audio_count} audio and {subtitle_count} subtitle track(s)"
                            );
                            if should_apply {
                                apply_stream_choices(&playbin, &streams, None, None);
                            }
                            let (audio, subtitles) = streams.borrow().snapshots();
                            on_event(Event::AudioChanged(audio));
                            on_event(Event::SubtitlesChanged(subtitles));
                        }
                        gst::MessageView::StreamsSelected(message) => {
                            let selected = message
                                .streams()
                                .filter_map(|stream| stream.stream_id().map(|id| id.to_string()))
                                .collect();
                            streams.borrow_mut().select(selected);
                            let (audio, subtitles) = streams.borrow().snapshots();
                            on_event(Event::AudioChanged(audio));
                            on_event(Event::SubtitlesChanged(subtitles));
                        }
                        gst::MessageView::DurationChanged(_) => {
                            playback.borrow_mut().invalidate_duration();
                        }
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
            streams,
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
        self.streams.borrow().external().is_some()
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
        self.playback.borrow_mut().start_video(path);
        let mut streams = self.streams.borrow_mut();
        *streams = StreamState::new(self.subtitles_default_on.get());
        streams.set_external(subtitle);
        drop(streams);
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
        self.playback.borrow_mut().reset();
        *self.streams.borrow_mut() = StreamState::new(self.subtitles_default_on.get());
    }

    /// Set the initial subtitle policy applied independently to every
    /// newly opened video (FR-8.2/10.7).
    pub fn set_subtitles_default(&self, enabled: bool) {
        self.subtitles_default_on.set(enabled);
        if self.playback.borrow().current_video().is_none() {
            self.streams.borrow_mut().set_default_subtitles(enabled);
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
            .current_video()
            .map(Path::to_path_buf)
            .ok_or_else(|| PlayerError::SubtitleFile {
                path: path.to_path_buf(),
                source: io::Error::new(io::ErrorKind::InvalidInput, "no video is playing"),
            })?;
        let already_attached = self.streams.borrow().external() == Some(path);
        if already_attached {
            crate::applog!(
                "player: subtitle {} already attached; selecting existing track",
                path.display()
            );
            if !self.choose_subtitle(SubtitleChoice::Automatic) {
                self.streams.borrow_mut().reset_subtitle_choice();
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
        self.playback
            .borrow_mut()
            .prepare_subtitle_rebuild(position, rate, play_after_seek);
        self.streams
            .borrow_mut()
            .reset_for_subtitle_rebuild(Some(path.to_path_buf()));
        configure_uris(&self.playbin, &uri, Some(&suburi));
        crate::applog!("player: attached subtitle {}", path.display());
        if let Err(source) = self.playbin.set_state(gst::State::Playing) {
            self.playback.borrow_mut().cancel_resume();
            let _ = recover_without_external(&self.playbin, &self.playback, &self.streams);
            return Err(PlayerError::Playback {
                path: video,
                source,
            });
        }
        Ok(())
    }

    pub fn subtitle_snapshot(&self) -> SubtitleSnapshot {
        self.streams.borrow().subtitle_snapshot()
    }

    pub fn audio_snapshot(&self) -> AudioSnapshot {
        self.streams.borrow().audio_snapshot()
    }

    pub fn choose_audio(&self, choice: AudioChoice) -> bool {
        if !self.streams.borrow().audio_choice_available(&choice) {
            return false;
        }
        let sent = apply_stream_choices(&self.playbin, &self.streams, Some(&choice), None);
        if sent {
            crate::applog!("player: audio selection {}", choice.action_target());
            self.streams.borrow_mut().set_audio_choice(choice);
        }
        sent
    }

    pub fn choose_subtitle(&self, choice: SubtitleChoice) -> bool {
        if !self.streams.borrow().subtitle_choice_available(&choice) {
            return false;
        }
        let sent = apply_stream_choices(&self.playbin, &self.streams, None, Some(&choice));
        if sent {
            crate::applog!("player: subtitle selection {}", choice.action_target());
            self.streams.borrow_mut().set_subtitle_choice(choice);
        }
        sent
    }

    pub fn toggle_subtitles(&self) -> SubtitleSnapshot {
        if !self.streams.borrow().has_subtitles() {
            return self.subtitle_snapshot();
        }
        let choice = self.streams.borrow().toggled_subtitle_choice();
        self.choose_subtitle(choice);
        self.subtitle_snapshot()
    }

    pub fn cycle_subtitles(&self) -> SubtitleSnapshot {
        if !self.streams.borrow().has_subtitles() {
            return self.subtitle_snapshot();
        }
        let choice = self.streams.borrow().cycled_subtitle_choice();
        self.choose_subtitle(choice);
        self.subtitle_snapshot()
    }

    /// True when playback is running or the user has asked it to run.
    pub fn is_playing(&self) -> bool {
        self.playback.borrow().is_playing()
    }

    pub fn is_muted(&self) -> bool {
        self.playbin.property::<bool>("mute")
    }

    /// Toggle pause; returns true when now playing.
    pub fn toggle_pause(&self) -> bool {
        if self.playback.borrow().is_playing() {
            let _ = self.playbin.set_state(gst::State::Paused);
            self.playback.borrow_mut().set_playing(false);
            crate::applog!("player: paused");
            false
        } else {
            let _ = self.playbin.set_state(gst::State::Playing);
            self.playback.borrow_mut().set_playing(true);
            crate::applog!("player: playing");
            true
        }
    }

    /// Position and duration in seconds, once the pipeline knows them.
    /// While a seek is in flight the position is its target, so the UI
    /// tracks the user instead of the pipeline's catch-up.
    pub fn progress(&self) -> Option<(f64, f64)> {
        let dur = self.duration()?;
        let pos = match self.playback.borrow().pending_seek_position() {
            Some(position) => position,
            None => self
                .playbin
                .query_position::<gst::ClockTime>()?
                .seconds_f64(),
        };
        Some((pos.clamp(0.0, dur), dur))
    }

    /// Stream duration in seconds, cached once the demuxer reports it.
    fn duration(&self) -> Option<f64> {
        if let Some(dur) = self.playback.borrow().cached_duration() {
            return Some(dur);
        }
        let dur = self
            .playbin
            .query_duration::<gst::ClockTime>()?
            .seconds_f64();
        if dur <= 0.0 {
            return None;
        }
        self.playback.borrow_mut().cache_duration(dur);
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
        let request = SeekRequest::new(secs, self.playback_rate());
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
        let request = SeekRequest::new(position, rate);
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
        self.playback.borrow_mut().set_playing(true);
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

fn apply_stream_choices(
    playbin: &gst::Element,
    streams: &RefCell<StreamState>,
    audio_request: Option<&AudioChoice>,
    subtitle_request: Option<&SubtitleChoice>,
) -> bool {
    let selected = streams
        .borrow()
        .selection_ids(audio_request, subtitle_request);
    if selected.is_empty() {
        return false;
    }
    let event = gst::event::SelectStreams::new(selected.iter().map(String::as_str));
    playbin.send_event(event)
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
fn recover_without_external(
    playbin: &gst::Element,
    playback: &RefCell<FocusedPlayback>,
    streams: &RefCell<StreamState>,
) -> bool {
    if streams.borrow().external().is_none() {
        return false;
    }
    let Some(video) = playback.borrow().current_video().map(Path::to_path_buf) else {
        return false;
    };
    let Ok(uri) = glib::filename_to_uri(&video, None) else {
        return false;
    };
    // Never query a failed pipeline here. Some sinks answer position/state
    // synchronously by waiting on the streaming thread that just errored.
    // Replacement already owns an exact resume point; an automatic sidecar
    // failing during initial playback safely falls back to the beginning.
    let (position, rate, play_after_seek) = playback.borrow().resume_point();

    crate::applog!("player: subtitle recovery tearing pipeline down");
    if teardown_pipeline(playbin).is_err() {
        crate::applog!("player: subtitle recovery could not reach null");
        return false;
    }
    playback
        .borrow_mut()
        .prepare_subtitle_rebuild(position, rate, play_after_seek);
    streams.borrow_mut().reset_for_subtitle_rebuild(None);
    configure_uris(playbin, &uri, None);
    if playbin.set_state(gst::State::Playing).is_err() {
        playback.borrow_mut().cancel_resume();
        crate::applog!("player: subtitle recovery could not restart video");
        return false;
    }
    crate::applog!("player: external subtitle failed; restoring video without it");
    true
}

impl Drop for Player {
    fn drop(&mut self) {
        // NFR-2.2: nothing keeps running once the window is gone.
        self.stop();
    }
}
