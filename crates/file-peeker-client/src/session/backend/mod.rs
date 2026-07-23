pub mod connection;
mod error;
mod local;
mod remote;

use std::{fmt::Debug, io};

use async_trait::async_trait;
use bytes::Bytes;
use futures::stream::BoxStream;

use crate::EntryStream;

pub(crate) use remote::RemoteBackend;

pub(crate) type ReadStream = BoxStream<'static, io::Result<Bytes>>;

#[async_trait]
pub(crate) trait SessionBackend: Debug + Send + Sync {
    async fn resolve_path(&self, path: &str) -> io::Result<String>;
    async fn list_dir(&self, path: &str) -> io::Result<EntryStream>;
    #[allow(dead_code, reason = "backend-only operation awaiting Session exposure")]
    async fn read_file(&self, path: &str) -> io::Result<ReadStream>;
    async fn close(self: Box<Self>) -> io::Result<()>;
}
