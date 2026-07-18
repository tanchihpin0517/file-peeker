//! UI-independent File Peeker client API.
//!
//! This crate defines the native Rust and `UniFFI` surfaces for connection
//! sessions, independent browsing states, and filesystem operations.

mod api;
mod client;
mod install;
mod ops;
mod session;
mod startup;

pub use api::{
    Client, DirectoryEntry, EntryKind, FileMetadata, FilePeekerError, Listing, Session,
    SessionConfig, SessionTarget,
};

uniffi::setup_scaffolding!();

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{Client, Listing, Session};

    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn exported_objects_are_thread_safe() {
        assert_send_sync::<Client>();
        assert_send_sync::<Session>();
        assert_send_sync::<Listing>();
        assert_send_sync::<Arc<Client>>();
        assert_send_sync::<Arc<Session>>();
        assert_send_sync::<Arc<Listing>>();
    }
}
