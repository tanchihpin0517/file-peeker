use std::io;

use thiserror::Error;

const SERVICE_CANCELLED_MESSAGE: &str = "filesystem service is cancelled";

/// Stable category for a filesystem operation failure.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum FsErrorKind {
    InvalidArgument,
    NotFound,
    PermissionDenied,
    NotDirectory,
    NotFile,
    /// The operation was interrupted because its filesystem service was cancelled.
    Cancelled,
    Internal,
}

/// A transport-independent filesystem operation failure.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("{message}")]
pub struct FsError {
    kind: FsErrorKind,
    message: String,
}

impl FsError {
    #[must_use]
    pub fn new(kind: FsErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    #[must_use]
    pub const fn kind(&self) -> FsErrorKind {
        self.kind
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    #[must_use]
    pub(crate) fn from_io(error: &io::Error) -> Self {
        let kind = match error.kind() {
            io::ErrorKind::NotFound => FsErrorKind::NotFound,
            io::ErrorKind::PermissionDenied => FsErrorKind::PermissionDenied,
            io::ErrorKind::NotADirectory => FsErrorKind::NotDirectory,
            io::ErrorKind::IsADirectory => FsErrorKind::NotFile,
            io::ErrorKind::Interrupted => FsErrorKind::Cancelled,
            _ => FsErrorKind::Internal,
        };
        Self::new(kind, error.to_string())
    }
}

pub(crate) fn service_cancelled_error() -> FsError {
    FsError::new(FsErrorKind::Cancelled, SERVICE_CANCELLED_MESSAGE)
}
