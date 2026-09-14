//! Desktop integration checks for first-frame scheduling.

use super::*;

#[test]
#[ignore = "requires a desktop session"]
fn rapid_navigation_bounds_slow_decodes() {
    gtk::init().unwrap();
    let gtk_app = gtk::Application::builder()
        .application_id("io.github.TheRealShek.OpenMpv.DecodeTest")
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    gtk_app.register(gio::Cancellable::NONE).unwrap();
    let app = App::new(&gtk_app, Config::default());
    let dir = tempfile::tempdir().unwrap();
    for i in 0..12 {
        std::fs::write(dir.path().join(format!("{i:02}.png")), []).unwrap();
    }
    let gate = gio::Cancellable::new();
    loader::SLOW_TEST.with(|paths| *paths.borrow_mut() = Some((Vec::new(), gate.clone())));
    app.install_folder(Folder::scan(dir.path(), app.cfg.sort).unwrap());
    for i in 0..10 {
        app.show_index(i, Arrival::Direct);
        let context = glib::MainContext::default();
        for _ in 0..100 {
            if !context.pending() {
                break;
            }
            context.iteration(false);
        }
    }
    let paths = loader::SLOW_TEST.with(|paths| paths.borrow().as_ref().unwrap().0.clone());
    app.shutdown();
    loader::SLOW_TEST.with(|paths| paths.borrow_mut().take());
    gate.cancel();
    glib::MainContext::default().block_on(glib::timeout_future(Duration::from_millis(100)));
    assert_eq!(paths.len(), 2, "slow decodes: {paths:?}");
}

/// Run separately from other GTK tests. Fixture generation and /proc sampling
/// are external so this exercises the actual Viewer without test decode gates.
#[test]
#[ignore = "requires a desktop session and OPEN_MPV_STRESS_DIR image fixtures"]
fn sustained_real_decodes() {
    gtk::init().unwrap();
    let gtk_app = gtk::Application::builder()
        .application_id("io.github.TheRealShek.OpenMpv.DecodeStress")
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    gtk_app.register(gio::Cancellable::NONE).unwrap();
    let app = App::new(&gtk_app, Config::default());
    let directory = std::env::var_os("OPEN_MPV_STRESS_DIR")
        .expect("set OPEN_MPV_STRESS_DIR to an image fixture folder");
    let folder = Folder::scan(Path::new(&directory), app.cfg.sort).unwrap();
    let count = folder.len();
    assert!(count >= 12, "provide at least 12 images");
    app.install_folder(folder);
    let context = glib::MainContext::default();
    for cycle in 0..6 {
        crate::applog!(
            "stress: navigation cycle {cycle}, pid={}",
            std::process::id()
        );
        for i in 0..200 {
            app.show_index(i % count, Arrival::Direct);
            context.block_on(glib::timeout_future(Duration::from_millis(25)));
        }
        crate::applog!("stress: settling cycle {cycle}");
        context.block_on(glib::timeout_future(Duration::from_secs(3)));
        assert!(matches!(*app.media.borrow(), MediaState::Image { .. }));
    }
    app.clear_media();
    app.shutdown();
    context.block_on(glib::timeout_future(Duration::from_millis(250)));
}

