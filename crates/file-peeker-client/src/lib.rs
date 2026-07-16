//! UI-independent File Peeker client API.
//!
//! This crate defines the native Rust and `UniFFI` surfaces for v1. All
//! operational methods deliberately return [`ClientError::NotImplemented`]
//! until host lifecycle and filesystem behavior are implemented.

use std::sync::Arc;

use thiserror::Error;

uniffi::setup_scaffolding!();

#[derive(Clone, Debug, Eq, PartialEq, uniffi::Record)]
pub struct ClientConfig {
    pub host_executable_path: String,
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

#[derive(Clone, Debug, Error, Eq, PartialEq, uniffi::Error)]
pub enum ClientError {
    #[error("operation is not implemented: {operation}")]
    NotImplemented { operation: String },
    #[error("invalid path: {message}")]
    InvalidPath { message: String },
    #[error("failed to start host: {message}")]
    HostStart { message: String },
    #[error("host process exited: {message}")]
    HostExited { message: String },
    #[error("connection closed: {message}")]
    ConnectionClosed { message: String },
    #[error("protocol error: {message}")]
    Protocol { message: String },
    #[error("filesystem I/O error: {message}")]
    Io { message: String },
}

impl ClientError {
    #[must_use]
    pub fn not_implemented(operation: impl Into<String>) -> Self {
        Self::NotImplemented {
            operation: operation.into(),
        }
    }
}

#[derive(Debug, uniffi::Object)]
pub struct BrowserClient {
    _private: (),
}

#[uniffi::export]
impl BrowserClient {
    /// Creates a client and starts its dedicated host.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError::NotImplemented`] in the empty v1 skeleton.
    #[uniffi::constructor(name = "start")]
    #[allow(clippy::unused_async)]
    pub async fn start(config: ClientConfig) -> Result<Arc<Self>, ClientError> {
        let _ = config;
        Err(ClientError::not_implemented("BrowserClient.start"))
    }

    /// Starts a pull-based directory listing operation.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError::NotImplemented`] in the empty v1 skeleton.
    #[allow(clippy::unused_async)]
    pub async fn start_listing(&self, path: String) -> Result<Arc<DirectoryListing>, ClientError> {
        let _ = path;
        Err(ClientError::not_implemented("BrowserClient.start_listing"))
    }

    /// Retrieves metadata for one path.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError::NotImplemented`] in the empty v1 skeleton.
    #[allow(clippy::unused_async)]
    pub async fn metadata(&self, path: String) -> Result<FileMetadata, ClientError> {
        let _ = path;
        Err(ClientError::not_implemented("BrowserClient.metadata"))
    }
}

#[derive(Debug, uniffi::Object)]
pub struct DirectoryListing {
    _private: (),
}

#[uniffi::export]
impl DirectoryListing {
    /// Waits for the next directory entry or successful completion.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError::NotImplemented`] in the empty v1 skeleton.
    #[allow(clippy::unused_async)]
    pub async fn next_entry(&self) -> Result<Option<DirectoryEntry>, ClientError> {
        Err(ClientError::not_implemented("DirectoryListing.next_entry"))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{BrowserClient, ClientConfig, ClientError, DirectoryListing};

    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn exported_objects_are_thread_safe() {
        assert_send_sync::<BrowserClient>();
        assert_send_sync::<DirectoryListing>();
        assert_send_sync::<Arc<BrowserClient>>();
        assert_send_sync::<Arc<DirectoryListing>>();
    }

    #[tokio::test]
    async fn start_fails_safely_until_implemented() {
        let error = BrowserClient::start(ClientConfig {
            host_executable_path: "/tmp/file-peeker-host".into(),
        })
        .await
        .expect_err("the skeleton must not claim startup succeeded");

        assert_eq!(
            error,
            ClientError::NotImplemented {
                operation: "BrowserClient.start".into()
            }
        );
    }
}
