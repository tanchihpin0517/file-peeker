//! Fixtures for exercising `Session` workflows without touching a real
//! filesystem or a remote host.
//!
//! [`ScriptedServer`] is a real File Peeker server whose filesystem adapter has
//! been swapped for a queue of canned responses. Everything below the response
//! is genuine — the loopback listener, the bearer token, the health check, the
//! channel, and every conversion in `session::backend::rpc` — so a test built on
//! it exercises the whole client stack and not just the layer above it.
//!
//! Compiled under `cfg(test)` only, so these fixtures cannot reach a release
//! build.

use std::{
    collections::VecDeque,
    io,
    sync::{Arc, Mutex},
};

use futures::{StreamExt as _, stream, stream::BoxStream};
use tonic::{Request, Response, Status};

use crate::{
    DirectoryEntry, EntryKind,
    protocol::v1::{
        EntryKind as ProtocolEntryKind, ListBatch, ListRequest, ListingEntry, ReadChunk,
        ReadRequest, ResolvePathRequest, ResolvePathResponse, WalkBatch, WalkRequest, WriteFrame,
        WriteResponse, file_peeker_server::FilePeeker,
    },
    session::backend::connection::ServerConnection,
};

/// One scripted response to a List request.
#[derive(Debug)]
pub(crate) enum Listing {
    /// Entries, then a clean end of stream.
    Entries(Vec<DirectoryEntry>),
    /// Entries, then a terminal error — the partial-results case.
    Failing(Vec<DirectoryEntry>, io::Error),
    /// The call fails before any stream is returned.
    Rejected(io::Error),
    /// A stream that never yields, for cancellation tests.
    Pending,
}

/// A File Peeker server that replays scripted responses and records requests.
///
/// Cloning shares one script, so a test can retain a handle after handing a
/// clone to the server task.
#[derive(Clone, Default)]
pub(crate) struct ScriptedServer {
    inner: Arc<Inner>,
}

#[derive(Default)]
struct Inner {
    listings: Mutex<VecDeque<Listing>>,
    listed_paths: Mutex<Vec<String>>,
}

impl ScriptedServer {
    /// Queues the responses List returns, in call order. An exhausted queue
    /// rejects later requests.
    #[must_use]
    pub(crate) fn new(listings: impl IntoIterator<Item = Listing>) -> Self {
        let server = Self::default();
        server.inner.listings.lock().unwrap().extend(listings);
        server
    }

    /// Starts this server on a loopback port and connects to it.
    ///
    /// # Panics
    ///
    /// Panics if the server cannot be started or reached.
    pub(crate) async fn connect(&self) -> ServerConnection {
        ServerConnection::open_scripted(self.clone())
            .await
            .expect("a scripted server should start")
    }

    /// Paths passed to List, in call order.
    ///
    /// # Panics
    ///
    /// Panics if a fixture thread panicked while recording.
    #[must_use]
    pub(crate) fn listed_paths(&self) -> Vec<String> {
        self.inner.listed_paths.lock().unwrap().clone()
    }

    fn next_listing(&self, path: String) -> Listing {
        self.inner.listed_paths.lock().unwrap().push(path);
        self.inner
            .listings
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Listing::Rejected(io::Error::other("no scripted listing")))
    }
}

#[tonic::async_trait]
impl FilePeeker for ScriptedServer {
    type ListStream = BoxStream<'static, Result<ListBatch, Status>>;
    type ReadStream = BoxStream<'static, Result<ReadChunk, Status>>;
    type WalkStream = BoxStream<'static, Result<WalkBatch, Status>>;

    /// The identity, which the client accepts because every scripted test uses
    /// absolute paths.
    async fn resolve_path(
        &self,
        request: Request<ResolvePathRequest>,
    ) -> Result<Response<ResolvePathResponse>, Status> {
        Ok(Response::new(ResolvePathResponse {
            path: request.into_inner().path,
        }))
    }

    async fn list(
        &self,
        request: Request<ListRequest>,
    ) -> Result<Response<Self::ListStream>, Status> {
        match self.next_listing(request.into_inner().path) {
            Listing::Entries(entries) => Ok(Response::new(batches(entries, None))),
            Listing::Failing(entries, error) => Ok(Response::new(batches(entries, Some(error)))),
            Listing::Rejected(error) => Err(scripted_status(&error)),
            Listing::Pending => Ok(Response::new(stream::pending().boxed())),
        }
    }

    /// No test scripts a walk: the TUI expands directories through List, and
    /// Walk reaches only the CLI.
    async fn walk(
        &self,
        _request: Request<WalkRequest>,
    ) -> Result<Response<Self::WalkStream>, Status> {
        Err(Status::internal("no scripted walk"))
    }

    async fn read(
        &self,
        _request: Request<ReadRequest>,
    ) -> Result<Response<Self::ReadStream>, Status> {
        Ok(Response::new(stream::empty().boxed()))
    }

    async fn write(
        &self,
        _request: Request<tonic::Streaming<WriteFrame>>,
    ) -> Result<Response<WriteResponse>, Status> {
        Err(Status::internal("no scripted write"))
    }
}

/// Emits one batch holding `entries`, then `error` if the script has one.
///
/// The batch is omitted when there are no entries: the client rejects an empty
/// batch as a protocol violation, which is exactly the invariant it should keep.
fn batches(
    entries: Vec<DirectoryEntry>,
    error: Option<io::Error>,
) -> BoxStream<'static, Result<ListBatch, Status>> {
    let batch = (!entries.is_empty()).then(|| {
        Ok(ListBatch {
            entries: entries.into_iter().map(listing_entry).collect(),
        })
    });
    stream::iter(
        batch
            .into_iter()
            .chain(error.map(|error| Err(scripted_status(&error)))),
    )
    .boxed()
}

fn listing_entry(entry: DirectoryEntry) -> ListingEntry {
    let kind = match entry.kind {
        EntryKind::File => ProtocolEntryKind::File,
        EntryKind::Directory => ProtocolEntryKind::Directory,
        EntryKind::Symlink => ProtocolEntryKind::Symlink,
        EntryKind::Other => ProtocolEntryKind::Other,
    };
    ListingEntry {
        name: entry.name,
        kind: kind.into(),
        navigable: entry.navigable,
    }
}

/// Sends a scripted `io::Error` as the status the real server would use, so it
/// arrives back at the caller with its original kind and message.
fn scripted_status(error: &io::Error) -> Status {
    let message = error.to_string();
    match error.kind() {
        io::ErrorKind::NotFound => Status::not_found(message),
        io::ErrorKind::PermissionDenied => Status::permission_denied(message),
        io::ErrorKind::InvalidInput => Status::invalid_argument(message),
        io::ErrorKind::AlreadyExists => Status::already_exists(message),
        io::ErrorKind::NotADirectory => Status::failed_precondition(message),
        io::ErrorKind::Interrupted => Status::cancelled(message),
        _ => Status::internal(message),
    }
}