/// Exercise actual folder workers and stale open delivery on the GTK context.
#[test]
#[ignore = "requires a GNOME/Wayland session; run separately with --ignored --exact"]
fn large_folder_open_stays_responsive_and_latest_request_wins() {
    gtk::init().unwrap();
    let gtk_app = gtk::Application::builder()
        .application_id("io.github.TheRealShek.OpenMpv.OpenTest")
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    gtk_app.register(gio::Cancellable::NONE).unwrap();
    let mut cfg = Config::default();
    cfg.sort.order = config::SortOrder::Date;
    let app = App::new(&gtk_app, cfg);
    let large = tempfile::tempdir().unwrap();
    for i in 0..20_000 {
        std::fs::write(large.path().join(format!("{i:05}.png")), []).unwrap();
    }
    let latest = tempfile::tempdir().unwrap();
    let decode_gate = gio::Cancellable::new();
    loader::SLOW_TEST.with(|paths| *paths.borrow_mut() = Some((Vec::new(), decode_gate.clone())));
    let context = glib::MainContext::default();
    app.open_path(large.path());
    context.block_on(async {
        let start = std::time::Instant::now();
        let mut last = start;
        let mut max_gap = Duration::ZERO;
        let mut ticks = 0;
        while app.navigation.borrow().directory() != Some(large.path()) {
            glib::timeout_future(Duration::from_millis(1)).await;
            let now = std::time::Instant::now();
            max_gap = max_gap.max(now.duration_since(last));
            last = now;
            ticks += 1;
            assert!(start.elapsed() < Duration::from_secs(30));
        }
        eprintln!(
            "20,000 date-sorted media entries: {:?}, {ticks} GTK ticks, max gap {max_gap:?}",
            start.elapsed()
        );
        assert!(ticks > 1);
        app.open_path(large.path());
        for _ in 0..100 {
            app.open_path(latest.path());
        }
        while app.navigation.borrow().directory() != Some(latest.path()) {
            glib::timeout_future(Duration::from_millis(1)).await;
            assert!(start.elapsed() < Duration::from_secs(30));
        }
        assert!(matches!(*app.media.borrow(), MediaState::Error(_)));
        app.shutdown();
        loader::SLOW_TEST.with(|paths| paths.borrow_mut().take());
        decode_gate.cancel();
        glib::timeout_future(Duration::from_millis(100)).await;
    });
}

fn edit_test_app() -> Rc<App> {
    gtk::init().unwrap();
    let gtk_app = gtk::Application::builder()
        .application_id("io.github.TheRealShek.OpenMpv.ExternalEditTest")
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    gtk_app.register(gio::Cancellable::NONE).unwrap();
    App::new(&gtk_app, Config::default())
}

fn test_image(width: i32) -> Rc<Decoded> {
    let stride = usize::try_from(width).unwrap() * 4;
    let texture = gdk::MemoryTexture::new(
        width,
        1,
        gdk::MemoryFormat::R8g8b8a8,
        &glib::Bytes::from_owned(vec![255_u8; stride]),
        stride,
    );
    Rc::new(Decoded::Static {
        texture: texture.into(),
    })
}

fn write_test_image(path: &Path, width: i32) {
    // std::fs::write truncates the existing inode; GDK's path writer replaces
    // atomically, which would miss the in-place-edit regression.
    std::fs::write(path, test_image(width).first_texture().save_to_png_bytes()).unwrap();
}

async fn wait_for_image(app: &App, path: &Path, width: i32) {
    let start = std::time::Instant::now();
    loop {
        if matches!(&*app.media.borrow(), MediaState::Image { path: current, decoded, .. }
            if current == path && decoded.first_texture().width() == width)
        {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "expected image width {width} at {}",
            path.display()
        );
        glib::timeout_future(Duration::from_millis(5)).await;
    }
    assert_eq!(
        app.cache.get(path).unwrap().0.first_texture().width(),
        width
    );
}

