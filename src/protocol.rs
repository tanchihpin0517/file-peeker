//! Shared wire contract for the File Peeker server.

pub(crate) const PROTOCOL_VERSION: u32 = 1;
/// The gRPC service name, as the health-check protocol spells it. The server
/// registers its status through `NamedService` instead, so only the client and
/// the server's own tests name it directly.
#[cfg(any(test, feature = "client"))]
pub(crate) const FILE_PEEKER_SERVICE_NAME: &str = "file_peeker.v1.FilePeeker";
/// Prefix of the line a server prints to report the loopback port and bearer
/// token it just minted. Both the server that writes it and the launcher that
/// scans a remote server's stdout for it name this constant.
pub(crate) const SERVER_STARTUP_PREFIX: &str = "FILE_PEEKER_SERVER_STARTUP=";
pub(crate) const GRPC_MESSAGE_MAX_BYTES: usize = 1024 * 1024;
pub(crate) const FILE_DATA_CHUNK_MAX_BYTES: usize = 64 * 1024;

#[allow(clippy::all, clippy::pedantic, unreachable_pub)]
pub mod v1 {
    tonic::include_proto!("file_peeker.v1");
}

/// Wire metadata for an entry with nothing interesting about it, for the fixtures
/// that build `ListingEntry` values by hand.
#[cfg(test)]
pub(crate) fn readable_metadata() -> v1::EntryMetadata {
    v1::EntryMetadata {
        size: 0,
        modified_unix_seconds: 0,
        modified_nanos: 0,
        unix_mode: None,
        symlink_target: String::new(),
    }
}
