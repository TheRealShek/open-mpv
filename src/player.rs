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
use std::path::Path;
use std::rc::Rc;
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
/// Safety net: if a seek never gets its `AsyncDone` (broken file, stalled
/// demuxer), stop reporting its target and fall back to real queries.
const SEEK_SETTLE: Duration = Duration::from_millis(1500);

/// Pipeline happenings the window reacts to; delivered on the main loop.
pub enum Event {
    EndOfStream,
    Error(String),
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
    /// Keeps the bus watch alive; dropping it detaches the watch.
    _bus_watch: gst::bus::BusWatchGuard,
}

impl Player {
    /// Build the pipeline. `on_event` fires on the GTK main loop.
    pub fn new(on_event: impl Fn(Event) + 'static) -> Result<Player, String> {
        gst::init().map_err(|e| format!("GStreamer init failed: {e}"))?;
        let sink = gst::ElementFactory::make("gtk4paintablesink")
            .build()
            .map_err(|e| format!("gtk4paintablesink unavailable: {e}"))?;
        // The sink's paintable must be pulled from the main thread.
        let paintable = sink.property::<gdk::Paintable>("paintable");
        let playbin = gst::ElementFactory::make("playbin3")
            .property("video-sink", &sink)
            .build()
            .map_err(|e| format!("playbin3 unavailable: {e}"))?;

        let seek = Rc::new(RefCell::new(SeekState::default()));
        let duration = Rc::new(Cell::new(None));

        let bus = playbin.bus().ok_or("playbin has no bus")?;
        let bus_watch = bus
            .add_watch_local({
                // The pipeline holds the bus, which holds this closure —
                // the guard below detaches the watch on drop, so the
                // cycle ends with the `Player`.
                let playbin = playbin.clone();
                let seek = seek.clone();
                let duration = duration.clone();
                move |_, msg| {
                    match msg.view() {
                        gst::MessageView::Eos(_) => on_event(Event::EndOfStream),
                        gst::MessageView::Error(e) => {
                            crate::applog!(
                                "player: error from {:?}: {} ({:?})",
                                e.src().map(|s| s.path_string()),
                                e.error(),
                                e.debug()
                            );
                            on_event(Event::Error(e.error().to_string()));
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
            .map_err(|e| format!("cannot watch pipeline bus: {e}"))?;

        Ok(Player {
            playbin,
            paintable,
            seek,
            duration,
            _bus_watch: bus_watch,
        })
    }

    pub fn paintable(&self) -> gdk::Paintable {
        self.paintable.clone()
    }

    /// Start playing `path` from the beginning, replacing any current
    /// video. The pipeline object is reused; only its state cycles.
    pub fn play(&self, path: &Path) -> Result<(), String> {
        let uri = glib::filename_to_uri(path, None)
            .map_err(|e| format!("cannot build uri for {}: {e}", path.display()))?;
        let _ = self.playbin.set_state(gst::State::Null);
        self.forget_stream();
        self.playbin.set_property("uri", uri.as_str());
        self.playbin
            .set_state(gst::State::Playing)
            .map_err(|e| format!("cannot start playback of {}: {e}", path.display()))?;
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
        *self.seek.borrow_mut() = SeekState::default();
        self.duration.set(None);
    }

    /// Toggle pause; returns true when now playing.
    pub fn toggle_pause(&self) -> bool {
        let (_, current, pending) = self.playbin.state(gst::ClockTime::ZERO);
        let target = if pending == gst::State::VoidPending {
            current
        } else {
            pending
        };
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
    use super::{Instant, SEEK_SETTLE, SeekState};

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
}
