use std::{io, path::Path};

use thiserror::Error;

use crate::path_string::EscapeError;

/// What kind of failure, stable enough for a client to act on.
///
/// There is no cancelled kind: an operation is cancelled by being dropped, so it
/// never returns to say so.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum FsErrorKind {
    InvalidArgument,
    AlreadyExists,
    NotFound,
    PermissionDenied,
    NotDirectory,
    NotFile,
    Internal,
}

/// A filesystem operation's failure: its kind, and a message naming the path.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("{message}")]
pub(crate) struct FsError {
    kind: FsErrorKind,
    message: String,
}

impl FsError {
    #[must_use]
    pub(crate) fn new(kind: FsErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    #[must_use]
    pub(crate) const fn kind(&self) -> FsErrorKind {
        self.kind
    }

    #[must_use]
    pub(crate) fn message(&self) -> &str {
        &self.message
    }

    #[must_use]
    pub(crate) fn from_io(error: &io::Error) -> Self {
        Self::new(kind_of(error), error.to_string())
    }

    /// `error` while doing `action` to `path`: "cannot list `/x`: …".
    #[must_use]
    pub(crate) fn at(action: &str, path: &Path, error: &io::Error) -> Self {
        Self::new(
            kind_of(error),
            format!("{action} `{}`: {error}", path.display()),
        )
    }
}

fn kind_of(error: &io::Error) -> FsErrorKind {
    match error.kind() {
        io::ErrorKind::AlreadyExists => FsErrorKind::AlreadyExists,
        io::ErrorKind::NotFound => FsErrorKind::NotFound,
        io::ErrorKind::PermissionDenied => FsErrorKind::PermissionDenied,
        io::ErrorKind::NotADirectory => FsErrorKind::NotDirectory,
        io::ErrorKind::IsADirectory => FsErrorKind::NotFile,
        _ => FsErrorKind::Internal,
    }
}

/// A path that does not decode never came out of the escape, so it is the
/// caller's argument that is wrong.
impl From<EscapeError> for FsError {
    fn from(error: EscapeError) -> Self {
        Self::new(
            FsErrorKind::InvalidArgument,
            format!("path is not correctly escaped: {error}"),
        )
    }
}
