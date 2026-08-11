//! Fixtures for exercising `Session` workflows without touching a real
//! filesystem, a remote host, or the client operating system.
//!
//! Compiled under `cfg(test)` only, so these fixtures cannot reach a release
//! build.

use std::{
    collections::VecDeque,
    fmt, io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use futures::{StreamExt as _, stream};

use crate::{
    EntryStream, WalkStream,
    session::{
        backend::{ReadStream, SessionBackend},
        file::opener::FileOpener,
    },
};

/// A `SessionBackend` that replays scripted results and records its requests.
///
/// Cloning shares one script, so a test can retain a handle after handing a
/// clone to [`Session::for_test`](crate::Session::for_test).
#[derive(Clone, Default)]
pub(crate) struct FakeBackend {
    inner: Arc<FakeBackendInner>,
}

impl fmt::Debug for FakeBackend {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FakeBackend")
            .field("listed_paths", &self.listed_paths())
            .field("read_paths", &self.read_paths())
            .finish_non_exhaustive()
    }
}

#[derive(Default)]
struct FakeBackendInner {
    listings: Mutex<VecDeque<io::Result<EntryStream>>>,
    listed_paths: Mutex<Vec<String>>,
    read_paths: Mutex<Vec<String>>,
}

impl FakeBackend {
    #[must_use]
    pub(crate) fn builder() -> FakeBackendBuilder {
        FakeBackendBuilder::default()
    }

    /// Paths passed to `list_dir`, in call order.
    ///
    /// # Panics
    ///
    /// Panics if a fixture thread panicked while recording.
    #[must_use]
    pub(crate) fn listed_paths(&self) -> Vec<String> {
        self.inner.listed_paths.lock().unwrap().clone()
    }

    /// Paths passed to `read_file`, in call order.
    ///
    /// # Panics
    ///
    /// Panics if a fixture thread panicked while recording.
    #[must_use]
    pub(crate) fn read_paths(&self) -> Vec<String> {
        self.inner.read_paths.lock().unwrap().clone()
    }
}

impl fmt::Debug for FakeBackendBuilder {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("FakeBackendBuilder").finish()
    }
}

/// Builds a [`FakeBackend`] from queued results.
///
/// Each queue is consumed in order, and an exhausted queue yields an error.
/// `resolve_path` is the identity and `read_file` always yields an empty byte
/// stream, which is what a local open needs: `FileService` discards the stream
/// and opens the resolved path directly.
#[derive(Default)]
pub(crate) struct FakeBackendBuilder {
    inner: FakeBackendInner,
}

impl FakeBackendBuilder {
    /// Queues the results `list_dir` returns, in call order.
    ///
    /// # Panics
    ///
    /// Panics if a fixture thread panicked while queueing.
    #[must_use]
    pub(crate) fn listings(
        mut self,
        listings: impl IntoIterator<Item = io::Result<EntryStream>>,
    ) -> Self {
        self.inner.listings.get_mut().unwrap().extend(listings);
        self
    }

    #[must_use]
    pub(crate) fn build(self) -> FakeBackend {
        FakeBackend {
            inner: Arc::new(self.inner),
        }
    }
}

#[async_trait]
impl SessionBackend for FakeBackend {
    async fn resolve_path(&self, path: &str) -> io::Result<String> {
        Ok(path.to_owned())
    }

    async fn list_dir(&self, path: &str) -> io::Result<EntryStream> {
        self.inner
            .listed_paths
            .lock()
            .unwrap()
            .push(path.to_owned());
        let listing = self.inner.listings.lock().unwrap().pop_front();
        listing.unwrap_or_else(|| Err(io::Error::other("no scripted listing")))
    }

    /// No test scripts walks yet: the TUI expands directories through
    /// `list_dir`, and `walk_dir` reaches only the CLI.
    async fn walk_dir(&self, _path: &str) -> io::Result<WalkStream> {
        Err(io::Error::other("no scripted walk"))
    }

    async fn read_file(&self, path: &str) -> io::Result<ReadStream> {
        self.inner.read_paths.lock().unwrap().push(path.to_owned());
        Ok(stream::empty().boxed())
    }

    async fn close(self: Box<Self>) -> io::Result<()> {
        Ok(())
    }
}

/// A `FileOpener` that records requests instead of invoking the operating
/// system, and can replay scripted failures.
///
/// Cloning shares one recording.
#[derive(Clone, Debug, Default)]
pub(crate) struct RecordingOpener {
    inner: Arc<RecordingOpenerInner>,
}

#[derive(Debug, Default)]
struct RecordingOpenerInner {
    results: Mutex<VecDeque<io::Result<()>>>,
    paths: Mutex<Vec<PathBuf>>,
}

impl RecordingOpener {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Replays `results` in order; later opens succeed.
    ///
    /// # Panics
    ///
    /// Panics if a fixture thread panicked while queueing.
    #[must_use]
    pub(crate) fn with_results(results: impl IntoIterator<Item = io::Result<()>>) -> Self {
        let opener = Self::new();
        opener.inner.results.lock().unwrap().extend(results);
        opener
    }

    /// Paths this opener was asked to open, in call order.
    ///
    /// # Panics
    ///
    /// Panics if a fixture thread panicked while recording.
    #[must_use]
    pub(crate) fn paths(&self) -> Vec<PathBuf> {
        self.inner.paths.lock().unwrap().clone()
    }
}

#[async_trait]
impl FileOpener for RecordingOpener {
    async fn open(&self, path: &Path) -> io::Result<()> {
        self.inner.paths.lock().unwrap().push(path.to_path_buf());
        let result = self.inner.results.lock().unwrap().pop_front();
        result.unwrap_or(Ok(()))
    }
}
