//! First-frame work for the current destination and its two neighbors.
//! Cancellation requests do not release slots: only completion does, so a
//! slow-to-cancel decoder cannot cause replacement work to grow without bound.

use std::path::{Path, PathBuf};

use gtk4::gio::{self, prelude::*};

const MAX_ACTIVE: usize = 2;
const MAX_NEIGHBORS: usize = 2;

pub enum Interest<T> {
    Foreground(T),
    Neighbor,
}

struct Request<T> {
    path: PathBuf,
    interest: Interest<T>,
}

pub struct Job {
    pub path: PathBuf,
    pub cancellable: gio::Cancellable,
}

struct Active<T> {
    job: Job,
    interest: Option<Interest<T>>,
}

pub struct Scheduler<T> {
    active: Vec<Active<T>>,
    pending: Vec<Request<T>>,
}

impl<T> Default for Scheduler<T> {
    fn default() -> Self {
        Self {
            active: Vec::new(),
            pending: Vec::new(),
        }
    }
}

impl<T> Scheduler<T> {
    /// Replace demand, promoting an in-flight neighbor without decoding twice.
    /// `foreground` contains the latest presentation token, not the token from
    /// when the job started. A cancelled job is never revived.
    pub fn replace(
        &mut self,
        foreground: Option<(PathBuf, T)>,
        neighbors: impl IntoIterator<Item = PathBuf>,
    ) {
        self.pending.clear();
        if let Some((path, token)) = foreground {
            self.pending.push(Request {
                path,
                interest: Interest::Foreground(token),
            });
        }
        for path in neighbors.into_iter().take(MAX_NEIGHBORS) {
            if !self.pending.iter().any(|request| request.path == path) {
                self.pending.push(Request {
                    path,
                    interest: Interest::Neighbor,
                });
            }
        }
        for active in &mut self.active {
            let desired = self
                .pending
                .iter()
                .position(|request| request.path == active.job.path);
            if !active.job.cancellable.is_cancelled()
                && let Some(index) = desired
            {
                active.interest = Some(self.pending.remove(index).interest);
            } else {
                active.interest = None;
                active.job.cancellable.cancel();
            }
        }
        // A former foreground can become a second neighbor. Preempt that
        // extra speculation so a newly requested foreground is not left
        // waiting for two still-wanted neighbors to decode in full.
        let mut kept_neighbor = false;
        for active in &mut self.active {
            if matches!(active.interest, Some(Interest::Neighbor)) {
                if kept_neighbor {
                    self.pending.push(Request {
                        path: active.job.path.clone(),
                        interest: Interest::Neighbor,
                    });
                    active.interest = None;
                    active.job.cancellable.cancel();
                }
                kept_neighbor = true;
            }
        }
    }

    /// Start foreground first. At most one speculative job runs, leaving room
    /// for a new foreground request even when the displayed image was cached.
    pub fn start(&mut self) -> Option<Job> {
        if self.active.len() >= MAX_ACTIVE {
            return None;
        }
        let index = self.pending.iter().position(|request| {
            !self
                .active
                .iter()
                .any(|active| active.job.path == request.path)
                && match request.interest {
                    Interest::Foreground(_) => true,
                    Interest::Neighbor => {
                        !self
                            .pending
                            .iter()
                            .any(|request| matches!(request.interest, Interest::Foreground(_)))
                            && !self
                                .active
                                .iter()
                                .any(|active| matches!(active.interest, Some(Interest::Neighbor)))
                    }
                }
        })?;
        let request = self.pending.remove(index);
        let cancellable = gio::Cancellable::new();
        let job = Job {
            path: request.path.clone(),
            cancellable: cancellable.clone(),
        };
        self.active.push(Active {
            job: Job {
                path: request.path,
                cancellable,
            },
            interest: Some(request.interest),
        });
        crate::applog!(
            "loader: start {} (active={}, queued={})",
            job.path.display(),
            self.active.len(),
            self.pending.len()
        );
        Some(job)
    }

    /// Only the latest interest receives the result. Obsolete successes and
    /// cancellation errors are discarded, including after file invalidation.
    pub fn finish(&mut self, path: &Path) -> Option<Interest<T>> {
        let index = self
            .active
            .iter()
            .position(|active| active.job.path == path)?;
        let active = self.active.remove(index);
        crate::applog!(
            "loader: finish {} (active={}, queued={})",
            path.display(),
            self.active.len(),
            self.pending.len()
        );
        active.interest
    }

    pub fn invalidate(&mut self, path: &Path) {
        // A save may finish after the user has navigated away and back, or
        // after its rename notification already requested the new contents.
        // Keep that latest foreground demand while replacing its decoder.
        self.pending.retain(|request| {
            request.path != path || matches!(request.interest, Interest::Foreground(_))
        });
        for active in &mut self.active {
            if active.job.path == path {
                if let Some(Interest::Foreground(token)) = active.interest.take() {
                    self.pending.insert(
                        0,
                        Request {
                            path: active.job.path.clone(),
                            interest: Interest::Foreground(token),
                        },
                    );
                }
                active.job.cancellable.cancel();
            }
        }
    }

    pub fn cancel_all(&mut self) {
        self.pending.clear();
        for active in &mut self.active {
            active.interest = None;
            active.job.cancellable.cancel();
        }
    }
}

