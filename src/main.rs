mod config;
mod fileops;
mod folder;
mod loader;
mod log;
mod player;
mod viewer;
mod window;

use std::cell::RefCell;
use std::rc::Rc;

use gtk4 as gtk;

use gtk::prelude::*;

fn main() -> gtk::glib::ExitCode {
    log::init();
    let app = gtk::Application::builder()
        .application_id("dev.thakur.OpenMpv")
        .flags(gtk::gio::ApplicationFlags::HANDLES_OPEN)
        .build();

    // Single instance (FR-7): GApplication uniqueness routes `open` of
    // later launches to the primary instance; one window is reused.
    type AppFactory = Rc<dyn Fn(&gtk::Application) -> Rc<window::App>>;
    let holder: Rc<RefCell<Option<Rc<window::App>>>> = Rc::new(RefCell::new(None));
    let get_app: AppFactory = Rc::new({
        let holder = holder.clone();
        move |gtk_app| {
            if let Some(existing) = holder.borrow().as_ref() {
                return existing.clone();
            }
            let created = window::App::new(gtk_app, config::Config::load());
            *holder.borrow_mut() = Some(created.clone());
            created
        }
    });

    app.connect_activate({
        let get_app = get_app.clone();
        move |gtk_app| get_app(gtk_app).present_default()
    });
    app.connect_open({
        let get_app = get_app.clone();
        move |gtk_app, files, _hint| {
            let app = get_app(gtk_app);
            match files.first().and_then(|f| f.path()) {
                Some(path) => app.open_path(&path),
                None => app.present_default(),
            }
        }
    });
    app.run()
}
