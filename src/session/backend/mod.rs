//! The capability seam between a Session and its selected host.
//!
//! There is one implementation, [`RpcBackend`]. Both targets reach a File
//! Peeker server over gRPC; only the server's location differs.

pub(crate) mod connection;
mod rpc;

use std::{fmt::Debug, io};

use async_trait::async_trait;
use bytes::Bytes;
use futures::{future::BoxFuture, stream::BoxStream};

use crate::{EntryStream, WalkStream};

pub(crate) use rpc::RpcBackend;

pub(crate) type ReadStream = BoxStream<'static, io::Result<Bytes>>;
pub(crate) type WriteFuture = BoxFuture<'static, io::Result<BackendWriteResult>>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BackendWriteConflict {
    FailIfExists,
    Replace,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct BackendWriteResult {
    pub(crate) path: String,
    pub(crate) bytes_written: u64,
}

#[async_trait]
pub(crate) trait SessionBackend: Debug + Send + Sync {
    async fn resolve_path(&self, path: &str) -> io::Result<String>;
    async fn list_dir(&self, path: &str) -> io::Result<EntryStream>;
    async fn walk_dir(&self, path: &str) -> io::Result<WalkStream>;
    async fn read_file(&self, path: &str) -> io::Result<ReadStream>;
    /// Not an `async fn`: the caller builds the future under the backend lock,
    /// drops the lock, and only then awaits a transfer that may run for a long
    /// time.
    fn write_file(
        &self,
        directory: String,
        name: String,
        conflict: BackendWriteConflict,
        source: ReadStream,
    ) -> WriteFuture;
    async fn close(self: Box<Self>) -> io::Result<()>;
}
