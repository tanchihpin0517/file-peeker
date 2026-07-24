//! Shared wire contract for the File Peeker server.

pub mod protocol {
    pub const PROTOCOL_VERSION: u32 = 1;
    pub const FILE_PEEKER_SERVICE_NAME: &str = "file_peeker.v1.FilePeeker";
    pub const GRPC_MESSAGE_MAX_BYTES: usize = 1024 * 1024;
    pub const FILE_DATA_CHUNK_MAX_BYTES: usize = 64 * 1024;

    #[allow(clippy::all, clippy::pedantic)]
    pub mod v1 {
        tonic::include_proto!("file_peeker.v1");
    }

    pub use v1::{
        EntryKind, ListBatch, ListRequest, ListingEntry, ReadChunk, ReadRequest,
        ResolvePathRequest, ResolvePathResponse, WalkBatch, WalkEntry, WalkRequest, WriteCommit,
        WriteConflict, WriteFrame, WriteHeader, WriteResponse,
    };
}
