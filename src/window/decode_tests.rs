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
