//! How a Session reaches its selected host.
//!
//! [`connection`] owns where the server runs and the channel to it; [`rpc`]
//! turns each Session operation into a request over that channel. Both targets
//! use the same path — only the server's location differs.

pub(crate) mod connection;
pub(crate) mod rpc;

use std::io;

use bytes::Bytes;
use futures::{future::BoxFuture, stream::BoxStream};

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
