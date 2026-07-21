//! Generated gRPC contract shared by the File Peeker client and server.

pub const PROTOCOL_VERSION: u32 = 1;
pub const FILE_PEEKER_SERVICE_NAME: &str = "file_peeker.v1.FilePeeker";

#[allow(clippy::all, clippy::pedantic)]
pub mod v1 {
    tonic::include_proto!("file_peeker.v1");
}

pub use v1::{EntryKind, ListBatch, ListRequest, ListingEntry};
