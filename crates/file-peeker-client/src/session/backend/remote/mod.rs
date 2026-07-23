mod entry;
mod error;
mod list_dir;
mod read_file;
mod resolve_path;
mod walk_dir;

use std::io;

use async_trait::async_trait;

use super::{ReadStream, SessionBackend, connection::RemoteConnection};
use crate::{EntryStream, WalkStream};

#[derive(Debug)]
pub(crate) struct RemoteBackend {
    connection: RemoteConnection,
}

impl RemoteBackend {
    pub(crate) async fn connect(destination: &str, force_install: bool) -> io::Result<Self> {
        Ok(Self {
            connection: RemoteConnection::from(destination, force_install).await?,
        })
    }
}

#[async_trait]
impl SessionBackend for RemoteBackend {
    async fn resolve_path(&self, path: &str) -> io::Result<String> {
        resolve_path::resolve_path(&self.connection, path).await
    }

    async fn list_dir(&self, path: &str) -> io::Result<EntryStream> {
        list_dir::list_dir(&self.connection, path).await
    }

    async fn walk_dir(&self, path: &str) -> io::Result<WalkStream> {
        walk_dir::walk_dir(&self.connection, path).await
    }

    async fn read_file(&self, path: &str) -> io::Result<ReadStream> {
        read_file::read_file(&self.connection, path).await
    }

    async fn close(self: Box<Self>) -> io::Result<()> {
        let Self { connection } = *self;
        connection.close().await
    }
}

#[cfg(test)]
mod tests {
    use super::RemoteBackend;

    #[tokio::test]
    async fn rejects_empty_destination_on_every_attempt() {
        for _ in 0..2 {
            assert_eq!(
                RemoteBackend::connect("", false).await.unwrap_err().kind(),
                std::io::ErrorKind::InvalidInput
            );
        }
    }
}
