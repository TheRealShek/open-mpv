//! Plain-language presentation at the window boundary; typed errors remain
//! available to callers and diagnostics retain their original context.

use std::error::Error;
use std::io;

use gtk4::{gio, glib};

use crate::{fileops, player};

pub(super) fn message(error: &(dyn Error + 'static), operation: &str) -> String {
    let mut cause = Some(error);
    while let Some(error) = cause {
        let reason = if let Some(error) = error.downcast_ref::<io::Error>() {
            io_reason(error.kind())
        } else if let Some(error) = error.downcast_ref::<glib::Error>() {
            gio_reason(error)
        } else if let Some(error) = error.downcast_ref::<glycin::ErrorCtx>() {
            glycin_reason(error.error())
        } else if let Some(error) = error.downcast_ref::<glycin::Error>() {
            glycin_reason(error)
        } else if let Some(error) = error.downcast_ref::<fileops::RestoreError>() {
            match error {
                fileops::RestoreError::NotFound(_) => Some("The file is no longer in the trash."),
                fileops::RestoreError::DestinationExists(_) => {
                    Some("Another file already exists at the original location.")
                }
                _ => None,
            }
        } else if matches!(
            error.downcast_ref::<fileops::SaveRotationError>(),
            Some(fileops::SaveRotationError::LosslessUnavailable(_))
        ) {
            Some("This image cannot be rotated without losing quality.")
        } else if matches!(
            error.downcast_ref::<player::PlayerError>(),
            Some(
                player::PlayerError::SinkUnavailable(_)
                    | player::PlayerError::PlaybinUnavailable(_)
            )
        ) {
            Some(
                "A required video component is missing. Check the video packages listed in Troubleshooting.",
            )
        } else {
            None
        };
        if let Some(reason) = reason {
            return format!("{operation} {reason}");
        }
        cause = error.source();
    }
    operation.to_owned()
}

fn io_reason(kind: io::ErrorKind) -> Option<&'static str> {
    match kind {
        io::ErrorKind::PermissionDenied => {
            Some("You do not have permission to access this file or folder.")
        }
        io::ErrorKind::NotFound => Some("The file or folder is no longer available."),
        io::ErrorKind::AlreadyExists => Some("Another file already exists at that location."),
        io::ErrorKind::StorageFull => Some("There is not enough free space."),
        io::ErrorKind::ReadOnlyFilesystem => Some("This location is read-only."),
        _ => None,
    }
}

fn gio_reason(error: &glib::Error) -> Option<&'static str> {
    match error.kind::<gio::IOErrorEnum>() {
        Some(gio::IOErrorEnum::PermissionDenied) => io_reason(io::ErrorKind::PermissionDenied),
        Some(gio::IOErrorEnum::NotFound) => io_reason(io::ErrorKind::NotFound),
        Some(gio::IOErrorEnum::Exists) => io_reason(io::ErrorKind::AlreadyExists),
        Some(gio::IOErrorEnum::NoSpace) => io_reason(io::ErrorKind::StorageFull),
        Some(gio::IOErrorEnum::ReadOnly) => io_reason(io::ErrorKind::ReadOnlyFilesystem),
        _ => None,
    }
}

fn glycin_reason(error: &glycin::Error) -> Option<&'static str> {
    match error {
        glycin::Error::GLibError(error) | glycin::Error::ImageSource(error) => gio_reason(error),
        glycin::Error::StdIoError { err, .. } => io_reason(err.kind()),
        glycin::Error::NoLoadersConfigured(_) | glycin::Error::SpawnErrorNotFound { .. } => {
            Some("An image loader is missing. Check that glycin-loaders is installed.")
        }
        error if error.unsupported_format().is_some() => {
            Some("No installed image loader supports this format.")
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_causes_are_readable_without_backend_details() {
        let error = fileops::SaveRotationError::AtomicWrite {
            path: "/private/picture.png".into(),
            source: io::Error::from(io::ErrorKind::PermissionDenied),
        };
        assert_eq!(
            message(&error, "Could not save the rotation."),
            "Could not save the rotation. You do not have permission to access this file or folder."
        );
        let error = glycin::ErrorCtx::from_error(glycin::Error::ImageSource(glib::Error::new(
            gio::IOErrorEnum::NotFound,
            "backend details",
        )));
        assert_eq!(
            message(&error, "Could not open the image."),
            "Could not open the image. The file or folder is no longer available."
        );
        assert_eq!(
            message(
                &io::Error::other("internal debug dump"),
                "Could not restore the file."
            ),
            "Could not restore the file."
        );
    }
}
