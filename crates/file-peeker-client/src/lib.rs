mod client;
pub mod session;

pub use client::{Client, ClientCloseSessionError};
pub use session::directory::{DirectoryEntry, EntryKind, EntryStream};
pub use session::directory::{WalkEntry, WalkStream};
pub use session::ffi::{ListError, Listing, OpenFileError, ResolvePathError};
pub use session::{Session, SessionShutdownError, SessionStartError, SessionTarget};

uniffi::setup_scaffolding!();

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{Client, Listing, Session};

    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn exported_objects_are_thread_safe() {
        assert_send_sync::<Client>();
        assert_send_sync::<Listing>();
        assert_send_sync::<Session>();
        assert_send_sync::<Arc<Client>>();
        assert_send_sync::<Arc<Listing>>();
        assert_send_sync::<Arc<Session>>();
    }
}
