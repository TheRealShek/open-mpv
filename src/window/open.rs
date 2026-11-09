//! Bounded folder scans, subtitle discovery and attachment validation. Each
//! queue retains one running job and only the newest pending request, even
//! when cancellation is stuck in I/O.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub(super) struct ScanJob<T> {
    pub request: T,
    pub cancelled: Arc<AtomicBool>,
}

impl<T: Send + 'static> ScanJob<T> {
    /// Keep blocking filesystem calls on GIO's pool. The queue still owns the
    /// active slot until this future settles, including after cancellation.
    pub(super) async fn run<R: Send + 'static>(
        self,
        work: impl FnOnce(T, &AtomicBool) -> R + Send + 'static,
    ) -> std::thread::Result<R> {
        gtk4::gio::spawn_blocking(move || work(self.request, &self.cancelled)).await
    }
}

pub(super) struct ScanQueue<T> {
    active: Option<Arc<AtomicBool>>,
    pending: Option<T>,
}

impl<T> Default for ScanQueue<T> {
    fn default() -> Self {
        Self {
            active: None,
            pending: None,
        }
    }
}

impl<T> ScanQueue<T> {
    pub(super) fn request(&mut self, request: T) -> Option<ScanJob<T>> {
        if let Some(active) = &self.active {
            active.store(true, Ordering::Relaxed);
            self.pending = Some(request);
            None
        } else {
            Some(self.start(request))
        }
    }

    fn start(&mut self, request: T) -> ScanJob<T> {
        let cancelled = Arc::new(AtomicBool::new(false));
        self.active = Some(cancelled.clone());
        ScanJob { request, cancelled }
    }

    /// Called only after the running worker finishes, including panic results.
    pub(super) fn finish(&mut self) -> (bool, Option<ScanJob<T>>) {
        let current = self
            .active
            .take()
            .is_some_and(|flag| !flag.load(Ordering::Relaxed));
        let next = self.pending.take().map(|request| self.start(request));
        (current, next)
    }

    #[cfg(test)]
    pub(super) fn is_idle(&self) -> bool {
        self.active.is_none() && self.pending.is_none()
    }

