//! File Peeker: browse and open files on a local or remote host.
//!
//! One package, three binaries. The shared layers are always compiled:
//!
//! - [`fs`] performs host-local filesystem work, transport-neutral.
//! - [`protocol`] is the generated wire contract both sides of the gRPC hop use.
//!
//! The remaining layers are feature-gated so a slim build can leave them out —
//! the remote installer provisions a server with
//! `--no-default-features --features server`:
//!
//! - `client` (implied by `cli` and `tui`) adds `Client` and `Session`.
//! - `cli`, `server`, and `tui` each add one binary's logic, exposed as a single
//!   `run` entry point that the matching `src/bin` shim calls.

#[cfg(feature = "client")]
mod client;
mod fs;
pub(crate) mod protocol;
#[cfg(feature = "client")]
mod session;
#[cfg(all(test, feature = "client"))]
mod test_support;

#[cfg(feature = "cli")]
pub mod cli;
#[cfg(feature = "server")]
pub mod server;
#[cfg(feature = "tui")]
pub mod tui;

#[cfg(feature = "client")]
pub(crate) use client::Client;
#[cfg(feature = "client")]
pub(crate) use session::directory::{
    DirectoryEntry, EntryKind, EntryStream, WalkEntry, WalkStream,
};
#[cfg(feature = "client")]
pub(crate) use session::{
    CopyConflict, CopyResult, Session, SessionShutdownError, SessionStartError, SessionTarget,
};

#[cfg(all(test, feature = "client"))]
mod tests {
    use std::sync::Arc;

    use super::{Client, Session};

    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn shared_objects_are_thread_safe() {
        assert_send_sync::<Client>();
        assert_send_sync::<Session>();
        assert_send_sync::<Arc<Client>>();
        assert_send_sync::<Arc<Session>>();
    }
}