#[test]
#[ignore = "requires a GNOME/Wayland session; run separately with --ignored --exact"]
fn external_image_edits_refresh_current_and_cached_contents() {
    let app = edit_test_app();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.png");
    let neighbor = dir.path().join("b.png");
    write_test_image(&path, 2);
    write_test_image(&neighbor, 10);
    app.install_folder(Folder::scan(dir.path(), app.cfg.sort).unwrap());
    app.show_index(0, Arrival::Direct);
    glib::MainContext::default().block_on(async {
        wait_for_image(&app, &path, 2).await;
        write_test_image(&path, 3);
        wait_for_image(&app, &path, 3).await;

        // An atomic replacement must refresh the same logical destination.
        let replacement = dir.path().join("replacement.tmp");
        write_test_image(&replacement, 4);
        std::fs::rename(&replacement, &path).unwrap();
        wait_for_image(&app, &path, 4).await;

        // Edit a cached neighbor without moving the current destination.
        app.show_index(1, Arrival::Direct);
        wait_for_image(&app, &neighbor, 10).await;
        write_test_image(&path, 5);
        glib::timeout_future(Duration::from_millis(250)).await;
        assert_eq!(
            app.navigation.borrow().current_path(),
            Some(neighbor.as_path())
        );
        assert!(!app.cache.contains(&path));
        app.show_index(0, Arrival::Direct);
        wait_for_image(&app, &path, 5).await;

        // Removal selects the remaining item; recreating the path must never
        // restore its old cached pixels or steal the current selection.
        std::fs::remove_file(&path).unwrap();
        wait_for_image(&app, &neighbor, 10).await;
        assert!(!app.cache.contains(&path));
        write_test_image(&path, 6);
        glib::timeout_future(Duration::from_millis(250)).await;
        assert_eq!(
            app.navigation.borrow().current_path(),
            Some(neighbor.as_path())
        );
        let index = app.navigation.borrow().index_of(&path).unwrap();
        app.show_index(index, Arrival::Direct);
        wait_for_image(&app, &path, 6).await;
        app.shutdown();
    });
}

#[test]
#[ignore = "requires a GNOME/Wayland session; run separately with --ignored --exact"]
fn content_events_reject_late_successes_and_coalesce_refresh() {
    let app = edit_test_app();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.png");
    write_test_image(&path, 3);
    app.install_folder(Folder::scan(dir.path(), app.cfg.sort).unwrap());
    // Inject events deterministically; the real monitor path is covered above.
    app.monitor.borrow_mut().take().unwrap().cancel();
    let destination = app.navigation.borrow_mut().select(0).unwrap();
    let set = app.navigation.borrow().set_id().unwrap();
    let file = gio::File::for_path(&path);
    let gate = gio::Cancellable::new();
    loader::SLOW_TEST.with(|paths| *paths.borrow_mut() = Some((Vec::new(), gate.clone())));

    // Deliver a successful old decode through the production completion handler
    // after invalidation, for both foreground and speculative work.
    for foreground in [true, false] {
        let demand = foreground.then(|| (path.clone(), (destination.generation, Arrival::Direct)));
        let neighbors = (!foreground).then(|| path.clone());
        app.decodes.borrow_mut().replace(demand, neighbors);
        let job = app.decodes.borrow_mut().start().unwrap();
        *app.media.borrow_mut() = MediaState::Loading(path.clone());
        app.on_fs_event(set, &file, None, gio::FileMonitorEvent::Changed);
        assert!(job.cancellable.is_cancelled());
        app.on_decode_completed(path.clone(), Ok((test_image(2), "image/png".into())));
        assert!(!app.cache.contains(&path));
        assert!(matches!(*app.media.borrow(), MediaState::Loading(_)));
    }
    for _ in 0..100 {
        app.on_fs_event(set, &file, None, gio::FileMonitorEvent::ChangesDoneHint);
    }
    glib::MainContext::default().block_on(async {
        glib::timeout_future(Duration::from_millis(150)).await;
        let starts = loader::SLOW_TEST.with(|paths| paths.borrow().as_ref().unwrap().0.len());
        assert_eq!(
            starts, 1,
            "a content-event burst should launch one replacement"
        );
        loader::SLOW_TEST.with(|paths| paths.borrow_mut().take());
        gate.cancel();
        wait_for_image(&app, &path, 3).await;

        // A queued refresh belongs to its navigation set, even when the same
        // folder is reopened, and cannot survive close.
        app.on_fs_event(set, &file, None, gio::FileMonitorEvent::Changed);
        app.install_folder(Folder::scan(dir.path(), app.cfg.sort).unwrap());
        assert!(app.fs_refresh_timer.0.borrow().is_none());
        app.show_index(0, Arrival::Direct);
        wait_for_image(&app, &path, 3).await;
        let generation = app.navigation.borrow().generation();
        app.on_fs_event(set, &file, None, gio::FileMonitorEvent::Changed);
        assert_eq!(app.navigation.borrow().generation(), generation);
        assert!(app.cache.contains(&path));
        let current_set = app.navigation.borrow().set_id().unwrap();
        app.on_fs_event(current_set, &file, None, gio::FileMonitorEvent::Changed);
        app.shutdown();
        assert!(app.fs_refresh_timer.0.borrow().is_none());
        glib::timeout_future(Duration::from_millis(150)).await;
        assert!(!app.cache.contains(&path));
    });
}

