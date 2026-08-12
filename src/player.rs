//! Video playback (FR-10.1): one `playbin3` pipeline rendering into a
//! `gtk4paintablesink`, whose `GdkPaintable` the viewer displays like
//! any other paintable.
//!
//! This is the resource-minimal path on this machine: VA-API decoders
//! decode on the iGPU and frames reach GTK as dmabufs — no CPU pixel
//! copies. GStreamer is initialized lazily on the first video so
//! image-only sessions keep their cold-start and footprint (NFR-1.1,
//! NFR-2.1). The pipeline is reused across videos; `stop` drops it to
//! `Null`, freeing decoder state while an image is shown.

use std::cell::{Cell, RefCell};
use std::fmt;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use gstreamer as gst;
use gstreamer::prelude::*;

use gtk4::gdk;
use gtk4::glib;

/// Seeks land on the exact target, not on the nearest keyframe. Keyframe
/// seeks are cheaper, but short clips are routinely encoded as a single
/// GOP — every seek then snaps back to 0:00 and the video looks stuck at
/// the start. Measured on this machine, an accurate seek costs 2–455 ms,
/// and at most one is ever in flight (see `SeekState`).
const SEEK_FLAGS: gst::SeekFlags = gst::SeekFlags::FLUSH.union(gst::SeekFlags::ACCURATE);
const VOLUME_MAX: f64 = 1.5;
/// Hardware decoders proven on the target machine. GStreamer's libav
/// decoders rank at `Primary`, while these QSV factories normally rank
/// lower. Prefer QSV for streams its caps accept and leave libav as the
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
            PlayerError::MissingBus => None,
        }
    }
}

/// A flushing seek only answers position queries with the new position
/// once the pipeline has re-prerolled; until then it still reports where
/// it was. Two consequences the UI would otherwise wear: the seek bar
/// snaps backwards after every scrub step, and repeated `seek_by` calls
/// all compute their delta from the same stale position. `in_flight`
/// covers that gap, and `queued` coalesces the scrub positions that
/// arrive while a seek is running — issuing them all would flood the
/// pipeline with flushes and leave the picture trailing the pointer.
#[derive(Default)]
struct SeekState {
    in_flight: Option<(f64, Instant)>,
    queued: Option<f64>,
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

    let frame_mbs = (width as u64).div_ceil(16) * (height as u64).div_ceil(16);
    frame_mbs > max_frame_mbs
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
    fn pending(&self) -> Option<f64> {
        self.queued.or_else(|| self.running().map(|(secs, _)| secs))
    }

    /// The in-flight seek, unless it is old enough to count as lost.
    fn running(&self) -> Option<(f64, Instant)> {
        self.in_flight.filter(|(_, at)| at.elapsed() < SEEK_SETTLE)
    }

    /// Record a request to seek to `secs`. Returns true when the caller
    /// must issue it, false when the running seek will pick it up.
    fn request(&mut self, secs: f64) -> bool {
        if self.running().is_some() {
            self.queued = Some(secs);
            return false;
        }
        true
    }
}

pub struct Player {
    playbin: gst::Element,
    paintable: gdk::Paintable,
    seek: Rc<RefCell<SeekState>>,
    /// Cached so the per-frame transport update does not re-query the
    /// demuxer; invalidated on `DurationChanged` and on every new video.
    duration: Rc<Cell<Option<f64>>>,
    decoder_fallback: Arc<Mutex<DecoderFallback>>,
    /// Keeps the bus watch alive; dropping it detaches the watch.
    _bus_watch: gst::bus::BusWatchGuard,
}

