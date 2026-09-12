//! Exercise real Player callers against a GStreamer element that can refuse
//! individual transitions. No codecs, display server, or timing races required.
use super::*;
use gst::subclass::prelude::*;
use std::sync::atomic::{AtomicBool, Ordering};

mod imp {
    use super::*;

    #[derive(Default, glib::Properties)]
    #[properties(wrapper_type = super::TestPipeline)]
    pub struct TestPipeline {
        #[property(get, set, nullable)]
        uri: Mutex<Option<String>>,
        #[property(get, set, nullable)]
        suburi: Mutex<Option<String>>,
        pub refuse: Mutex<Option<gst::StateChange>>,
        pub refuse_seek: AtomicBool,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for TestPipeline {
        const NAME: &'static str = "OpenMpvFailureTestPipeline";
        type Type = super::TestPipeline;
        type ParentType = gst::Element;
    }
    #[glib::derived_properties]
    impl ObjectImpl for TestPipeline {}
    impl GstObjectImpl for TestPipeline {}
    impl ElementImpl for TestPipeline {
        fn change_state(
            &self,
            transition: gst::StateChange,
        ) -> Result<gst::StateChangeSuccess, gst::StateChangeError> {
            if *self.refuse.lock().unwrap() == Some(transition) {
                return Err(gst::StateChangeError);
            }
            self.parent_change_state(transition)
        }
        fn send_event(&self, event: gst::Event) -> bool {
            matches!(event.view(), gst::EventView::Seek(_))
                && !self.refuse_seek.load(Ordering::Relaxed)
        }
    }
}
glib::wrapper! {
    pub struct TestPipeline(ObjectSubclass<imp::TestPipeline>) @extends gst::Element, gst::Object;
}

impl TestPipeline {
    pub(crate) fn refuse_transition(&self, transition: Option<gst::StateChange>) {
        *self.imp().refuse.lock().unwrap() = transition;
    }
}

pub(crate) fn with_player(test: impl FnOnce(Player, TestPipeline)) {
    gst::init().unwrap();
    let context = glib::MainContext::new();
    let _guard = context.acquire().unwrap();
    context
        .with_thread_default(|| {
            let pipeline: TestPipeline = glib::Object::new();
            let bus = gst::Bus::new();
            pipeline.set_bus(Some(&bus));
            let watch = bus
                .add_watch_local(|_, _| glib::ControlFlow::Continue)
                .unwrap();
            let texture = gdk::MemoryTexture::new(
                1,
                1,
                gdk::MemoryFormat::R8g8b8a8,
                &glib::Bytes::from_static(&[0, 0, 0, 255]),
                4,
            );
            let player = Player {
                playbin: pipeline.clone().upcast(),
                seek_target: pipeline.clone().upcast(),
                paintable: texture.upcast(),
                playback: Rc::new(RefCell::new(FocusedPlayback::default())),
                pitch_preserving: true,
                subtitles_default_on: Cell::new(true),
                decoder_fallback: Arc::new(Mutex::new(DecoderFallback::default())),
                _bus_watch: watch,
            };
            test(player, pipeline);
        })
        .unwrap();
}

#[test]
fn refused_pause_and_resume_do_not_commit_state() {
    with_player(|player, pipeline| {
        player
            .play(Path::new("/tmp/open-mpv-test.mp4"), None)
            .unwrap();
        *pipeline.imp().refuse.lock().unwrap() = Some(gst::StateChange::PlayingToPaused);
        assert!(player.toggle_pause().is_err());
        assert!(player.is_playing());
        *pipeline.imp().refuse.lock().unwrap() = None;
        assert!(!player.toggle_pause().unwrap());
        *pipeline.imp().refuse.lock().unwrap() = Some(gst::StateChange::PausedToPlaying);
        assert!(player.toggle_pause().is_err());
        assert!(!player.is_playing());
        *pipeline.imp().refuse.lock().unwrap() = None;
        assert!(player.toggle_pause().unwrap());
    });
}

#[test]
fn failed_teardown_does_not_replace_uri_and_stop_reports_failure() {
    with_player(|player, pipeline| {
        player.play(Path::new("/tmp/old.mp4"), None).unwrap();
        let uri = pipeline.uri();
        *pipeline.imp().refuse.lock().unwrap() = Some(gst::StateChange::ReadyToNull);
        assert!(player.play(Path::new("/tmp/new.mp4"), None).is_err());
        assert_eq!(pipeline.uri(), uri);
        assert!(!player.is_playing());
        assert!(player.stop().is_err());
        assert!(player.playback.borrow().current_video().is_none());
        *pipeline.imp().refuse.lock().unwrap() = None;
        player.play(Path::new("/tmp/new.mp4"), None).unwrap();
        assert_ne!(pipeline.uri(), uri);
        player.stop().unwrap();
        assert!(!player.is_playing());
    });
}

#[test]
fn refused_rewind_seek_and_resume_never_report_playing() {
    with_player(|player, pipeline| {
        player.play(Path::new("/tmp/movie.mp4"), None).unwrap();
        pipeline.imp().refuse_seek.store(true, Ordering::Relaxed);
        assert!(matches!(player.rewind(), Err(PlayerError::SeekRefused)));
        assert!(!player.is_playing());
        pipeline.imp().refuse_seek.store(false, Ordering::Relaxed);
        pipeline.set_state(gst::State::Paused).unwrap();
        *pipeline.imp().refuse.lock().unwrap() = Some(gst::StateChange::PausedToPlaying);
        assert!(player.rewind().is_err());
        assert!(!player.is_playing());
        *pipeline.imp().refuse.lock().unwrap() = None;
        player.rewind().unwrap();
        assert!(player.is_playing());
    });
}

#[test]
fn subtitle_attach_and_recovery_require_completed_teardown() {
    with_player(|player, pipeline| {
        let subtitle = tempfile::Builder::new().suffix(".srt").tempfile().unwrap();
        player.play(Path::new("/tmp/movie.mp4"), None).unwrap();
        *pipeline.imp().refuse.lock().unwrap() = Some(gst::StateChange::ReadyToNull);
        assert!(player.attach_subtitle(subtitle.path()).is_err());
        assert!(pipeline.suburi().is_none());
        *pipeline.imp().refuse.lock().unwrap() = None;
        player.play(Path::new("/tmp/movie.mp4"), None).unwrap();
        player.attach_subtitle(subtitle.path()).unwrap();
        let suburi = pipeline.suburi();
        *pipeline.imp().refuse.lock().unwrap() = Some(gst::StateChange::ReadyToNull);
        assert!(!recover_without_external(&player.playbin, &player.playback));
        assert_eq!(pipeline.suburi(), suburi);
        assert!(!player.is_playing());
        *pipeline.imp().refuse.lock().unwrap() = None;
    });
}

#[test]
fn failed_subtitle_restart_and_recovery_clear_pending_resume() {
    with_player(|player, pipeline| {
        let subtitle = tempfile::Builder::new().suffix(".srt").tempfile().unwrap();
        player.play(Path::new("/tmp/movie.mp4"), None).unwrap();
        *pipeline.imp().refuse.lock().unwrap() = Some(gst::StateChange::PausedToPlaying);
        assert!(player.attach_subtitle(subtitle.path()).is_err());
        assert!(!player.is_playing());
        assert_eq!(
            player.playback.borrow_mut().observe_async_done(),
            ResumeAction::None
        );
        assert_eq!(pipeline.current_state(), gst::State::Null);
        *pipeline.imp().refuse.lock().unwrap() = None;
    });
}

#[test]
fn drop_attempts_cleanup_even_after_a_failure() {
    with_player(|player, pipeline| {
        player.play(Path::new("/tmp/movie.mp4"), None).unwrap();
        *pipeline.imp().refuse.lock().unwrap() = Some(gst::StateChange::ReadyToNull);
        assert!(player.stop().is_err());
        *pipeline.imp().refuse.lock().unwrap() = None;
        drop(player);
        assert_eq!(pipeline.current_state(), gst::State::Null);
    });
}

#[test]
fn both_subtitle_completion_paths_report_refusal_and_reject_stale_errors() {
    let context = glib::MainContext::default();
    let _guard = context.acquire().unwrap();
    for position in [0.0, 20.0] {
        for playing in [false, true] {
            for supersede in [false, true] {
                with_player(|player, pipeline| {
                    player.play(Path::new("/tmp/movie.mp4"), None).unwrap();
                    if playing {
                        pipeline.set_state(gst::State::Paused).unwrap();
                    }
                    player.playback.borrow_mut().set_playing(!playing);
                    player
                        .playback
                        .borrow_mut()
                        .prepare_subtitle_rebuild(position, 1.0, playing, None);
                    *pipeline.imp().refuse.lock().unwrap() = Some(if playing {
                        gst::StateChange::PausedToPlaying
                    } else {
                        gst::StateChange::PlayingToPaused
                    });
                    let errors = Rc::new(Cell::new(0));
                    let on_event: Rc<dyn Fn(Event)> = Rc::new({
                        let errors = errors.clone();
                        move |event| {
                            assert!(matches!(event, Event::StateError(_)));
                            errors.set(errors.get() + 1);
                        }
                    });
                    finish_async(
                        &player.playbin,
                        &player.seek_target,
                        &player.playback,
                        &on_event,
                    );
                    if position > 0.0 {
                        finish_async(
                            &player.playbin,
                            &player.seek_target,
                            &player.playback,
                            &on_event,
                        );
                    }
                    assert_eq!(player.is_playing(), !playing);
                    assert_eq!(
                        errors.get(),
                        0,
                        "must leave the bus callback before cleanup"
                    );
                    if supersede {
                        player.playback.borrow_mut().reset(true);
                    }
                    while context.pending() {
                        context.iteration(false);
                    }
                    assert_eq!(errors.get(), usize::from(!supersede));
                    *pipeline.imp().refuse.lock().unwrap() = None;
                });
            }
        }
    }
}

#[test]
fn play_attaches_explicit_subtitle() {
    with_player(|player, _pipeline| {
        let video = Path::new("/tmp/movie.mp4");
        let subtitle = PathBuf::from("/tmp/movie.srt");
        player.play(video, Some(subtitle.clone())).unwrap();
        assert_eq!(
            player.playback.borrow().external_subtitle(),
            Some(subtitle.as_path())
        );
    });
}
