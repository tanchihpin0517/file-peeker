pub mod connection;
mod error;
mod local;
mod remote;

use std::{fmt::Debug, io};

use async_trait::async_trait;
use bytes::Bytes;
use futures::{future::BoxFuture, stream::BoxStream};

use crate::{EntryStream, WalkStream};

pub(crate) use remote::RemoteBackend;

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
    fn write_file(
        &self,
        _directory: String,
        _name: String,
        _conflict: BackendWriteConflict,
        _source: ReadStream,
    ) -> WriteFuture {
        Box::pin(async {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "backend does not support writing",
            ))
        })
    }
    async fn close(self: Box<Self>) -> io::Result<()>;
}
