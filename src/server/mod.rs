//! The gRPC filesystem server over [`crate::fs`].
//!
//! [`serve::start`] is the reusable core: it binds a loopback listener and
//! serves on a task, which is how the client embeds its own server for a local
//! Session. The `file-peeker-server` binary wraps that same core in a command
//! line and a stdin lifetime lease.

mod ops;
mod serve;

// Only the client embeds a server; the binary drives `serve` through `command`.
#[cfg(test)]
pub(crate) use serve::start_with;
#[cfg(feature = "client")]
pub(crate) use serve::{ServerHandle, start};

#[cfg(feature = "server")]
mod command;
#[cfg(feature = "server")]
pub use command::run;
