//! Owns focused playback state, bounded seeking, rate requests, resume sequencing, and generations.

use std::cell::RefCell;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use gstreamer as gst;
use gstreamer::prelude::*;

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
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ErrorContext {
    generation: u64,
    external: Option<PathBuf>,
}

impl FocusedPlayback {
    pub(super) fn reset(&mut self) {
        let generation = self.generation.wrapping_add(1);
        *self = Self {
            generation,
            ..Self::default()
        };
    }

    pub(super) fn start_video(&mut self, path: &Path) {
        self.reset();
        self.current_video = Some(path.to_path_buf());
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
    ) {
        self.forget_timing();
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

    pub(super) fn resume_point(&self) -> (f64, f64, bool) {
        self.resume
            .as_ref()
            .map_or((0.0, self.playback_rate, true), |state| {
                (state.position, state.rate, state.play_after_seek)
            })
    }

    pub(super) fn begin_error(&mut self, external: Option<PathBuf>) -> Option<ErrorContext> {
        if self.error_pending {
            return None;
        }
        self.error_pending = true;
        Some(self.context(external))
    }

    pub(super) fn context(&self, external: Option<PathBuf>) -> ErrorContext {
        ErrorContext {
            generation: self.generation,
            external,
        }
    }

    pub(super) fn error_is_current(&self, context: &ErrorContext, external: Option<&Path>) -> bool {
        self.generation == context.generation && external == context.external.as_deref()
    }

    pub(super) fn finish_error(&mut self, context: &ErrorContext) {
        if self.generation == context.generation {
            self.error_pending = false;
        }
    }
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
        playback.start_video(std::path::Path::new("movie.mkv"));
        playback.prepare_subtitle_rebuild(42.5, 1.5, true);

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
        playback.start_video(std::path::Path::new("movie.mkv"));
        assert!(!playback.playing);

        playback.playback_started();
        assert!(playback.playing);
    }

    #[test]
    fn focused_playback_trace_coalesces_queued_seeks_and_speed_changes() {
        let mut playback = FocusedPlayback::default();
        playback.start_video(std::path::Path::new("movie.mkv"));

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
        playback.start_video(video);
        let stale = playback.begin_error(Some("movie.srt".into())).unwrap();

        // Generation identity matters because path and sidecar can be equal
        // after navigation or an explicit reopen.
        playback.start_video(video);
        assert!(!playback.error_is_current(&stale, Some(std::path::Path::new("movie.srt"))));
        let current = playback.begin_error(Some("movie.srt".into())).unwrap();
        playback.finish_error(&stale);
        assert!(playback.begin_error(Some("movie.srt".into())).is_none());
        playback.finish_error(&current);
        assert!(playback.begin_error(Some("movie.srt".into())).is_some());
    }

    #[test]
    fn a_lost_seek_stops_blocking_and_stops_being_reported() {
        let mut state = SeekState::default();
        issued(&mut state, request(12.0, 1.0), SEEK_SETTLE);
        assert_eq!(state.pending(), None);
        assert!(state.request(request(20.0, 1.0)));
    }
}
