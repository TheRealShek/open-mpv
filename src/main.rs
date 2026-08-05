mod config;
mod folder;
mod viewer;

use gtk4 as gtk;
use gtk::prelude::*;

fn main() -> gtk::glib::ExitCode {
    let app = gtk::Application::builder()
        .application_id("dev.thakur.OpenMpv")
        .flags(gtk::gio::ApplicationFlags::HANDLES_OPEN)
        .build();

    app.connect_activate(build_window);
    app.connect_open(|app, files, _hint| {
        build_window(app);
        if let Some(file) = files.first() {
            let file = file.clone();
            let win = app.active_window().unwrap();
            gtk::glib::spawn_future_local(async move {
                match glycin::Loader::new(file).load().await {
                    Ok(image) => match image.next_frame().await {
                        Ok(frame) => {
                            let texture = frame.texture();
                            let picture = gtk::Picture::for_paintable(&texture);
                            win.set_child(Some(&picture));
                            println!(
                                "decoded: {}x{} ({:?})",
                                frame.width(),
                                frame.height(),
                                image.details().info_format_name()
                            );
                        }
                        Err(e) => eprintln!("frame error: {e}"),
                    },
                    Err(e) => eprintln!("load error: {e}"),
                }
            });
        }
    });
    app.run()
}

fn build_window(app: &gtk::Application) {
    if app.active_window().is_none() {
        gtk::ApplicationWindow::builder()
            .application(app)
            .title("open-mpv")
            .default_width(800)
            .default_height(600)
            .build()
            .present();
    }
}
