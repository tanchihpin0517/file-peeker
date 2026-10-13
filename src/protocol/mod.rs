//! The two surfaces between a client and a server, as herdr has two sockets.
//!
//! - The **client protocol** is private: gRPC on `file-peeker-client.sock`,
//!   versioned by [`CLIENT_PROTOCOL`]. It may change in any release; a client and
//!   a server whose versions differ refuse to attach.
//! - The **API** is public: newline-delimited JSON on `file-peeker.sock`,
//!   versioned by [`api::API_GENERATION`], and the surface the CLI drives.

pub(crate) mod api;
pub(crate) mod probe;

/// The client protocol's version. Bump it on any change a client relies on,
/// including a new call: a client that needs `Watch` cannot use a server that
/// answers the handshake but not the stream, and the handshake is the only place
/// that can say so.
///
/// - 1: `Attach`.
/// - 2: `Watch`.
/// - 3: tasks on `Watch`: a snapshot, then every change.
pub(crate) const CLIENT_PROTOCOL: u32 = 3;

/// This build's version, as both roles report it.
pub(crate) const VERSION: &str = env!("CARGO_PKG_VERSION");

#[allow(clippy::all, clippy::pedantic, unreachable_pub)]
pub(crate) mod client_v1 {
    tonic::include_proto!("file_peeker.client.v1");
}