#[test]
#[ignore = "requires a GNOME/Wayland session; run separately with --ignored --exact"]
fn renamed_image_waits_for_metadata_before_reloading() {
    let app = edit_test_app();
    let dir = tempfile::tempdir().unwrap();
    let old = dir.path().join("old.png");
    let new = dir.path().join("new.png");
    write_test_image(&old, 3);
    app.install_folder(Folder::scan(dir.path(), app.cfg.sort).unwrap());
    app.monitor.borrow_mut().take().unwrap().cancel();
    app.show_index(0, Arrival::Direct);
    glib::MainContext::default().block_on(async {
        wait_for_image(&app, &old, 3).await;
        let gate = gio::Cancellable::new();
        monitor::SNAPSHOT_TEST_GATE.with(|slot| *slot.borrow_mut() = Some(gate.clone()));
        let set = app.navigation.borrow().set_id().unwrap();
        std::fs::rename(&old, &new).unwrap();
        app.on_fs_event(
            set,
            &gio::File::for_path(&old),
            Some(&gio::File::for_path(&new)),
            gio::FileMonitorEvent::Renamed,
        );
        glib::timeout_future(Duration::from_millis(250)).await;
        assert!(
            matches!(*app.media.borrow(), MediaState::Loading(_)),
            "must not decode the vanished source while metadata is pending"
        );
        assert!(app.fs_refresh_timer.0.borrow().is_none());
        assert!(!app.cache.contains(&old));
        monitor::SNAPSHOT_TEST_GATE.with(|slot| slot.borrow_mut().take());
        gate.cancel();
        wait_for_image(&app, &new, 3).await;

        // A destination that no longer resolves to a regular file follows
        // removal policy instead of leaving the old image permanently Loading.
        let broken = dir.path().join("broken.png");
        std::fs::rename(&new, &broken).unwrap();
        std::fs::remove_file(&broken).unwrap();
        std::os::unix::fs::symlink("broken.png", &broken).unwrap();
        app.on_fs_event(
            set,
            &gio::File::for_path(&new),
            Some(&gio::File::for_path(&broken)),
            gio::FileMonitorEvent::Renamed,
        );
        let start = std::time::Instant::now();
        while !matches!(*app.media.borrow(), MediaState::Empty) {
            assert!(start.elapsed() < Duration::from_secs(5));
            glib::timeout_future(Duration::from_millis(5)).await;
        }
        app.shutdown();
    });
}

