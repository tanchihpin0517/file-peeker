mod client;
mod ops;
mod session;

pub use client::Client;
pub use ops::list::ListStream;
pub use ops::{DirectoryEntry, EntryKind, ListError, Listing};
pub mod server;
pub use session::{CloseError, ConnectError, Session, SessionConfig, SessionTarget};

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