impl Player {
    /// Build the pipeline. `on_event` fires on the GTK main loop.
    pub fn new(on_event: impl Fn(Event) + 'static) -> Result<Player, PlayerError> {
        gst::init().map_err(PlayerError::Init)?;
        prefer_intel_video_decoders();
        let sink = gst::ElementFactory::make("gtk4paintablesink")
            .build()
            .map_err(PlayerError::SinkUnavailable)?;
        // The sink's paintable must be pulled from the main thread.
        let paintable = sink.property::<gdk::Paintable>("paintable");
        let playbin = gst::ElementFactory::make("playbin3")
            .property("video-sink", &sink)
            .build()
            .map_err(PlayerError::PlaybinUnavailable)?;

        let seek = Rc::new(RefCell::new(SeekState::default()));
        let duration = Rc::new(Cell::new(None));
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
                    if INTEL_VIDEO_DECODERS.iter().any(|(hardware, software)| {
                        factory.name() == *hardware || factory.name() == *software
                    }) {
                        crate::applog!("player: selected decoder {}", factory.name());
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
                        let Some(caps) = collection.stream(index).and_then(|stream| stream.caps())
                        else {
                            continue;
                        };
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
                let seek = seek.clone();
                let duration = duration.clone();
                let decoder_fallback = decoder_fallback.clone();
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
                            on_event(Event::Error(e.error()));
                        }
                        gst::MessageView::Element(e) => {
                            if let Some(description) = e.structure().and_then(missing_video_decoder)
                            {
                                crate::applog!("player: missing video decoder: {description}");
                                on_event(Event::MissingVideoDecoder(description));
                            }
                        }
                        // The seek landed: real positions are truthful
                        // again, and the newest scrub position that piled
                        // up behind it can go out now.
                        gst::MessageView::AsyncDone(_) => {
                            let next = {
                                let mut state = seek.borrow_mut();
                                state.in_flight = None;
                                state.queued.take()
                            };
                            if let Some(secs) = next {
                                issue_seek(&playbin, &seek, secs);
                            }
                        }
                        gst::MessageView::DurationChanged(_) => duration.set(None),
                        _ => {}
                    }
                    glib::ControlFlow::Continue
                }
            })
            .map_err(PlayerError::BusWatch)?;

        Ok(Player {
            playbin,
            paintable,
            seek,
            duration,
            decoder_fallback,
            _bus_watch: bus_watch,
        })
    }

    pub fn paintable(&self) -> gdk::Paintable {
        self.paintable.clone()
    }

    /// Start playing `path` from the beginning, replacing any current
    /// video. The pipeline object is reused; only its state cycles.
    pub fn play(&self, path: &Path) -> Result<(), PlayerError> {
        let uri = glib::filename_to_uri(path, None).map_err(|source| PlayerError::Uri {
            path: path.to_path_buf(),
            source,
        })?;
        let _ = self.playbin.set_state(gst::State::Null);
        self.forget_stream();
        self.playbin.set_property("uri", uri.as_str());
        self.playbin
            .set_state(gst::State::Playing)
            .map_err(|source| PlayerError::Playback {
                path: path.to_path_buf(),
                source,
            })?;
        Ok(())
    }

    /// Drop to `Null`: stops playback and frees decoder state.
    pub fn stop(&self) {
        let (_, current, _) = self.playbin.state(gst::ClockTime::ZERO);
        let _ = self.playbin.set_state(gst::State::Null);
        self.forget_stream();
        if current != gst::State::Null {
            crate::applog!("player: stopped, pipeline released");
        }
    }

    /// Drop everything that describes the outgoing stream so the next
    /// video never reports the previous one's duration or seek target.
    fn forget_stream(&self) {
        lock_decoder_fallback(&self.decoder_fallback).restore();
        *self.seek.borrow_mut() = SeekState::default();
        self.duration.set(None);
    }

    /// Where the pipeline is heading: the pending state while a
    /// transition is in flight, otherwise the current one. Asking for the
    /// current state alone would report the state being left behind.
    fn target_state(&self) -> gst::State {
        let (_, current, pending) = self.playbin.state(gst::ClockTime::ZERO);
        if pending == gst::State::VoidPending {
            current
        } else {
            pending
        }
    }

    /// True when playback is running or about to be.
    pub fn is_playing(&self) -> bool {
        self.target_state() == gst::State::Playing
    }

    pub fn is_muted(&self) -> bool {
        self.playbin.property::<bool>("mute")
    }

    /// Toggle pause; returns true when now playing.
    pub fn toggle_pause(&self) -> bool {
        let target = self.target_state();
        if target == gst::State::Playing {
            let _ = self.playbin.set_state(gst::State::Paused);
            crate::applog!("player: paused");
            false
        } else {
            let _ = self.playbin.set_state(gst::State::Playing);
            crate::applog!("player: playing");
            true
        }
    }

    /// Position and duration in seconds, once the pipeline knows them.
    /// While a seek is in flight the position is its target, so the UI
    /// tracks the user instead of the pipeline's catch-up.
    pub fn progress(&self) -> Option<(f64, f64)> {
        let dur = self.duration()?;
        let pos = match self.seek.borrow().pending() {
            Some(secs) => secs,
            None => self.playbin.query_position::<gst::ClockTime>()?.nseconds() as f64 / 1e9,
        };
        Some((pos.clamp(0.0, dur), dur))
    }

    /// Stream duration in seconds, cached once the demuxer reports it.
    fn duration(&self) -> Option<f64> {
        if let Some(dur) = self.duration.get() {
            return Some(dur);
        }
        let dur = self.playbin.query_duration::<gst::ClockTime>()?.nseconds() as f64 / 1e9;
        if dur <= 0.0 {
            return None;
        }
        self.duration.set(Some(dur));
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
        let issue_now = self.seek.borrow_mut().request(secs);
        if issue_now {
            issue_seek(&self.playbin, &self.seek, secs);
        }
    }

    /// Restart from the beginning (EOS loop, FR-10.3).
    pub fn rewind(&self) {
        self.seek_to(0.0);
        let _ = self.playbin.set_state(gst::State::Playing);
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

/// Give a working hardware decoder priority over the `Primary` software
/// fallback. `None` means preserve an explicit disable (`Rank::None`) or
/// a choice already ranked above ours.
fn preferred_hardware_rank(current: gst::Rank) -> Option<gst::Rank> {
    let preferred = gst::Rank::PRIMARY + 1;
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
    if structure.name() != "missing-plugin"
        || structure.get::<String>("type").ok().as_deref() != Some("decoder")
    {
        return None;
    }

    let caps = structure.get::<gst::Caps>("detail").ok()?;
    if !caps
        .iter()
        .any(|candidate| candidate.name().starts_with("video/"))
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

/// Send the seek and record it as in flight. Free-standing because the
/// bus watch flushes queued scrub positions without holding a `Player`.
fn issue_seek(playbin: &gst::Element, seek: &RefCell<SeekState>, secs: f64) {
    let secs = secs.max(0.0);
    {
        let mut state = seek.borrow_mut();
        state.in_flight = Some((secs, Instant::now()));
        state.queued = None;
    }
    let target = gst::ClockTime::from_nseconds((secs * 1e9) as u64);
    let _ = playbin.seek_simple(SEEK_FLAGS, target);
    crate::applog!("player: seek to {secs:.1}s");
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
        DecoderFallback, INTEL_VIDEO_DECODERS, Instant, SEEK_SETTLE, SeekState, gst,
        h264_exceeds_declared_level, missing_video_decoder, prefer_intel_video_decoders,
        preferred_hardware_rank,
    };

    /// Stand-in for what `issue_seek` records on the pipeline's behalf.
    fn issued(state: &mut SeekState, secs: f64, ago: std::time::Duration) {
        state.in_flight = Some((secs, Instant::now() - ago));
        state.queued = None;
    }

    #[test]
    fn first_seek_goes_out_immediately() {
        let mut state = SeekState::default();
        assert!(state.request(12.0));
        assert_eq!(state.queued, None);
    }

    #[test]
    fn scrubbing_during_a_seek_keeps_only_the_newest_position() {
        let mut state = SeekState::default();
        issued(&mut state, 12.0, std::time::Duration::ZERO);
        assert!(!state.request(20.0));
        assert!(!state.request(31.0));
        assert_eq!(state.queued, Some(31.0));
        // The UI follows the pointer, not the seek still on its way.
        assert_eq!(state.pending(), Some(31.0));
    }

    #[test]
    fn a_lost_seek_stops_blocking_and_stops_being_reported() {
        let mut state = SeekState::default();
        issued(&mut state, 12.0, SEEK_SETTLE);
        assert_eq!(state.pending(), None);
        assert!(state.request(20.0));
    }

    #[test]
    fn hardware_preference_preserves_explicit_rank_choices() {
        assert_eq!(
            preferred_hardware_rank(gst::Rank::MARGINAL),
            Some(gst::Rank::PRIMARY + 1)
        );
        assert_eq!(preferred_hardware_rank(gst::Rank::NONE), None);
        assert_eq!(preferred_hardware_rank(gst::Rank::PRIMARY + 2), None);
    }

    #[test]
    fn installed_intel_video_decoders_outrank_software_fallbacks() {
        gst::init().unwrap();
        prefer_intel_video_decoders();

        for (name, _) in INTEL_VIDEO_DECODERS {
            let Some(factory) = gst::ElementFactory::find(name) else {
                continue;
            };
            if factory.rank() != gst::Rank::NONE {
                assert!(factory.rank() > gst::Rank::PRIMARY, "{name}");
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
}
