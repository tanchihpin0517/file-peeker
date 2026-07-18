use std::{path::Path, sync::Arc};

use crate::{FileMetadata, FilePeekerError, SessionConfig, SessionTarget, ops, startup};

#[derive(Debug)]
pub(crate) struct Session {
    lifecycle: startup::LifecycleHandle,
    mode: SessionMode,
    target: SessionTarget,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SessionMode {
    Local,
    Ssh,
}

impl From<&SessionTarget> for SessionMode {
    fn from(target: &SessionTarget) -> Self {
        match target {
            SessionTarget::Local { .. } => Self::Local,
            SessionTarget::Ssh { .. } => Self::Ssh,
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.lifecycle.shutdown();
    }
}

impl Session {
    /// Creates a session and starts its dedicated local or SSH server.
    ///
    /// # Errors
    ///
    /// Returns a typed startup, process, connection, or protocol error.
    pub(crate) async fn start(config: SessionConfig) -> Result<Arc<Self>, FilePeekerError> {
        let mode = SessionMode::from(&config.target);
        let target = config.target.clone();
        let lifecycle = startup::start(config).await?;
        Ok(Arc::new(Self {
            lifecycle,
            mode,
            target,
        }))
    }

    /// Returns the immutable local or SSH target owned by this session.
    #[must_use]
    pub(crate) fn target(&self) -> SessionTarget {
        self.target.clone()
    }

    /// Starts a streamed listing of the direct children at `path`.
    ///
    /// # Errors
    ///
    /// Returns a typed path, connection, protocol, or filesystem error.
    pub(crate) async fn list(
        self: Arc<Self>,
        path: String,
    ) -> Result<Arc<ops::Listing>, FilePeekerError> {
        ops::Listing::start(self, path).await
    }

    /// Returns the server process's current working directory.
    ///
    /// # Errors
    ///
    /// Returns a connection, protocol, or remote filesystem error.
    pub(crate) async fn current_root(&self) -> Result<String, FilePeekerError> {
        self.ensure_open()?;
        ops::current_root(self.socket_path().to_path_buf()).await
    }

    /// Closes the control connection and waits for the owned server to exit.
    ///
    /// # Errors
    ///
    /// Returns an error if shutdown does not complete within its bounded timeout.
    pub(crate) async fn close(&self) -> Result<(), FilePeekerError> {
        self.lifecycle.close().await
    }

    /// Opens a local path with the system default application.
    ///
    /// SSH clients intentionally treat this operation as a successful no-op.
    ///
    /// # Errors
    ///
    /// Returns a connection error when the session is closed, or an I/O error
    /// when the macOS system opener cannot be launched or reports failure.
    pub(crate) async fn open(&self, path: String) -> Result<(), FilePeekerError> {
        self.ensure_open()?;
        ops::open(self.mode, path).await
    }

    /// Retrieves metadata for one path.
    ///
    /// # Errors
    ///
    /// Returns [`FilePeekerError::NotImplemented`] in the empty v1 skeleton.
    #[allow(clippy::unused_async)]
    pub(crate) async fn metadata(&self, path: String) -> Result<FileMetadata, FilePeekerError> {
        let _ = path;
        Err(FilePeekerError::not_implemented("Session.metadata"))
    }
}

impl Session {
    pub(super) fn ensure_open(&self) -> Result<(), FilePeekerError> {
        if self.lifecycle.is_closed() {
            return Err(FilePeekerError::ConnectionClosed {
                message: "server is no longer running".into(),
            });
        }
        Ok(())
    }

    pub(super) fn socket_path(&self) -> &Path {
        self.lifecycle.socket_path()
    }
}

#[cfg(test)]
mod tests {
    use super::Session;
    use crate::{FilePeekerError, SessionConfig, SessionTarget};

    #[tokio::test]
    async fn start_rejects_an_empty_server_executable() {
        let error = Session::start(SessionConfig {
            target: SessionTarget::Local {
                server_executable_path: String::new(),
            },
        })
        .await
        .expect_err("an empty executable must fail");

        assert!(matches!(error, FilePeekerError::ServerStart { .. }));
    }

    #[tokio::test]
    async fn start_reports_an_early_server_exit() {
        let error = Session::start(SessionConfig {
            target: SessionTarget::Local {
                server_executable_path: "/usr/bin/false".into(),
            },
        })
        .await
        .expect_err("a process that exits immediately must fail startup");

        assert!(matches!(error, FilePeekerError::ServerExited { .. }));
    }

    #[tokio::test]
    async fn remote_connect_requires_an_explicit_destination() {
        let error = Session::start(SessionConfig {
            target: SessionTarget::Ssh {
                destination: String::new(),
            },
        })
        .await
        .expect_err("an empty SSH destination must fail");
        assert!(matches!(error, FilePeekerError::ServerStart { .. }));
    }
}