    pub(super) fn cancel_all(&mut self) {
        if let Some(active) = &self.active {
            active.store(true, Ordering::Relaxed);
        }
        self.pending = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slow_worker_bounds_rapid_requests_and_rejects_stale_completion() {
        let mut queue = ScanQueue::default();
        let first = queue.request(0).unwrap();
        for request in 1..10_000 {
            assert!(queue.request(request).is_none());
        }
        assert!(first.cancelled.load(Ordering::Relaxed));
        let (current, next) = queue.finish();
        assert!(!current);
        assert_eq!(next.unwrap().request, 9_999);
        let (current, next) = queue.finish();
        assert!(current);
        assert!(next.is_none());
    }

    #[test]
    fn cancellation_retains_worker_slot_and_discards_pending_request() {
        let mut queue = ScanQueue::default();
        let first = queue.request(0).unwrap();
        assert!(queue.request(1).is_none());
        queue.cancel_all();
        assert!(first.cancelled.load(Ordering::Relaxed));
        assert!(queue.request(2).is_none());
        let (current, next) = queue.finish();
        assert!(!current);
        assert_eq!(next.unwrap().request, 2);
        queue.cancel_all();
        let (current, next) = queue.finish();
        assert!(!current);
        assert!(next.is_none());
    }

    #[test]
    fn blocked_scan_keeps_main_context_responsive() {
        use gtk4::{gio, glib};
        use std::time::{Duration, Instant};
        let context = glib::MainContext::new();
        context.with_thread_default(|| context.block_on(async {
            let (release, gate) = std::sync::mpsc::channel();
            let mut queue = ScanQueue::default();
            let job = queue.request(0).unwrap();
            let worker = gio::spawn_blocking(move || {
                gate.recv_timeout(Duration::from_secs(5)).unwrap();
                job.cancelled.load(Ordering::Relaxed)
            });
            let start = Instant::now();
            let mut last = start;
            let mut max_gap = Duration::ZERO;
            for request in 1..=50 {
                glib::timeout_future(Duration::from_millis(2)).await;
                let now = Instant::now();
                max_gap = max_gap.max(now.duration_since(last));
                last = now;
                assert!(queue.request(request).is_none());
            }
            release.send(()).unwrap();
            assert!(worker.await.unwrap());
            let (current, next) = queue.finish();
            assert!(!current);
            assert_eq!(next.unwrap().request, 50);
            eprintln!("blocked filesystem simulation: 50 main-context ticks in {:?}, max gap {max_gap:?}", start.elapsed());
        })).unwrap();
    }

    #[test]
    fn delayed_subtitle_validation_rejects_old_successes_and_errors() {
        use super::super::SubtitleRequest;
        use crate::player::ValidatedSubtitle;
        use gtk4::glib;
        use std::path::PathBuf;
        use std::time::Duration;

        let context = glib::MainContext::new();
        context
            .with_thread_default(|| {
                context.block_on(async {
                    let dir = tempfile::tempdir().unwrap();
                    let path = dir.path().join("valid.srt");
                    std::fs::write(&path, "subtitle").unwrap();
                    // Cover valid results and failures after navigation away, return
                    // to the same path, video-to-image, and close.
                    for readable in [true, false] {
                        for (generation, video, shutdown) in [
                            (7, Some("/video.mp4"), false),
                            (8, Some("/other.mp4"), false),
                            (9, Some("/video.mp4"), false),
                            (8, None, false),
                            (7, Some("/video.mp4"), true),
                        ] {
                            let request = SubtitleRequest {
                                path: if readable {
                                    path.clone()
                                } else {
                                    dir.path().join("missing.srt")
                                },
                                video: PathBuf::from("/video.mp4"),
                                generation: 7,
                            };
                            let mut queue = ScanQueue::default();
                            let job = queue.request(request.clone()).unwrap();
                            let (release, gate) = std::sync::mpsc::channel();
                            let gtk_thread = std::thread::current().id();
                            let worker = job.run(move |request, cancelled| {
                                assert_ne!(std::thread::current().id(), gtk_thread);
                                gate.recv_timeout(Duration::from_secs(5)).unwrap();
                                ValidatedSubtitle::open(&request.path, cancelled)
                            });
                            // Poll the same worker future used by attachment while
                            // the main context dispatches a timer to release its I/O.
                            let heartbeat = glib::spawn_future_local(async move {
                                glib::timeout_future(Duration::from_millis(5)).await;
                                release.send(()).unwrap();
                            });
                            let result = worker.await.unwrap();
                            heartbeat.await.unwrap();
                            assert_eq!(result.is_ok(), readable);
                            let (current, next) = queue.finish();
                            assert!(current);
                            assert!(next.is_none());
                            let accepted = request.is_current(
                                generation,
                                video.map(std::path::Path::new),
                                shutdown,
                            );
                            assert_eq!(accepted, generation == 7 && !shutdown);
                        }
                    }
                })
            })
            .unwrap();
    }

    #[test]
    fn cancelled_subtitle_worker_keeps_its_slot_until_settled() {
        use crate::player::ValidatedSubtitle;
        use gtk4::glib;
        use std::time::Duration;

        let context = glib::MainContext::new();
        context
            .with_thread_default(|| {
                context.block_on(async {
                    let dir = tempfile::tempdir().unwrap();
                    let path = dir.path().join("valid.srt");
                    std::fs::write(&path, "subtitle").unwrap();
                    let mut queue = ScanQueue::default();
                    let job = queue.request(path.clone()).unwrap();
                    let (release, gate) = std::sync::mpsc::channel();
                    let worker = job.run(move |path, cancelled| {
                        gate.recv_timeout(Duration::from_secs(5)).unwrap();
                        ValidatedSubtitle::open(&path, cancelled)
                    });
                    for _ in 0..10_000 {
                        assert!(queue.request(path.clone()).is_none());
                    }
                    // Navigation/shutdown drops the pending request but never frees
                    // an occupied worker slot early, even after repeated cancellation.
                    queue.cancel_all();
                    queue.cancel_all();
                    release.send(()).unwrap();
                    assert_eq!(
                        worker.await.unwrap().unwrap_err().kind(),
                        std::io::ErrorKind::Interrupted
                    );
                    let (current, next) = queue.finish();
                    assert!(!current);
                    assert!(next.is_none());
                    assert!(queue.request(path).is_some());
                })
            })
            .unwrap();
    }
}
