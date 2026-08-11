//! A File Peeker server running inside this process, for a local Session.
//!
//! The client reaches it exactly the way it reaches a remote one — an
//! authenticated gRPC channel to a loopback port — so a local Session differs
//! from a remote Session only in where the server runs and how the connector
//! gets to it.

use std::io;

use crate::server;

use super::{ConnectionInfo, Route, ServerHost};

/// Starts an in-process server and reports where to reach it.
///
/// # Errors
///
/// Returns an error when the server cannot bind its loopback listener or mint
/// an authentication token.
pub(super) async fn start() -> io::Result<(ServerHost, ConnectionInfo, Route)> {
    tracing::debug!("starting embedded server");
    let handle = server::start().await.map_err(io::Error::other)?;
    let info = ConnectionInfo {
        server_port: handle.port(),
        token: handle.token().to_owned(),
    };
    tracing::debug!(server_port = info.server_port, "embedded server started");
    let route = Route::Direct {
        server_port: info.server_port,
    };
    Ok((ServerHost::Embedded(handle), info, route))
}

/// Stops the embedded server and waits for its task to finish.
///
/// # Errors
///
/// Returns an error when the serving task failed or missed its shutdown
/// deadline.
pub(super) async fn shutdown(handle: server::ServerHandle) -> io::Result<()> {
    handle.shutdown().await.map_err(io::Error::other)
}
