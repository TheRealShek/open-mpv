//! Bounded folder and subtitle scans. Each queue retains one running job and
//! only the newest pending request, even when cancellation is stuck in I/O.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub(super) struct ScanJob<T> {
    pub request: T,
    pub cancelled: Arc<AtomicBool>,
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
}
