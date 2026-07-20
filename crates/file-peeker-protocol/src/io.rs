use std::io;

use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};

use crate::Message;

/// Writes one newline-delimited protocol message.
///
/// # Errors
///
/// Returns an I/O error when serialization or writing fails.
pub async fn send_message<M, W>(stream: &mut W, message: &M) -> io::Result<()>
where
    M: Message,
    W: AsyncWrite + Unpin,
{
    let request = serde_json::to_vec(message).map_err(io::Error::other)?;
    stream.write_all(&request).await?;
    stream.write_all(b"\n").await?;
    stream.flush().await
}

/// Reads and returns one newline-delimited protocol message.
///
/// # Errors
///
/// Returns an I/O error when reading fails or the frame is not a valid message
/// of the requested type.
pub async fn read_message<M, R>(reader: &mut R) -> io::Result<M>
where
    M: Message,
    R: AsyncBufRead + Unpin,
{
    let mut line = String::new();
    let bytes_read = reader.read_line(&mut line).await?;

    if !line.ends_with('\n') {
        let message = if bytes_read == 0 {
            "connection closed before a message"
        } else {
            "connection closed before a complete line"
        };
        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, message));
    }

    serde_json::from_str(&line).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

#[cfg(test)]
mod tests {
    use crate::{ClientMessage, ServerMessage};
    use tokio::io::{AsyncReadExt as _, duplex};

    use super::{read_message, send_message};

    #[tokio::test]
    async fn sends_any_newline_delimited_message() {
        let (mut writer, mut reader) = duplex(256);

        send_message(&mut writer, &ClientMessage::Heartbeat)
            .await
            .unwrap();
        send_message(&mut writer, &ServerMessage::HeartbeatOk)
            .await
            .unwrap();
        drop(writer);
        let mut output = Vec::new();
        reader.read_to_end(&mut output).await.unwrap();

        assert_eq!(
            output,
            b"{\"type\":\"heartbeat\"}\n{\"type\":\"heartbeat_ok\"}\n"
        );
    }

    #[tokio::test]
    async fn reads_and_returns_the_requested_message_type() {
        let mut server_input = &b"{\"type\":\"heartbeat_ok\"}\n{\"type\":\"shutdown_ok\"}\n"[..];

        assert_eq!(
            read_message::<ServerMessage, _>(&mut server_input)
                .await
                .unwrap(),
            ServerMessage::HeartbeatOk
        );
        assert_eq!(
            read_message::<ServerMessage, _>(&mut server_input)
                .await
                .unwrap(),
            ServerMessage::ShutdownOk
        );

        let mut client_input = &b"{\"type\":\"heartbeat\"}\n"[..];
        assert_eq!(
            read_message::<ClientMessage, _>(&mut client_input)
                .await
                .unwrap(),
            ClientMessage::Heartbeat
        );
    }

    #[tokio::test]
    async fn rejects_empty_incomplete_and_invalid_frames() {
        let mut empty = tokio::io::empty();
        assert_eq!(
            read_message::<ServerMessage, _>(&mut empty)
                .await
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::UnexpectedEof
        );
        let mut invalid = &b"not-json\n"[..];
        assert_eq!(
            read_message::<ServerMessage, _>(&mut invalid)
                .await
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::InvalidData
        );
        let mut incomplete = &b"{\"type\":\"heartbeat_ok\"}"[..];
        assert_eq!(
            read_message::<ServerMessage, _>(&mut incomplete)
                .await
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::UnexpectedEof
        );
        let mut invalid_utf8 = &[0xff, b'\n'][..];
        assert_eq!(
            read_message::<ServerMessage, _>(&mut invalid_utf8)
                .await
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::InvalidData
        );
    }

    #[tokio::test]
    async fn reads_and_writes_messages_larger_than_one_mib() {
        let expected = ServerMessage::Error {
            code: crate::ErrorCode::Io,
            message: "x".repeat(1024 * 1024 + 1),
        };
        let mut encoded = Vec::new();
        send_message(&mut encoded, &expected).await.unwrap();
        assert!(encoded.len() > 1024 * 1024);
        let mut input = encoded.as_slice();

        assert_eq!(
            read_message::<ServerMessage, _>(&mut input).await.unwrap(),
            expected
        );
    }
}
