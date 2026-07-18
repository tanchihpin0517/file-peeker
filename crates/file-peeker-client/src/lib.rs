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
mod state;

pub use api::{
    Client, DirectoryEntry, DirectoryListing, EntryKind, FileMetadata, FilePeekerError, Session,
    SessionConfig, SessionTarget, State, StateRow, StateSnapshot,
};

uniffi::setup_scaffolding!();

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{Client, DirectoryListing, Session, State};

    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn exported_objects_are_thread_safe() {
        assert_send_sync::<Client>();
        assert_send_sync::<Session>();
        assert_send_sync::<State>();
        assert_send_sync::<DirectoryListing>();
        assert_send_sync::<Arc<Client>>();
        assert_send_sync::<Arc<Session>>();
        assert_send_sync::<Arc<State>>();
        assert_send_sync::<Arc<DirectoryListing>>();
    }
}
