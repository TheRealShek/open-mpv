//! Playback gate for one image animation. One frame loop waits here without polling.
use std::cell::{Cell, RefCell};
use std::future::poll_fn;
use std::task::{Poll, Waker};

#[derive(Default)]
pub(super) struct Playback {
    paused: Cell<bool>,
    suspended: Cell<bool>,
    stopped: Cell<bool>,
    waiter: RefCell<Option<Waker>>,
}

impl Playback {
    pub fn paused(&self) -> bool {
        self.paused.get()
    }

    pub fn toggle(&self) {
        self.paused.set(!self.paused.get());
        self.wake();
    }

    pub fn suspend(&self, suspended: bool) {
        self.suspended.set(suspended);
        self.wake();
    }

    pub fn stop(&self) {
        self.stopped.set(true);
        self.wake();
    }

    fn wake(&self) {
        let waiter = self.waiter.borrow_mut().take();
        if let Some(waiter) = waiter {
            waiter.wake();
        }
    }

    /// False ends this loop; true permits decoding or presenting its next frame.
    pub async fn ready(&self) -> bool {
        poll_fn(|cx| {
            if self.stopped.get() {
                Poll::Ready(false)
            } else if self.paused.get() || self.suspended.get() {
                *self.waiter.borrow_mut() = Some(cx.waker().clone());
                Poll::Pending
            } else {
                Poll::Ready(true)
            }
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::Future;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use std::task::{Context, Wake};

    #[derive(Default)]
    struct Counter(AtomicUsize);
    impl Wake for Counter {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[test]
    fn user_pause_survives_suspension_and_waits_for_resume() {
        let playback = Playback::default();
        let counter = Arc::new(Counter::default());
        let waker = Waker::from(counter.clone());
        let mut cx = Context::from_waker(&waker);
        playback.toggle();
        let mut ready = std::pin::pin!(playback.ready());
        assert_eq!(ready.as_mut().poll(&mut cx), Poll::Pending);
        assert_eq!(counter.0.load(Ordering::Relaxed), 0);
        playback.suspend(true);
        assert_eq!(ready.as_mut().poll(&mut cx), Poll::Pending);
        playback.suspend(false);
        assert!(playback.paused());
        assert_eq!(ready.as_mut().poll(&mut cx), Poll::Pending);
        playback.toggle();
        assert_eq!(ready.as_mut().poll(&mut cx), Poll::Ready(true));
    }

    #[test]
    fn suspension_alone_resumes_and_stop_wakes_a_paused_loop() {
        let playback = Playback::default();
        let counter = Arc::new(Counter::default());
        let waker = Waker::from(counter.clone());
        let mut cx = Context::from_waker(&waker);
        playback.suspend(true);
        let mut ready = std::pin::pin!(playback.ready());
        assert_eq!(ready.as_mut().poll(&mut cx), Poll::Pending);
        playback.suspend(false);
        assert!(!playback.paused());
        assert_eq!(ready.as_mut().poll(&mut cx), Poll::Ready(true));
        playback.toggle();
        let mut ready = std::pin::pin!(playback.ready());
        assert_eq!(ready.as_mut().poll(&mut cx), Poll::Pending);
        let before = counter.0.load(Ordering::Relaxed);
        playback.stop();
        assert_eq!(counter.0.load(Ordering::Relaxed), before + 1);
        assert_eq!(ready.as_mut().poll(&mut cx), Poll::Ready(false));
    }
}
