//! Implementation of the `current_root` operation.

use file_peeker_protocol::{ErrorCode, ServerMessage};
use tokio::io::{AsyncRead, AsyncWrite};

use super::write_error;
use crate::{ServerError, write_server_message};

pub(super) async fn handle<S>(stream: &mut S) -> Result<(), ServerError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let path = match std::env::current_dir() {
        Ok(path) => path,
        Err(error) => return write_error(stream, ErrorCode::Io, &error.to_string()).await,
    };
    // The protocol transports paths as JSON strings, so it cannot represent a
    // platform path containing arbitrary non-UTF-8 bytes.
    let Some(path) = path.to_str() else {
        return write_error(
            stream,
            ErrorCode::InvalidPath,
            "Current directory is not valid UTF-8",
        )
        .await;
    };
    write_server_message(
        stream,
        &ServerMessage::CurrentRoot {
            path: path.to_owned(),
        },
    )
    .await
}

#[cfg(test)]
mod tests {
    use file_peeker_protocol::ServerMessage;
    use tokio::io::{AsyncReadExt, duplex};

    use super::handle;

    #[tokio::test]
    async fn current_root_returns_the_process_directory() {
        let expected = std::env::current_dir()
            .expect("current directory should be available")
            .to_str()
            .expect("test directory should be UTF-8")
            .to_owned();
        let (mut server_stream, mut client_stream) = duplex(4096);

        let server = tokio::spawn(async move { handle(&mut server_stream).await });
        let mut response = String::new();
        client_stream
            .read_to_string(&mut response)
            .await
            .expect("current-root response should be readable");
        server.await.unwrap().unwrap();

        let message: ServerMessage = serde_json::from_str(response.trim_end()).unwrap();
        assert_eq!(message, ServerMessage::CurrentRoot { path: expected });
    }
}
