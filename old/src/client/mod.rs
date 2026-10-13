use std::{
    collections::{HashMap, hash_map::Entry},
    io,
    sync::Arc,
};

use tokio::sync::{OwnedRwLockReadGuard, RwLock};
use uuid::Uuid;

use self::connection::{ServerConnection, ServerLocation};

pub(crate) mod connection;
pub(crate) mod directory;
pub(crate) mod file;
mod path;
pub(crate) mod rpc;

pub(crate) use file::copy::{CopyConflict, CopyResult};

/// Names one connection a Client holds. Minted by [`Client::open`].
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct ConnectionId(Uuid);

impl ConnectionId {
    fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

/// One open connection. `None` once it is closed.
///
/// Each connection carries its own lock rather than sharing the registry's, so
/// one connection's in-flight request never delays another's close.
type ConnectionSlot = Arc<RwLock<Option<ServerConnection>>>;

/// The connection this crate's operations run against, borrowed for the length
/// of one request.
type ConnectionGuard = OwnedRwLockReadGuard<Option<ServerConnection>, ServerConnection>;

/// Entry point and owner for independent File Peeker connections.
#[derive(Debug, Default)]
pub(crate) struct Client {
    connections: RwLock<HashMap<ConnectionId, ConnectionSlot>>,
}

impl Client {
    #[must_use]
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Opens and retains a connection to a server at `location`, returning its
    /// ID.
    ///
    /// The location only selects where the server runs and is not retained: the
    /// TUI records its own copy before startup, which is the only place it is
    /// displayed.
    ///
    /// The Client retains the connection, so a caller that forgets the returned
    /// ID still leaves shutdown to `close_all`.
    ///
    /// # Errors
    ///
    /// Returns the underlying I/O error when the server cannot be started,
    /// reached, or authenticated.
    pub(crate) async fn open(&self, location: ServerLocation) -> io::Result<ConnectionId> {
        let connection = ServerConnection::open(location).await?;

        let id = ConnectionId::new();
        let mut connections = self.connections.write().await;
        match connections.entry(id) {
            Entry::Vacant(entry) => {
                entry.insert(Arc::new(RwLock::new(Some(connection))));
                Ok(id)
            }
            Entry::Occupied(_) => {
                drop(connections);
                let _ = connection.shutdown().await;
                Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "generated duplicate connection UUID",
                ))
            }
        }
    }

    /// Borrows an open connection, or fails when it is closed or unknown.
    ///
    /// The registry lock is held only long enough to clone the slot, so a slow
    /// request never blocks another connection's close.
    ///
    /// The returned guard holds the connection read lock, so callers must drop
    /// it before an owned stream is handed back to a caller. Closing the
    /// connection then cancels in-flight work instead of waiting on it.
    async fn connection(&self, id: &ConnectionId) -> io::Result<ConnectionGuard> {
        let slot = self
            .connections
            .read()
            .await
            .get(id)
            .map(Arc::clone)
            .ok_or_else(connection_closed)?;
        OwnedRwLockReadGuard::try_map(slot.read_owned().await, Option::as_ref)
            .map_err(|_| connection_closed())
    }

    /// Removes and gracefully closes the connection with the requested ID.
    /// Unknown and repeated IDs succeed.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the managed server does not exit cleanly.
    pub(crate) async fn close(&self, id: &ConnectionId) -> io::Result<()> {
        let Some(slot) = self.connections.write().await.remove(id) else {
            return Ok(());
        };
        close_slot(slot).await
    }

    /// Removes and gracefully closes every retained connection. Repeated calls
    /// succeed.
    ///
    /// # Errors
    ///
    /// Returns the first close error after attempting every connection.
    pub(crate) async fn close_all(&self) -> io::Result<()> {
        let slots: Vec<_> = self
            .connections
            .write()
            .await
            .drain()
            .map(|(_, slot)| slot)
            .collect();
        let mut first_error = None;
        for slot in slots {
            if let Err(error) = close_slot(slot).await
                && first_error.is_none()
            {
                first_error = Some(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }
}

/// Empties a slot and stops the server it held. An already empty slot succeeds.
///
/// Emptying the slot is what makes a close terminal: a request that cloned this
/// slot before the registry entry was removed is rejected rather than reaching a
/// server that is going away.
async fn close_slot(slot: ConnectionSlot) -> io::Result<()> {
    let Some(connection) = slot.write().await.take() else {
        return Ok(());
    };
    connection.shutdown().await
}

fn connection_closed() -> io::Error {
    io::Error::new(io::ErrorKind::NotConnected, "connection is closed")
}

#[cfg(test)]
impl Client {
    /// Builds a Client holding one connection, and returns its ID.
    #[must_use]
    pub(crate) fn with_connection_for_test(
        connection: ServerConnection,
    ) -> (Arc<Self>, ConnectionId) {
        Self::for_test(Some(connection))
    }

    /// Builds a Client holding a registered connection whose slot is already
    /// empty, as it is after a completed close.
    #[must_use]
    pub(crate) fn closed_for_test() -> (Arc<Self>, ConnectionId) {
        Self::for_test(None)
    }

    fn for_test(connection: Option<ServerConnection>) -> (Arc<Self>, ConnectionId) {
        let id = ConnectionId::new();
        let client = Arc::new(Self {
            connections: RwLock::new(HashMap::from([(
                id,
                Arc::new(RwLock::new(connection)) as ConnectionSlot,
            )])),
        });
        (client, id)
    }
}

#[cfg(test)]
mod tests {
    use crate::host_path::HostPath;
    use futures::TryStreamExt as _;

    use super::{Client, ConnectionId, ServerLocation};

    async fn open_local(client: &Client) -> ConnectionId {
        client
            .open(ServerLocation::Embedded)
            .await
            .expect("local startup should succeed")
    }

    #[tokio::test]
    async fn opens_and_retains_a_local_connection() {
        let fixture = tempfile::tempdir().unwrap();
        tokio::fs::write(fixture.path().join("entry.txt"), b"")
            .await
            .unwrap();
        let client = Client::new();
        let id = open_local(&client).await;

        assert!(
            client
                .op_resolve_path(&id, &"~".into())
                .await
                .unwrap()
                .to_path_buf()
                .unwrap()
                .is_absolute()
        );
        let entries = client
            .op_list_dir(&id, &HostPath::from_path(fixture.path()))
            .await
            .unwrap()
            .try_collect::<Vec<_>>()
            .await
            .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "entry.txt");
        client
            .close(&id)
            .await
            .expect("local shutdown should succeed");
    }

    #[tokio::test]
    async fn empty_remote_destination_is_rejected_without_retention() {
        let client = Client::new();
        let error = client
            .open(ServerLocation::Ssh {
                destination: String::new(),
                force_install: false,
            })
            .await
            .expect_err("an empty remote destination should fail");

        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains("remote server is required"));
        assert!(client.connections.read().await.is_empty());
    }

    #[tokio::test]
    async fn retains_and_removes_connections_by_id() {
        let client = Client::new();
        let id = open_local(&client).await;

        assert!(client.connections.read().await.contains_key(&id));
        client.close(&id).await.unwrap();
        assert!(client.connections.read().await.is_empty());
    }

    #[tokio::test]
    async fn closing_unknown_or_repeated_ids_succeeds() {
        let client = Client::new();

        client.close(&ConnectionId::new()).await.unwrap();
        let id = open_local(&client).await;
        client.close(&id).await.unwrap();
        client.close(&id).await.unwrap();
    }

    #[tokio::test]
    async fn concurrent_close_is_idempotent() {
        let client = Client::new();
        let id = open_local(&client).await;

        let (first, second) = tokio::join!(client.close(&id), client.close(&id));

        first.unwrap();
        second.unwrap();
    }

    /// Close empties the connection slot before shutting the server down, so a
    /// closed connection rejects later operations rather than reaching a server
    /// that is going away.
    #[tokio::test]
    async fn close_is_terminal() {
        let client = Client::new();
        let id = open_local(&client).await;

        client.close(&id).await.unwrap();

        assert_eq!(
            client
                .op_resolve_path(&id, &".".into())
                .await
                .unwrap_err()
                .to_string(),
            "connection is closed"
        );
        client.close(&id).await.unwrap();
    }

    #[tokio::test]
    async fn close_all_drains_every_retained_connection() {
        let client = Client::new();
        let first = open_local(&client).await;
        let second = open_local(&client).await;
        assert_ne!(first, second);

        client.close_all().await.unwrap();

        assert!(client.connections.read().await.is_empty());
        client.close_all().await.unwrap();
    }

    #[test]
    fn generated_connection_ids_are_unique() {
        assert_ne!(ConnectionId::new(), ConnectionId::new());
    }
}
