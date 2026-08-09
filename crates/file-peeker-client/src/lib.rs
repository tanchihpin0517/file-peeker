mod client;
pub mod session;

pub use client::Client;
pub use session::directory::{DirectoryEntry, EntryKind, EntryStream};
pub use session::directory::{WalkEntry, WalkStream};
pub use session::{
    CopyConflict, CopyResult, Session, SessionShutdownError, SessionStartError, SessionTarget,
};

#[cfg(test)]
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
