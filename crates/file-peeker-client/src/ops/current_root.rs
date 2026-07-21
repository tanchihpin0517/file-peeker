use std::{io, path::Path};

use file_peeker_protocol::{
    ClientMessage, ServerMessage,
    io::{read_message, send_message},
};
use thiserror::Error;
use tokio::io::{AsyncBufRead, AsyncWrite};

#[derive(Clone, Debug, Eq, Error, PartialEq, uniffi::Error)]
pub enum CurrentRootError {
    #[error("current-root operation failed: {message}")]
    Operation { message: String },
}

impl From<io::Error> for CurrentRootError {
    fn from(error: io::Error) -> Self {
        Self::Operation {
            message: error.to_string(),
        }
    }
}

pub(crate) async fn current_root<S>(mut stream: S) -> io::Result<String>
where
    S: AsyncBufRead + AsyncWrite + Unpin,
{
    send_message(&mut stream, &ClientMessage::CurrentRoot).await?;
    match read_message::<ServerMessage, _>(&mut stream).await? {
        ServerMessage::CurrentRoot { path } if Path::new(&path).is_absolute() => Ok(path),
        ServerMessage::CurrentRoot { .. } => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "server returned a non-absolute current root",
        )),
        ServerMessage::Error { code, message } => Err(io::Error::other(format!(
            "server rejected current-root ({code:?}): {message}"
        ))),
        response => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("server returned unexpected current-root response: {response:?}"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::{io, path::Path};

    use file_peeker_protocol::{
        ClientMessage, ErrorCode, ServerMessage,
        io::{read_message, send_message},
    };
    use tokio::io::{BufStream, duplex};

    use super::current_root;

    async fn exchange(response: ServerMessage) -> (io::Result<String>, ClientMessage) {
        let (client, server) = duplex(4096);
        let server = tokio::spawn(async move {
            let mut server = BufStream::new(server);
            let request = read_message::<ClientMessage, _>(&mut server).await.unwrap();
            send_message(&mut server, &response).await.unwrap();
            request
        });
        let result = current_root(BufStream::new(client)).await;
        (result, server.await.unwrap())
    }

    #[tokio::test]
    async fn returns_an_absolute_current_root() {
        let (result, request) = exchange(ServerMessage::CurrentRoot {
            path: "/remote/home".into(),
        })
        .await;

        assert_eq!(request, ClientMessage::CurrentRoot);
        assert_eq!(Path::new(&result.unwrap()), Path::new("/remote/home"));
    }

    #[tokio::test]
    async fn returns_server_errors() {
        let (result, _) = exchange(ServerMessage::Error {
            code: ErrorCode::PermissionDenied,
            message: "denied".into(),
        })
        .await;

        let error = result.unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Other);
        assert!(error.to_string().contains("PermissionDenied"));
        assert!(error.to_string().contains("denied"));
    }

    #[tokio::test]
    async fn rejects_unexpected_and_non_absolute_responses() {
        let (unexpected, _) = exchange(ServerMessage::HeartbeatOk).await;
        assert_eq!(unexpected.unwrap_err().kind(), io::ErrorKind::InvalidData);

        let (relative, _) = exchange(ServerMessage::CurrentRoot {
            path: "relative/root".into(),
        })
        .await;
        assert_eq!(relative.unwrap_err().kind(), io::ErrorKind::InvalidData);
    }
}
