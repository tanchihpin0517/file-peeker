use std::io;

use tokio::{io::BufStream, net::TcpStream};

mod common;
mod local;
mod remote;

pub use local::{LocalServer, LocalServerConfig};
pub use remote::RemoteServer;

pub type BufTcpStream = BufStream<TcpStream>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RemoteServerStartup {
    pub forward_port: u16,
    pub token: String,
}

#[allow(async_fn_in_trait)]
pub trait Server {
    type ConnectArgument;

    /// Opens this server connection.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the connection cannot be opened.
    async fn connect(&mut self, argument: Self::ConnectArgument) -> io::Result<()>;

    /// Opens a TCP connection for one file operation.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the server is not connected or the operation
    /// connection cannot be opened.
    async fn operate(&self) -> io::Result<BufTcpStream>;

    /// Gracefully shuts down the server and releases its owned processes.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the shutdown exchange fails.
    async fn shutdown(&mut self) -> io::Result<()>;
}

#[cfg(test)]
mod tests {
    use super::{LocalServer, RemoteServer, Server};

    fn assert_server<T: Server>() {}

    #[test]
    fn local_and_remote_servers_implement_server() {
        assert_server::<LocalServer>();
        assert_server::<RemoteServer>();
    }
}
