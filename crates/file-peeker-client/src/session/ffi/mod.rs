mod listing;

use std::{io, sync::Arc};

use thiserror::Error;

pub use listing::{ListError, Listing};

use super::{Session, SessionShutdownError, SessionTarget};

#[derive(Clone, Debug, Eq, Error, PartialEq, uniffi::Error)]
pub enum ResolvePathError {
    #[error("resolve-path operation failed: {message}")]
    Operation { message: String },
}

impl From<io::Error> for ResolvePathError {
    fn from(error: io::Error) -> Self {
        Self::Operation {
            message: error.to_string(),
        }
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl Session {
    /// Returns this session's immutable UUID.
    #[must_use]
    pub fn id(&self) -> String {
        self.id.clone()
    }

    /// Returns the immutable target associated with this session.
    #[must_use]
    pub fn target(&self) -> SessionTarget {
        self.target.clone()
    }

    /// Resolves a path into an absolute lexical path through `UniFFI`.
    ///
    /// # Errors
    ///
    /// Returns an operation error when the session is closed, expansion fails,
    /// the working directory cannot be read, or a remote response is invalid.
    pub async fn op_resolve_path_uniffi(&self, path: String) -> Result<String, ResolvePathError> {
        self.op_resolve_path(&path)
            .await
            .map_err(ResolvePathError::from)
    }

    /// Starts a Swift-compatible listing adapter for one directory.
    ///
    /// # Errors
    ///
    /// Returns a listing error when the operation cannot be started.
    pub async fn op_list_dir_uniffi(&self, path: String) -> Result<Arc<Listing>, ListError> {
        self.op_list_dir(&path)
            .await
            .map(Listing::new)
            .map_err(ListError::from)
    }

    /// Gracefully shuts down this session through `UniFFI`. Repeated calls succeed.
    ///
    /// # Errors
    ///
    /// Returns a shutdown error when the managed backend does not exit cleanly.
    pub async fn close_uniffi(&self) -> Result<(), SessionShutdownError> {
        self.close().await
    }
}

#[cfg(test)]
mod tests {
    use super::{ResolvePathError, Session};
    use crate::SessionTarget;

    #[tokio::test]
    async fn op_resolve_path_uniffi_maps_native_errors() {
        let session = Session::closed_for_test("resolve-path-uniffi-id", SessionTarget::Local);

        let error = session
            .op_resolve_path_uniffi(".".into())
            .await
            .unwrap_err();

        assert_eq!(
            error,
            ResolvePathError::Operation {
                message: "session is closed".into()
            }
        );
    }
}
