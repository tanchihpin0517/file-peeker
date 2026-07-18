//! Shared public API for every File Peeker UI.
//!
//! Rust consumers and `UniFFI` bindings use the same objects, value types, errors,
//! and methods declared here. The exported objects are thin wrappers around
//! crate-private implementations so internal connection, browsing, and file
//! operation code can evolve without changing the public API.
//!
//! The API starts with [`Client`]. A client creates independent [`Session`]
//! objects for local or SSH targets. A session owns one connection lifecycle,
//! opens files, reports its current root, and creates [`State`] objects. A state
//! maintains the expandable browsing status for one fixed root. All operations
//! return [`FilePeekerError`] on failure.
//!
//! Public object API:
//!
//! - [`Client::new`] creates the shared API entry point.
//! - [`Client::connect`] creates an independent local or SSH session.
//! - [`Session::target`] returns the immutable connection target.
//! - [`Session::open_state`] creates a browsing state rooted at a path.
//! - [`Session::current_root`] returns the connected server's working directory.
//! - [`Session::open`] opens a path with its associated application.
//! - [`Session::metadata`] retrieves path metadata when implemented.
//! - [`Session::close`] shuts down the session lifecycle.
//! - [`State::snapshot`] returns the current flattened browsing rows.
//! - [`State::expand`] freshly loads and expands a visible directory.
//! - [`State::collapse`] removes a directory's visible descendants.

use std::sync::Arc;

use thiserror::Error;

#[derive(Debug, uniffi::Object)]
pub struct Client {
    inner: Arc<crate::client::Client>,
}

#[derive(Debug, uniffi::Object)]
pub struct Session {
    inner: Arc<crate::session::Session>,
}

#[derive(Debug, uniffi::Object)]
pub struct State {
    inner: Arc<crate::state::State>,
}

#[derive(Clone, Debug, Eq, PartialEq, uniffi::Record)]
pub struct SessionConfig {
    pub target: SessionTarget,
}

#[derive(Clone, Debug, Eq, PartialEq, uniffi::Enum)]
pub enum SessionTarget {
    Local { server_executable_path: String },
    Ssh { destination: String },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, uniffi::Enum)]
pub enum EntryKind {
    File,
    Directory,
    Symlink,
    Other,
}

#[derive(Clone, Debug, Eq, PartialEq, uniffi::Record)]
pub struct DirectoryEntry {
    pub path: String,
    pub name: String,
    pub kind: EntryKind,
    pub navigable: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, uniffi::Record)]
pub struct FileMetadata {
    pub path: String,
    pub kind: EntryKind,
    pub size: u64,
    pub readonly: bool,
    pub modified: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, uniffi::Record)]
pub struct StateRow {
    pub entry: DirectoryEntry,
    pub parent_path: Option<String>,
    pub depth: u32,
    pub expanded: bool,
    pub error_message: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, uniffi::Record)]
pub struct StateSnapshot {
    pub path: String,
    pub rows: Vec<StateRow>,
}

#[derive(Clone, Debug, Error, Eq, PartialEq, uniffi::Error)]
pub enum FilePeekerError {
    #[error("operation is not implemented: {operation}")]
    NotImplemented { operation: String },
    #[error("invalid path: {message}")]
    InvalidPath { message: String },
    #[error("failed to start server: {message}")]
    ServerStart { message: String },
    #[error("server process exited: {message}")]
    ServerExited { message: String },
    #[error("connection closed: {message}")]
    ConnectionClosed { message: String },
    #[error("protocol error: {message}")]
    Protocol { message: String },
    #[error("filesystem I/O error: {message}")]
    Io { message: String },
}

impl FilePeekerError {
    pub(crate) fn not_implemented(operation: impl Into<String>) -> Self {
        Self::NotImplemented {
            operation: operation.into(),
        }
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl Client {
    #[uniffi::constructor]
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: crate::client::Client::new(),
        })
    }

    /// Creates an independent connection session.
    ///
    /// # Errors
    ///
    /// Returns a typed startup, process, connection, or protocol error.
    pub async fn connect(&self, config: SessionConfig) -> Result<Arc<Session>, FilePeekerError> {
        let inner = self.inner.connect(config).await?;
        Ok(Arc::new(Session { inner }))
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl Session {
    #[must_use]
    pub fn target(&self) -> SessionTarget {
        self.inner.target()
    }

    /// Opens a fully loaded browsing state rooted at `path`.
    ///
    /// # Errors
    ///
    /// Returns a path, connection, protocol, or filesystem error.
    pub async fn open_state(&self, path: String) -> Result<Arc<State>, FilePeekerError> {
        let inner = Arc::clone(&self.inner).open_state(path).await?;
        Ok(Arc::new(State { inner }))
    }

    /// Returns the connected server's current working directory.
    ///
    /// # Errors
    ///
    /// Returns a connection, protocol, or filesystem error.
    pub async fn current_root(&self) -> Result<String, FilePeekerError> {
        self.inner.current_root().await
    }

    /// Closes this connection session and its owned server lifecycle.
    ///
    /// # Errors
    ///
    /// Returns an error if bounded shutdown does not complete.
    pub async fn close(&self) -> Result<(), FilePeekerError> {
        self.inner.close().await
    }

    /// Opens a path with the platform application associated with it.
    ///
    /// # Errors
    ///
    /// Returns a connection or local process I/O error.
    pub async fn open(&self, path: String) -> Result<(), FilePeekerError> {
        self.inner.open(path).await
    }

    /// Retrieves metadata for one path.
    ///
    /// # Errors
    ///
    /// Currently returns [`FilePeekerError::NotImplemented`].
    pub async fn metadata(&self, path: String) -> Result<FileMetadata, FilePeekerError> {
        self.inner.metadata(path).await
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl State {
    #[must_use]
    pub fn snapshot(&self) -> StateSnapshot {
        self.inner.snapshot()
    }

    /// Freshly loads and expands one visible directory.
    ///
    /// # Errors
    ///
    /// Returns a path, connection, protocol, or filesystem error.
    pub async fn expand(&self, path: String) -> Result<StateSnapshot, FilePeekerError> {
        self.inner.expand(path).await
    }

    /// Collapses a visible directory and discards its descendants.
    ///
    /// # Errors
    ///
    /// Returns an invalid-path error for an unknown or non-navigable row.
    pub fn collapse(&self, path: String) -> Result<StateSnapshot, FilePeekerError> {
        self.inner.collapse(path)
    }
}

#[cfg(test)]
mod tests {
    use super::{Client, FilePeekerError, SessionConfig, SessionTarget};

    #[tokio::test]
    async fn connect_exposes_typed_startup_errors() {
        let error = Client::new()
            .connect(SessionConfig {
                target: SessionTarget::Local {
                    server_executable_path: "/definitely/missing/file-peeker-server".into(),
                },
            })
            .await
            .expect_err("a missing server executable must fail");

        assert!(matches!(error, FilePeekerError::ServerStart { .. }));
    }
}
