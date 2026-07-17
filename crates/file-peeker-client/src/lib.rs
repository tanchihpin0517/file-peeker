//! UI-independent File Peeker client API.
//!
//! This crate defines the native Rust and `UniFFI` surfaces for v1. Local server
//! startup is implemented; filesystem operations remain placeholders.

use std::sync::Arc;

use thiserror::Error;

mod install;
mod listing;
mod opener;
mod startup;
mod tree;

uniffi::setup_scaffolding!();

#[derive(Clone, Debug, Eq, PartialEq, uniffi::Record)]
pub struct ClientConfig {
    pub target: ServerTarget,
}

#[derive(Clone, Debug, Eq, PartialEq, uniffi::Enum)]
pub enum ServerTarget {
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
pub struct DirectoryTreeRow {
    pub entry: DirectoryEntry,
    pub parent_path: Option<String>,
    pub depth: u32,
    pub expanded: bool,
    pub error_message: Option<String>,
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
    lifecycle: startup::LifecycleHandle,
    mode: ClientMode,
    tree: Arc<std::sync::Mutex<tree::DirectoryTree>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ClientMode {
    Local,
    Ssh,
}

impl From<&ServerTarget> for ClientMode {
    fn from(target: &ServerTarget) -> Self {
        match target {
            ServerTarget::Local { .. } => Self::Local,
            ServerTarget::Ssh { .. } => Self::Ssh,
        }
    }
}

impl Drop for BrowserClient {
    fn drop(&mut self) {
        self.lifecycle.shutdown();
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl BrowserClient {
    /// Creates a client and starts its dedicated local or SSH server.
    ///
    /// # Errors
    ///
    /// Returns a typed startup, process, connection, or protocol error.
    #[uniffi::constructor(name = "start")]
    pub async fn start(config: ClientConfig) -> Result<Arc<Self>, ClientError> {
        let mode = ClientMode::from(&config.target);
        let lifecycle = startup::start(config).await?;
        Ok(Arc::new(Self {
            lifecycle,
            mode,
            tree: Arc::new(std::sync::Mutex::new(tree::DirectoryTree::default())),
        }))
    }

    /// Starts a pull-based directory listing operation.
    ///
    /// # Errors
    ///
    /// Returns a typed path, connection, protocol, or filesystem error.
    pub async fn start_listing(&self, path: String) -> Result<Arc<DirectoryListing>, ClientError> {
        if self.lifecycle.is_closed() {
            return Err(ClientError::ConnectionClosed {
                message: "server is no longer running".into(),
            });
        }
        let state = listing::start(self.lifecycle.socket_path().to_path_buf(), path).await?;
        Ok(Arc::new(DirectoryListing { state }))
    }

    /// Replaces the active tree root and returns its direct entries.
    ///
    /// # Errors
    ///
    /// Returns a typed path, connection, protocol, or filesystem error.
    pub async fn load_tree(&self, path: String) -> Result<Vec<DirectoryTreeRow>, ClientError> {
        self.ensure_open()?;
        let generation = lock_tree(&self.tree).begin_root(path.clone());
        let entries = listing::collect(self.lifecycle.socket_path().to_path_buf(), path).await?;
        Ok(lock_tree(&self.tree).finish_root(generation, entries))
    }

    /// Freshly loads and expands one visible directory in the active tree.
    ///
    /// # Errors
    ///
    /// Returns an invalid-path error for an unknown or non-navigable row, or a
    /// connection, protocol, or filesystem error when listing the directory fails.
    pub async fn expand_tree(&self, path: String) -> Result<Vec<DirectoryTreeRow>, ClientError> {
        self.ensure_open()?;
        let (generation, revision) = match lock_tree(&self.tree).prepare_expand(&path)? {
            tree::ExpandAction::Ready(rows) => return Ok(rows),
            tree::ExpandAction::Load {
                generation,
                revision,
            } => (generation, revision),
        };
        match listing::collect(self.lifecycle.socket_path().to_path_buf(), path.clone()).await {
            Ok(entries) => {
                Ok(lock_tree(&self.tree).finish_expand(generation, revision, &path, entries))
            }
            Err(error) => {
                lock_tree(&self.tree).fail_expand(generation, revision, &path, &error);
                Err(error)
            }
        }
    }

    /// Collapses a visible directory and discards all of its descendants.
    ///
    /// # Errors
    ///
    /// Returns an invalid-path error for an unknown or non-navigable row.
    #[allow(clippy::needless_pass_by_value)]
    pub fn collapse_tree(&self, path: String) -> Result<Vec<DirectoryTreeRow>, ClientError> {
        self.ensure_open()?;
        lock_tree(&self.tree).collapse(&path)
    }

    /// Returns the flattened visible snapshot of the active directory tree.
    #[must_use]
    pub fn tree_rows(&self) -> Vec<DirectoryTreeRow> {
        lock_tree(&self.tree).rows()
    }

    /// Returns the server process's current working directory.
    ///
    /// # Errors
    ///
    /// Returns a connection, protocol, or remote filesystem error.
    pub async fn current_root(&self) -> Result<String, ClientError> {
        if self.lifecycle.is_closed() {
            return Err(ClientError::ConnectionClosed {
                message: "server is no longer running".into(),
            });
        }
        listing::current_root(self.lifecycle.socket_path().to_path_buf()).await
    }

    /// Closes the control connection and waits for the owned server to exit.
    ///
    /// # Errors
    ///
    /// Returns an error if shutdown does not complete within its bounded timeout.
    pub async fn close(&self) -> Result<(), ClientError> {
        self.lifecycle.close().await
    }

    /// Opens a local path with the system default application.
    ///
    /// SSH clients intentionally treat this operation as a successful no-op.
    ///
    /// # Errors
    ///
    /// Returns a connection error when the client is closed, or an I/O error
    /// when the macOS system opener cannot be launched or reports failure.
    pub async fn open(&self, path: String) -> Result<(), ClientError> {
        if self.lifecycle.is_closed() {
            return Err(ClientError::ConnectionClosed {
                message: "server is no longer running".into(),
            });
        }
        opener::open(self.mode, path).await
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

    fn ensure_open(&self) -> Result<(), ClientError> {
        if self.lifecycle.is_closed() {
            return Err(ClientError::ConnectionClosed {
                message: "server is no longer running".into(),
            });
        }
        Ok(())
    }
}

fn lock_tree(
    tree: &std::sync::Mutex<tree::DirectoryTree>,
) -> std::sync::MutexGuard<'_, tree::DirectoryTree> {
    tree.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[derive(Debug, uniffi::Object)]
pub struct DirectoryListing {
    state: Arc<listing::ListingState>,
}

#[uniffi::export(async_runtime = "tokio")]
impl DirectoryListing {
    /// Waits for the next directory entry or successful completion.
    ///
    /// # Errors
    ///
    /// Returns the next streamed entry or `None` when listing is complete.
    pub async fn next_entry(&self) -> Result<Option<DirectoryEntry>, ClientError> {
        listing::next(&self.state).await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{BrowserClient, ClientConfig, ClientError, DirectoryListing, ServerTarget};

    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn exported_objects_are_thread_safe() {
        assert_send_sync::<BrowserClient>();
        assert_send_sync::<DirectoryListing>();
        assert_send_sync::<Arc<BrowserClient>>();
        assert_send_sync::<Arc<DirectoryListing>>();
    }

    #[tokio::test]
    async fn start_rejects_an_empty_server_executable() {
        let error = BrowserClient::start(ClientConfig {
            target: ServerTarget::Local {
                server_executable_path: String::new(),
            },
        })
        .await
        .expect_err("an empty executable must fail");

        assert!(matches!(error, ClientError::ServerStart { .. }));
    }

    #[tokio::test]
    async fn start_reports_an_early_server_exit() {
        let error = BrowserClient::start(ClientConfig {
            target: ServerTarget::Local {
                server_executable_path: "/usr/bin/false".into(),
            },
        })
        .await
        .expect_err("a process that exits immediately must fail startup");

        assert!(matches!(error, ClientError::ServerExited { .. }));
    }

    #[tokio::test]
    async fn remote_connect_requires_an_explicit_destination() {
        let error = BrowserClient::start(ClientConfig {
            target: ServerTarget::Ssh {
                destination: String::new(),
            },
        })
        .await
        .expect_err("an empty SSH destination must fail");
        assert!(matches!(error, ClientError::ServerStart { .. }));
    }
}