#[test]
#[ignore = "requires a GNOME/Wayland session; run separately with --ignored --exact"]
fn pending_rename_survives_deletion_and_a_second_rename() {
    let app = edit_test_app();
    glib::MainContext::default().block_on(async {
        for mode in 0..4 {
            let deleted = mode == 0;
            let dir = tempfile::tempdir().unwrap();
            let old = dir.path().join("a.png");
            let intermediate = dir.path().join("b.png");
            let final_path = dir.path().join("c.png");
            let neighbor = dir.path().join("z.png");
            write_test_image(&old, 3);
            write_test_image(&neighbor, 10);
            if mode == 3 {
                write_test_image(&final_path, 7);
            }
            app.install_folder(Folder::scan(dir.path(), app.cfg.sort).unwrap());
            app.monitor.borrow_mut().take().unwrap().cancel();
            app.show_index(0, Arrival::Direct);
            wait_for_image(&app, &old, 3).await;
            let set = app.navigation.borrow().set_id().unwrap();
            let gate = gio::Cancellable::new();
            monitor::SNAPSHOT_TEST_GATE.with(|slot| *slot.borrow_mut() = Some(gate.clone()));
            std::fs::rename(&old, &intermediate).unwrap();
            app.on_fs_event(
                set,
                &gio::File::for_path(&old),
                Some(&gio::File::for_path(&intermediate)),
                gio::FileMonitorEvent::Renamed,
            );
            glib::timeout_future(Duration::from_millis(10)).await;
            if deleted {
                std::fs::remove_file(&intermediate).unwrap();
                app.on_fs_event(
                    set,
                    &gio::File::for_path(&intermediate),
                    None,
                    gio::FileMonitorEvent::Deleted,
                );
                wait_for_image(&app, &neighbor, 10).await;
            } else if mode == 1 {
                std::fs::rename(&intermediate, &final_path).unwrap();
                app.on_fs_event(
                    set,
                    &gio::File::for_path(&intermediate),
                    Some(&gio::File::for_path(&final_path)),
                    gio::FileMonitorEvent::Renamed,
                );
            }
            if mode == 2 {
                write_test_image(&old, 9);
                app.on_fs_event(
                    set,
                    &gio::File::for_path(&old),
                    None,
                    gio::FileMonitorEvent::Created,
                );
            } else if mode == 3 {
                std::fs::rename(&final_path, &intermediate).unwrap();
                app.on_fs_event(
                    set,
                    &gio::File::for_path(&final_path),
                    Some(&gio::File::for_path(&intermediate)),
                    gio::FileMonitorEvent::Renamed,
                );
            }
            glib::timeout_future(Duration::from_millis(150)).await;
            monitor::SNAPSHOT_TEST_GATE.with(|slot| slot.borrow_mut().take());
            gate.cancel();
            let (expected, width) = match mode {
                0 => (&neighbor, 10),
                1 => (&final_path, 3),
                2 => (&intermediate, 3),
                _ => (&intermediate, 7),
            };
            wait_for_image(&app, expected, width).await;
            glib::timeout_future(Duration::from_millis(20)).await;
            if mode == 2 {
                let index = app.navigation.borrow().index_of(&old).unwrap();
                app.show_index(index, Arrival::Direct);
                wait_for_image(&app, &old, 9).await;
            } else {
                assert!(app.navigation.borrow().index_of(&old).is_none());
            }
            if mode <= 1 {
                assert!(app.navigation.borrow().index_of(&intermediate).is_none());
            }
        }
        app.shutdown();
    });
}

#[test]
#[ignore = "requires a GNOME/Wayland session; run separately with --ignored --exact"]
fn folder_replacement_releases_cached_images() {
    let app = edit_test_app();
    let old = tempfile::tempdir().unwrap();
    let new = tempfile::tempdir().unwrap();
    let path = old.path().join("a.png");
    write_test_image(&path, 3);
    app.install_folder(Folder::scan(old.path(), app.cfg.sort).unwrap());
    let decoded = test_image(3);
    let weak = Rc::downgrade(&decoded);
    app.cache
        .put_foreground(path.clone(), decoded, "image/png".into());
    app.install_folder(Folder::scan(old.path(), app.cfg.sort).unwrap());
    assert!(
        app.cache.contains(&path),
        "same-folder opens retain cache hits"
    );
    app.install_folder(Folder::scan(new.path(), app.cfg.sort).unwrap());
    assert!(
        weak.upgrade().is_none(),
        "old decoded storage remains cached"
    );
    app.shutdown();
}

