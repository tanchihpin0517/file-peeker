use std::io;

use file_peeker_core::{FsError, FsErrorKind};

pub(crate) fn fs_error(error: &FsError) -> io::Error {
    let kind = match error.kind() {
        FsErrorKind::InvalidArgument => io::ErrorKind::InvalidInput,
        FsErrorKind::NotFound => io::ErrorKind::NotFound,
        FsErrorKind::PermissionDenied => io::ErrorKind::PermissionDenied,
        FsErrorKind::NotDirectory => io::ErrorKind::NotADirectory,
        FsErrorKind::NotFile => io::ErrorKind::IsADirectory,
        FsErrorKind::Cancelled => io::ErrorKind::Interrupted,
        FsErrorKind::Internal => io::ErrorKind::Other,
    };
    io::Error::new(kind, error.message().to_owned())
}
