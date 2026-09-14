//! Async handoff of one captured source path. The Workspace retains one
//! cancellable until completion, bounding repeated commands and closing work
//! at shutdown without changing media, playback, or view state.

use super::*;

#[derive(Clone, Copy)]
pub(super) enum Handoff {
    ShowInFiles,
    OpenWith,
}

impl Handoff {
    fn failure(self) -> &'static str {
        match self {
            Self::ShowInFiles => "Could not show the file in Files",
            Self::OpenWith => "Could not open the file with another application",
        }
    }
}

impl App {
    pub(super) fn start_handoff(self: &Rc<Self>, handoff: Handoff) {
        if self.shutting_down.get() || self.handoff.borrow().is_some() {
            return;
        }
        // Presentation owns the source. Navigation may already be moving
        // toward a different destination when this action is invoked.
        let Some(path) = self.media.borrow().path().map(Path::to_path_buf) else {
            return;
        };
        let file = gio::File::for_path(&path);
        let cancellable = gio::Cancellable::new();
        *self.handoff.borrow_mut() = Some(cancellable.clone());
        self.sync_action_enabled();
        let weak = Rc::downgrade(self);
        // Do not let Show in Files silently succeed for a missing target just
        // because its parent still exists. Query asynchronously, following
        // symlinks, and keep the original path for both launch and diagnostics.
        file.clone().query_info_async(
            "standard::type",
            gio::FileQueryInfoFlags::NONE,
            glib::Priority::DEFAULT,
            Some(&cancellable),
            move |result| {
                let Some(app) = weak.upgrade() else { return };
                if app.shutting_down.get() {
                    return;
                }
                if let Err(error) = check_target(result) {
                    app.finish_handoff(handoff, &path, Err(error));
                    return;
                }
                let launcher = gtk::FileLauncher::new(Some(&file));
                let weak = Rc::downgrade(&app);
                let complete = move |result| {
                    if let Some(app) = weak.upgrade() {
                        app.finish_handoff(handoff, &path, result);
                    }
                };
                let pending = app.handoff.borrow().clone();
                match handoff {
                    Handoff::ShowInFiles => {
                        launcher.open_containing_folder(Some(&app.win), pending.as_ref(), complete);
                    }
                    Handoff::OpenWith => {
                        launcher.set_always_ask(true);
                        launcher.set_writable(true);
                        launcher.launch(Some(&app.win), pending.as_ref(), complete);
                    }
                }
            },
        );
    }

    fn finish_handoff(
        self: &Rc<Self>,
        handoff: Handoff,
        path: &Path,
        result: Result<(), glib::Error>,
    ) {
        self.handoff.borrow_mut().take();
        if self.shutting_down.get() {
            return;
        }
        self.sync_action_enabled();
        if let Err(error) = result {
            if dialog_was_cancelled(&error) {
                return;
            }
            eprintln!(
                "open-mpv: {} {}: {error}",
                handoff.failure(),
                path.display()
            );
            self.show_toast(&failure_message(handoff, path, &error));
        }
    }
}

fn check_target(result: Result<gio::FileInfo, glib::Error>) -> Result<(), glib::Error> {
    let info = result?;
    if info.file_type() != gio::FileType::Regular {
        return Err(glib::Error::new(
            gio::IOErrorEnum::NotRegularFile,
            "The target is not a regular media file.",
        ));
    }
    Ok(())
}

fn failure_message(handoff: Handoff, path: &Path, error: &glib::Error) -> String {
    let operation = format!("{}: {}.", handoff.failure(), path.display());
    if error.matches(gio::IOErrorEnum::NotRegularFile) {
        return format!("{operation} Choose a media file, not a folder or device.");
    }
    let message = error::message(error, &operation);
    if message == operation {
        format!(
            "{operation} Try again and check that the desktop application service is available."
        )
    } else {
        message
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn async_target_checks_follow_symlinks_and_reject_missing_files() {
        let context = glib::MainContext::new();
        context
            .with_thread_default(|| {
                context.block_on(async {
                    let directory = tempfile::tempdir().unwrap();
                    let original = directory.path().join("original image.jpg");
                    std::fs::write(&original, b"undecodable but valid handoff target").unwrap();
                    let link = directory.path().join("linked image.jpg");
                    std::os::unix::fs::symlink(&original, &link).unwrap();
                    // A read-only or undecodable file is still a valid handoff target.
                    let mut permissions = std::fs::metadata(&original).unwrap().permissions();
                    permissions.set_readonly(true);
                    std::fs::set_permissions(&original, permissions).unwrap();
                    for path in [&original, &link] {
                        let file = gio::File::for_path(path);
                        let result = file
                            .query_info_future(
                                "standard::type",
                                gio::FileQueryInfoFlags::NONE,
                                glib::Priority::DEFAULT,
                            )
                            .await;
                        assert!(check_target(result).is_ok());
                        assert_eq!(file.path().as_ref(), Some(path));
                    }
                    let captured = gio::File::for_path(&original);
                    std::fs::rename(&original, directory.path().join("renamed.jpg")).unwrap();
                    let result = captured
                        .query_info_future(
                            "standard::type",
                            gio::FileQueryInfoFlags::NONE,
                            glib::Priority::DEFAULT,
                        )
                        .await;
                    assert!(
                        check_target(result)
                            .unwrap_err()
                            .matches(gio::IOErrorEnum::NotFound)
                    );
                    assert_eq!(captured.path(), Some(original));
                })
            })
            .unwrap();
    }

    #[test]
    fn target_checks_accept_files_and_preserve_query_errors() {
        let info = gio::FileInfo::new();
        info.set_file_type(gio::FileType::Regular);
        assert!(check_target(Ok(info)).is_ok());
        for kind in [gio::FileType::Directory, gio::FileType::Special] {
            let info = gio::FileInfo::new();
            info.set_file_type(kind);
            assert!(
                check_target(Ok(info))
                    .unwrap_err()
                    .matches(gio::IOErrorEnum::NotRegularFile)
            );
        }
        for kind in [
            gio::IOErrorEnum::NotFound,
            gio::IOErrorEnum::PermissionDenied,
            gio::IOErrorEnum::Cancelled,
        ] {
            assert!(
                check_target(Err(glib::Error::new(kind, "original cause")))
                    .unwrap_err()
                    .matches(kind)
            );
        }
    }

    #[test]
    fn late_failures_identify_the_original_target_and_action() {
        let original = Path::new("/pictures/original.jpg");
        let missing = glib::Error::new(gio::IOErrorEnum::NotFound, "missing");
        let message = failure_message(Handoff::ShowInFiles, original, &missing);
        assert!(message.contains("/pictures/original.jpg"));
        assert!(message.contains("Files"));
        assert!(message.contains("no longer available"));
        let denied = glib::Error::new(gio::IOErrorEnum::PermissionDenied, "denied");
        assert!(failure_message(Handoff::OpenWith, original, &denied).contains("permission"));
        let portal = glib::Error::new(gtk::DialogError::Failed, "Portal unavailable");
        assert!(
            failure_message(Handoff::OpenWith, original, &portal)
                .contains("desktop application service")
        );
    }
}