#[test]
#[ignore = "requires a GNOME/Wayland session; run separately with --ignored --exact"]
fn folder_cleanup_rejects_late_decodes_and_preserves_operations() {
    let app = edit_test_app();
    let old = tempfile::tempdir().unwrap();
    let new = tempfile::tempdir().unwrap();
    let path = old.path().join("a.png");
    write_test_image(&path, 3);
    for foreground in [true, false] {
        app.install_folder(Folder::scan(old.path(), app.cfg.sort).unwrap());
        let destination = app.navigation.borrow_mut().select(0).unwrap();
        app.decodes.borrow_mut().replace(
            foreground.then(|| (path.clone(), (destination.generation, Arrival::Direct))),
            (!foreground).then(|| path.clone()),
        );
        let job = app.decodes.borrow_mut().start().unwrap();
        app.install_folder(Folder::scan(new.path(), app.cfg.sort).unwrap());
        assert!(job.cancellable.is_cancelled());
        // Even an immediate return must not revive cancelled work.
        app.install_folder(Folder::scan(old.path(), app.cfg.sort).unwrap());
        app.on_decode_completed(path.clone(), Ok((test_image(2), "image/png".into())));
        assert!(!app.cache.contains(&path));
    }
    app.navigation.borrow_mut().select(0).unwrap();
    let save = app
        .operations
        .borrow_mut()
        .start_save(&app.navigation.borrow(), &path)
        .unwrap();
    app.install_folder(Folder::scan(new.path(), app.cfg.sort).unwrap());
    assert!(
        app.operations
            .borrow_mut()
            .finish_save(&save, &app.navigation.borrow())
            .is_none()
    );

    app.install_folder(Folder::scan(old.path(), app.cfg.sort).unwrap());
    app.navigation.borrow_mut().select(0).unwrap();
    let trash = app
        .operations
        .borrow_mut()
        .start_trash(&app.navigation.borrow(), &path)
        .unwrap();
    app.operations.borrow_mut().finish_trash(trash);
    for error in [false, true] {
        app.cache
            .put_foreground(path.clone(), test_image(3), "image/png".into());
        let set = app.navigation.borrow().set_id();
        if error {
            app.show_error(&path, "test error");
        } else {
            app.empty_state("test empty");
        }
        assert!(!app.cache.contains(&path));
        assert_eq!(app.navigation.borrow().set_id(), set);
        assert!(app.operations.borrow().has_undo());
    }
    app.shutdown();
}

/// Exercise pipeline reuse with transport polling on the actual GTK frame clock.
/// Run under an external timeout as a lock inversion can stop GLib timers too.
#[test]
#[ignore = "requires GNOME/Wayland and OPEN_MPV_TRANSITION_VIDEO; run separately under timeout"]
fn repeated_image_video_transitions_keep_gtk_responsive() {
    let app = edit_test_app();
    let video = PathBuf::from(
        std::env::var_os("OPEN_MPV_TRANSITION_VIDEO").expect("provide a local test video"),
    );
    let dir = tempfile::tempdir().unwrap();
    let image = dir.path().join("image.png");
    write_test_image(&image, 3);
    let broken_video = dir.path().join("broken.webm");
    std::fs::write(&broken_video, "not a video").unwrap();
    glib::MainContext::default().block_on(async {
        for cycle in 0..10 {
            app.open_path(&video);
            let start = std::time::Instant::now();
            loop {
                glib::timeout_future(Duration::from_millis(10)).await;
                assert!(
                    start.elapsed() < Duration::from_secs(5),
                    "video did not start in cycle {cycle}"
                );
                if app
                    .player
                    .borrow()
                    .as_ref()
                    .and_then(|p| p.progress())
                    .is_some_and(|(position, _)| position > 0.1)
                {
                    break;
                }
            }
            app.update_transport();
            app.open_path(&image);
            wait_for_image(&app, &image, 3).await;
            glib::timeout_future(Duration::from_millis(50)).await;
            if cycle % 3 == 0 {
                app.open_path(&broken_video);
                let start = std::time::Instant::now();
                while !matches!(&*app.media.borrow(), MediaState::Error(path) if path == &broken_video) {
                    assert!(start.elapsed() < Duration::from_secs(5), "broken video did not report an error");
                    glib::timeout_future(Duration::from_millis(10)).await;
                }
                assert!(!app.player.borrow().as_ref().unwrap().has_video());
                assert!(!app.cache.contains(&image));
            }
        }
        app.shutdown();
    });
}

