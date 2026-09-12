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
