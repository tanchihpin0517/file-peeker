//! Operation-connection handshake and request dispatch.
//!
//! File operations use individual connections and carry exactly one request.
//! A persistent control connection carries heartbeats and the shutdown request.
//! This keeps streamed operation responses ordered without request identifiers.

mod current_root;
mod list;

use file_peeker_protocol::{ClientMessage, ErrorCode, PROTOCOL_VERSION, ServerMessage};
use tokio::io::{AsyncBufRead, AsyncRead, AsyncWrite, BufStream};
use tokio::sync::mpsc;
use tokio::time::{Duration, timeout};

use crate::{ServerError, read_client_message, write_server_message};

pub(crate) async fn handle<S>(
    stream: S,
    token: &str,
    shutdown: &mpsc::UnboundedSender<()>,
) -> Result<(), ServerError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut stream = BufStream::new(stream);
    let authentication = timeout(Duration::from_secs(5), read_client_message(&mut stream))
        .await
        .map_err(|_| ServerError::Protocol {
            message: "timed out waiting for connection authentication".into(),
        })??;
    match authentication {
        ClientMessage::Auth { token: candidate }
            if constant_time_eq(candidate.as_bytes(), token.as_bytes()) => {}
        _ => {
            write_server_message(
                &mut stream,
                &ServerMessage::Error {
                    code: ErrorCode::AuthenticationFailed,
                    message: "Authentication failed".into(),
                },
            )
            .await?;
            return Ok(());
        }
    }

    match read_client_message(&mut stream).await? {
        ClientMessage::Hello { version } if version == PROTOCOL_VERSION => {
            write_server_message(
                &mut stream,
                &ServerMessage::HelloOk {
                    version: PROTOCOL_VERSION,
                },
            )
            .await?;
            handle_control(&mut stream, shutdown).await
        }
        ClientMessage::Hello { .. } => {
            write_server_message(
                &mut stream,
                &ServerMessage::Error {
                    code: ErrorCode::UnsupportedVersion,
                    message: "Unsupported protocol version".into(),
                },
            )
            .await?;
            Ok(())
        }
        ClientMessage::List { path } => list::handle(&mut stream, &path).await,
        ClientMessage::CurrentRoot => current_root::handle(&mut stream).await,
        _ => Err(ServerError::Protocol {
            message: "authenticated connection must contain one request".into(),
        }),
    }
}

async fn handle_control<S>(
    stream: &mut S,
    shutdown: &mpsc::UnboundedSender<()>,
) -> Result<(), ServerError>
where
    S: AsyncBufRead + AsyncWrite + Unpin,
{
    loop {
        match read_client_message(stream).await? {
            ClientMessage::Heartbeat => {
                write_server_message(stream, &ServerMessage::HeartbeatOk).await?;
            }
            ClientMessage::Shutdown => {
                write_server_message(stream, &ServerMessage::ShutdownOk).await?;
                let _ = shutdown.send(());
                return Ok(());
            }
            _ => {
                return Err(ServerError::Protocol {
                    message: "control connection accepts only heartbeat or shutdown".into(),
                });
            }
        }
    }
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut difference = 0_u8;
    for (&left, &right) in left.iter().zip(right) {
        difference |= left ^ right;
    }
    difference == 0
}

async fn write_error<S>(stream: &mut S, code: ErrorCode, message: &str) -> Result<(), ServerError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    write_server_message(
        stream,
        &ServerMessage::Error {
            code,
            message: message.into(),
        },
    )
    .await
}

#[cfg(test)]
mod tests {
    use file_peeker_protocol::{ClientMessage, ErrorCode, PROTOCOL_VERSION, ServerMessage};
    use tokio::{
        io::duplex,
        io::{AsyncReadExt as _, AsyncWriteExt as _},
    };

    async fn round_trip(messages: &[ClientMessage]) -> Vec<ServerMessage> {
        let (server_stream, mut client) = duplex(4096);
        let (shutdown, _shutdown_receiver) = tokio::sync::mpsc::unbounded_channel();
        let server =
            tokio::spawn(
                async move { super::handle(server_stream, "expected-token", &shutdown).await },
            );
        for message in messages {
            let mut bytes = serde_json::to_vec(message).unwrap();
            bytes.push(b'\n');
            client.write_all(&bytes).await.unwrap();
        }
        let mut bytes = Vec::new();
        client.read_to_end(&mut bytes).await.unwrap();
        server.await.unwrap().unwrap();
        bytes
            .split(|byte| *byte == b'\n')
            .filter(|frame| !frame.is_empty())
            .map(|frame| serde_json::from_slice(frame).unwrap())
            .collect()
    }

    #[tokio::test]
    async fn invalid_tokens_are_rejected_generically() {
        let responses = round_trip(&[ClientMessage::Auth {
            token: "wrong-token".into(),
        }])
        .await;
        assert!(matches!(
            responses.as_slice(),
            [ServerMessage::Error {
                code: ErrorCode::AuthenticationFailed,
                message,
            }] if message == "Authentication failed"
        ));
    }

    #[tokio::test]
    async fn control_connection_handles_heartbeats_and_shutdown() {
        let responses = round_trip(&[
            ClientMessage::Auth {
                token: "expected-token".into(),
            },
            ClientMessage::Hello {
                version: PROTOCOL_VERSION,
            },
            ClientMessage::Heartbeat,
            ClientMessage::Heartbeat,
            ClientMessage::Shutdown,
        ])
        .await;
        assert_eq!(
            responses,
            [
                ServerMessage::HelloOk {
                    version: PROTOCOL_VERSION,
                },
                ServerMessage::HeartbeatOk,
                ServerMessage::HeartbeatOk,
                ServerMessage::ShutdownOk,
            ]
        );
    }

    #[tokio::test]
    async fn authenticated_operation_does_not_require_hello() {
        let responses = round_trip(&[
            ClientMessage::Auth {
                token: "expected-token".into(),
            },
            ClientMessage::CurrentRoot,
        ])
        .await;

        assert!(matches!(
            responses.as_slice(),
            [ServerMessage::CurrentRoot { path }] if !path.is_empty()
        ));
    }

    #[tokio::test]
    async fn missing_auth_is_rejected_generically() {
        let responses = round_trip(&[ClientMessage::Hello {
            version: PROTOCOL_VERSION,
        }])
        .await;

        assert!(matches!(
            responses.as_slice(),
            [ServerMessage::Error {
                code: ErrorCode::AuthenticationFailed,
                message,
            }] if message == "Authentication failed"
        ));
    }

    #[tokio::test]
    async fn unsupported_control_version_is_rejected() {
        let responses = round_trip(&[
            ClientMessage::Auth {
                token: "expected-token".into(),
            },
            ClientMessage::Hello {
                version: PROTOCOL_VERSION + 1,
            },
        ])
        .await;

        assert!(matches!(
            responses.as_slice(),
            [ServerMessage::Error {
                code: ErrorCode::UnsupportedVersion,
                message,
            }] if message == "Unsupported protocol version"
        ));
    }
}