/// Measures the typed navigation action through compositor presentation feedback,
/// rather than stopping the timer when the decoded cache lookup returns.
#[test]
#[ignore = "requires GNOME/Wayland and OPEN_MPV_TRANSITION_IMAGES with three images"]
fn cached_navigation_presentation_latency() {
    let app = edit_test_app();
    let directory = PathBuf::from(
        std::env::var_os("OPEN_MPV_TRANSITION_IMAGES").expect("provide an image fixture folder"),
    );
    app.install_folder(Folder::scan(&directory, app.cfg.sort).unwrap());
    assert_eq!(app.navigation.borrow().len(), 3);
    let paths: Vec<_> = (0..3)
        .map(|i| app.navigation.borrow().get(i).unwrap().to_path_buf())
        .collect();
    app.show_index(1, Arrival::Direct);
    glib::MainContext::default().block_on(async {
        let start = std::time::Instant::now();
        while !paths.iter().all(|path| app.cache.contains(path)) || !app.win.is_mapped() {
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "fixtures did not load"
            );
            glib::timeout_future(Duration::from_millis(5)).await;
        }
        glib::timeout_future(Duration::from_millis(250)).await;
        let clock = app.win.frame_clock().unwrap();
        let mut latencies = Vec::new();
        for i in 0..20 {
            let frame = Rc::new(Cell::new(None));
            let painted = frame.clone();
            let handler = clock.connect_after_paint(move |clock| {
                if painted.get().is_none() {
                    painted.set(Some(clock.frame_counter()));
                }
            });
            let started = glib::monotonic_time();
            app.dispatch_action(if i % 2 == 0 {
                Action::Next
            } else {
                Action::Previous
            });
            let expected = if i % 2 == 0 { &paths[2] } else { &paths[1] };
            assert_eq!(
                app.navigation.borrow().current_path(),
                Some(expected.as_path())
            );
            while frame.get().is_none() {
                assert!(
                    glib::monotonic_time() - started < 2_000_000,
                    "frame was not painted"
                );
                glib::timeout_future(Duration::from_millis(1)).await;
            }
            clock.disconnect(handler);
            let timings = clock.timings(frame.get().unwrap()).unwrap();
            while !timings.is_complete() {
                assert!(
                    glib::monotonic_time() - started < 2_000_000,
                    "no compositor feedback"
                );
                glib::timeout_future(Duration::from_millis(1)).await;
            }
            let presented = timings.presentation_time();
            assert!(
                presented >= started,
                "compositor did not provide a presentation timestamp"
            );
            latencies.push((presented - started) as f64 / 1000.0);
        }
        eprintln!("cached navigation action-to-presentation ms: {latencies:?}");
        assert!(
            latencies.iter().all(|latency| *latency < 100.0),
            "cached navigation exceeded NFR-1.2"
        );
        app.shutdown();
    });
}

#[test]
#[ignore = "requires GNOME/Wayland, a user trash, and ImageMagick; run separately"]
fn rotate_save_and_trash_undo_reload_after_cache_cleanup() {
    let app = edit_test_app();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("image.png");
    write_test_image(&path, 3);
    app.open_path(&path);
    glib::MainContext::default().block_on(async {
        wait_for_image(&app, &path, 3).await;
        let set = app.navigation.borrow().set_id();
        app.dispatch_action(Action::RotateClockwise);
        app.dispatch_action(Action::Save);
        wait_for_image(&app, &path, 1).await;
        app.dispatch_action(Action::Trash);
        let start = std::time::Instant::now();
        while !matches!(*app.media.borrow(), MediaState::Empty)
            || !app.operations.borrow().has_undo()
        {
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "trash did not reach the empty state"
            );
            glib::timeout_future(Duration::from_millis(5)).await;
        }
        assert!(!path.exists());
        assert!(!app.cache.contains(&path));
        assert_eq!(app.navigation.borrow().set_id(), set);
        app.dispatch_action(Action::Undo);
        wait_for_image(&app, &path, 1).await;
        assert!(path.is_file());
        assert_eq!(app.navigation.borrow().set_id(), set);
        app.shutdown();
    });
}
