//! Video playback (FR-9): one `playbin3` pipeline rendering into a
//! `gtk4paintablesink`, whose `GdkPaintable` the viewer displays like
//! any other paintable.
//!
//! This is the resource-minimal path on this machine: VA-API decoders
//! decode on the iGPU and frames reach GTK as dmabufs — no CPU pixel
//! copies. GStreamer is initialized lazily on the first video so
//! image-only sessions keep their cold-start and footprint (NFR-1.1,
//! NFR-2.1). The pipeline is reused across videos; `stop` drops it to
//! `Null`, freeing decoder state while an image is shown.

use std::path::Path;

use gstreamer as gst;
use gstreamer::prelude::*;

use gtk4::gdk;
use gtk4::glib;

const SEEK_FLAGS: gst::SeekFlags = gst::SeekFlags::FLUSH.union(gst::SeekFlags::KEY_UNIT);
const VOLUME_MAX: f64 = 1.5;

/// Pipeline happenings the window reacts to; delivered on the main loop.
pub enum Event {
    EndOfStream,
    Error(String),
}

pub struct Player {
    playbin: gst::Element,
    paintable: gdk::Paintable,
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

        let bus = playbin.bus().ok_or("playbin has no bus")?;
        let bus_watch = bus
            .add_watch_local(move |_, msg| {
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
                    _ => {}
                }
                glib::ControlFlow::Continue
            })
            .map_err(|e| format!("cannot watch pipeline bus: {e}"))?;

        Ok(Player {
            playbin,
            paintable,
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
        self.playbin.set_property("uri", uri.as_str());
        self.playbin
            .set_state(gst::State::Playing)
            .map_err(|e| format!("cannot start playback of {}: {e}", path.display()))?;
        Ok(())
    }

    /// Drop to `Null`: stops playback and frees decoder state.
    pub fn stop(&self) {
        let _ = self.playbin.set_state(gst::State::Null);
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
            false
        } else {
            let _ = self.playbin.set_state(gst::State::Playing);
            true
        }
    }

    /// Position and duration in seconds, once the pipeline knows them.
    pub fn progress(&self) -> Option<(f64, f64)> {
        let pos = self.playbin.query_position::<gst::ClockTime>()?;
        let dur = self.playbin.query_duration::<gst::ClockTime>()?;
        Some((pos.nseconds() as f64 / 1e9, dur.nseconds() as f64 / 1e9))
    }

    /// Seek by `delta` seconds, clamped to the stream.
    pub fn seek_by(&self, delta: f64) {
        let Some((pos, dur)) = self.progress() else {
            return;
        };
        self.seek_to((pos + delta).clamp(0.0, dur));
    }

    /// Seek to `fraction` (0..1) of the duration.
    pub fn seek_fraction(&self, fraction: f64) {
        let Some((_, dur)) = self.progress() else {
            return;
        };
        self.seek_to(dur * fraction.clamp(0.0, 1.0));
    }

    fn seek_to(&self, secs: f64) {
        let target = gst::ClockTime::from_nseconds((secs.max(0.0) * 1e9) as u64);
        let _ = self.playbin.seek_simple(SEEK_FLAGS, target);
    }

    /// Restart from the beginning (EOS loop, FR-9.3).
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
        muted
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        // NFR-2.2: nothing keeps running once the window is gone.
        self.stop();
    }
}