impl<T> Drop for Scheduler<T> {
    fn drop(&mut self) {
        self.cancel_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn select(scheduler: &mut Scheduler<u64>, path: &str, token: u64, neighbors: &[&str]) {
        scheduler.replace(
            Some((path.into(), token)),
            neighbors.iter().map(PathBuf::from),
        );
    }

    #[test]
    fn sustained_navigation_waits_for_cancelled_jobs_and_keeps_only_latest_demand() {
        let mut scheduler = Scheduler::default();
        select(&mut scheduler, "0", 0, &["1", "2"]);
        let first = scheduler.start().unwrap();
        let second = scheduler.start().unwrap();
        for i in 1..1000 {
            select(&mut scheduler, &i.to_string(), i, &["left", "right"]);
            assert!(scheduler.start().is_none());
            assert_eq!(scheduler.active.len(), 2);
            assert!(scheduler.pending.len() <= 3);
        }
        assert!(first.cancellable.is_cancelled());
        assert!(second.cancellable.is_cancelled());
        assert!(scheduler.finish(&second.path).is_none());
        let latest = scheduler.start().unwrap();
        assert_eq!(latest.path, Path::new("999"));
        assert!(matches!(
            scheduler.finish(&latest.path),
            Some(Interest::Foreground(999))
        ));
        assert!(scheduler.finish(&first.path).is_none());
    }

    #[test]
    fn neighbor_promotion_updates_token_and_failure_has_one_foreground_recipient() {
        let mut scheduler = Scheduler::default();
        select(&mut scheduler, "a", 1, &["b", "b"]);
        let a = scheduler.start().unwrap();
        let b = scheduler.start().unwrap();
        select(&mut scheduler, "b", 2, &["a", "b"]);
        select(&mut scheduler, "b", 3, &["a"]);
        assert!(!b.cancellable.is_cancelled());
        assert!(scheduler.start().is_none());
        // Completion routing is identical for success and decode failure.
        assert!(matches!(
            scheduler.finish(&b.path),
            Some(Interest::Foreground(3))
        ));
        assert!(scheduler.finish(&b.path).is_none());
        assert!(matches!(
            scheduler.finish(&a.path),
            Some(Interest::Neighbor)
        ));
        assert!(scheduler.start().is_none());
    }

    #[test]
    fn cached_foreground_reserves_capacity_for_next_miss() {
        let mut scheduler = Scheduler::default();
        scheduler.replace(None, ["a".into(), "b".into()]);
        let a = scheduler.start().unwrap();
        assert!(scheduler.start().is_none());
        select(&mut scheduler, "c", 1, &["a", "b"]);
        assert_eq!(scheduler.start().unwrap().path, Path::new("c"));
        assert!(!a.cancellable.is_cancelled());
    }

    #[test]
    fn demoted_foreground_does_not_leave_two_speculations_ahead_of_current_image() {
        let mut scheduler = Scheduler::default();
        select(&mut scheduler, "a", 1, &["b"]);
        let a = scheduler.start().unwrap();
        let b = scheduler.start().unwrap();
        select(&mut scheduler, "c", 2, &["a", "b"]);
        assert!(!a.cancellable.is_cancelled());
        assert!(b.cancellable.is_cancelled());
        assert!(scheduler.finish(&b.path).is_none());
        assert_eq!(scheduler.start().unwrap().path, Path::new("c"));
    }

    #[test]
    fn invalidation_and_return_to_cancelled_path_wait_for_old_completion() {
        let mut scheduler = Scheduler::default();
        select(&mut scheduler, "a", 1, &["b"]);
        let old = scheduler.start().unwrap();
        scheduler.invalidate(Path::new("a"));
        select(&mut scheduler, "a", 2, &["b"]);
        assert!(old.cancellable.is_cancelled());
        assert!(
            scheduler.start().is_none(),
            "pending foreground blocks speculation"
        );
        assert!(scheduler.finish(&old.path).is_none());
        let fresh = scheduler.start().unwrap();
        assert!(!fresh.cancellable.is_cancelled());
        assert!(matches!(
            scheduler.finish(&fresh.path),
            Some(Interest::Foreground(2))
        ));
    }

    #[test]
    fn late_save_invalidation_retries_latest_foreground_without_reselecting_it() {
        let mut scheduler = Scheduler::default();
        select(&mut scheduler, "a", 3, &[]);
        let old = scheduler.start().unwrap();
        // A save from an earlier destination generation has finished. Its
        // caller cannot select A again, but current generation 3 still needs A.
        scheduler.invalidate(Path::new("a"));
        scheduler.invalidate(Path::new("a"));
        assert!(scheduler.start().is_none());
        assert!(scheduler.finish(&old.path).is_none());
        let fresh = scheduler.start().unwrap();
        assert_eq!(fresh.path, Path::new("a"));
        assert!(matches!(
            scheduler.finish(&fresh.path),
            Some(Interest::Foreground(3))
        ));
        assert!(scheduler.start().is_none());
    }

    #[test]
    fn clear_and_drop_cancel_active_and_discard_queued_work() {
        let mut scheduler = Scheduler::default();
        select(&mut scheduler, "a", 1, &["b", "c"]);
        let a = scheduler.start().unwrap();
        let b = scheduler.start().unwrap();
        scheduler.cancel_all();
        assert!(a.cancellable.is_cancelled());
        assert!(b.cancellable.is_cancelled());
        assert!(scheduler.finish(&a.path).is_none());
        assert!(scheduler.finish(&b.path).is_none());
        assert!(scheduler.start().is_none());
        select(&mut scheduler, "d", 2, &[]);
        let d = scheduler.start().unwrap();
        drop(scheduler);
        assert!(d.cancellable.is_cancelled());
    }

    #[test]
    fn oversized_input_and_duplicate_paths_cannot_grow_queue() {
        let mut scheduler = Scheduler::default();
        scheduler.replace(
            Some((PathBuf::from("a"), 1)),
            (0..1000).map(|i| PathBuf::from(i.to_string())),
        );
        assert_eq!(scheduler.pending.len(), 3);
        select(&mut scheduler, "a", 2, &["a", "a"]);
        assert_eq!(scheduler.pending.len(), 1);
    }
}
