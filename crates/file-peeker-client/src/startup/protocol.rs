use std::{io::ErrorKind, path::Path};

use file_peeker_protocol::{ClientMessage, ConnectionRole, PROTOCOL_VERSION, ServerMessage};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::UnixStream,
    process::Child,
    time::{Instant, sleep, timeout_at},
};

use super::diagnostics::server_exited_error;
use super::{CONNECT_RETRY_DELAY, STARTUP_TIMEOUT};
use crate::FilePeekerError;

pub(super) async fn connect_control(
    child: &mut Child,
    socket_path: &Path,
    deadline: Instant,
) -> Result<UnixStream, FilePeekerError> {
    loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|error| FilePeekerError::ServerStart {
                message: format!("cannot inspect server process: {error}"),
            })?
        {
            return Err(server_exited_error(status, None));
        }
        if Instant::now() >= deadline {
            return Err(FilePeekerError::ServerStart {
                message: format!(
                    "timed out after {} ms waiting for the server socket",
                    STARTUP_TIMEOUT.as_millis()
                ),
            });
        }

        match UnixStream::connect(socket_path).await {
            Ok(stream) => return Ok(stream),
            Err(error)
                if matches!(
                    error.kind(),
                    ErrorKind::NotFound | ErrorKind::ConnectionRefused
                ) =>
            {
                sleep(CONNECT_RETRY_DELAY).await;
            }
            Err(error) => {
                return Err(FilePeekerError::ServerStart {
                    message: format!("cannot connect to server socket: {error}"),
                });
            }
        }
    }
}

pub(super) async fn complete_handshake(
    child: &mut Child,
    stream: &mut UnixStream,
    deadline: Instant,
) -> Result<(), FilePeekerError> {
    let handshake = handshake_control(stream);
    tokio::pin!(handshake);

    tokio::select! {
        result = timeout_at(deadline, &mut handshake) => {
            result.map_err(|_| FilePeekerError::ServerStart {
                message: format!(
                    "timed out after {} ms during the control handshake",
                    STARTUP_TIMEOUT.as_millis()
                ),
            })?
        }
        status = child.wait() => {
            let status = status.map_err(|error| FilePeekerError::ServerStart {
                message: format!("cannot wait for server process: {error}"),
            })?;
            Err(server_exited_error(status, None))
        }
    }
}

async fn handshake_control(stream: &mut UnixStream) -> Result<(), FilePeekerError> {
    let hello = ClientMessage::Hello {
        version: PROTOCOL_VERSION,
        role: ConnectionRole::Control,
    };
    let mut bytes = serde_json::to_vec(&hello).map_err(|error| FilePeekerError::Protocol {
        message: format!("cannot encode control hello: {error}"),
    })?;
    bytes.push(b'\n');
    stream
        .write_all(&bytes)
        .await
        .map_err(|error| FilePeekerError::ConnectionClosed {
            message: format!("cannot send control hello: {error}"),
        })?;
    stream
        .flush()
        .await
        .map_err(|error| FilePeekerError::ConnectionClosed {
            message: format!("cannot flush control hello: {error}"),
        })?;

    let response = read_server_message(stream).await?;
    match response {
        ServerMessage::HelloOk { version } if version == PROTOCOL_VERSION => Ok(()),
        ServerMessage::HelloOk { version } => Err(FilePeekerError::Protocol {
            message: format!("server accepted unexpected protocol version {version}"),
        }),
        ServerMessage::Error { code, message } => Err(FilePeekerError::Protocol {
            message: format!("server rejected control handshake ({code:?}): {message}"),
        }),
        response => Err(FilePeekerError::Protocol {
            message: format!("unexpected control handshake response: {response:?}"),
        }),
    }
}

async fn read_server_message(stream: &mut UnixStream) -> Result<ServerMessage, FilePeekerError> {
    let mut bytes = Vec::new();
    loop {
        let mut byte = [0_u8; 1];
        let count =
            stream
                .read(&mut byte)
                .await
                .map_err(|error| FilePeekerError::ConnectionClosed {
                    message: format!("cannot read control handshake: {error}"),
                })?;
        if count == 0 {
            return Err(FilePeekerError::ConnectionClosed {
                message: "server closed during the control handshake".into(),
            });
        }
        if byte[0] == b'\n' {
            break;
        }
        bytes.push(byte[0]);
    }

    serde_json::from_slice(&bytes).map_err(|error| FilePeekerError::Protocol {
        message: format!("server returned invalid JSON: {error}"),
    })
}

#[cfg(test)]
mod tests {
    use file_peeker_protocol::{ErrorCode, ServerMessage};
    use tokio::{io::AsyncWriteExt as _, net::UnixStream};

    #[tokio::test]
    async fn control_reader_does_not_impose_a_frame_limit() {
        let message = ServerMessage::Error {
            code: ErrorCode::Io,
            message: "x".repeat(1024 * 1024 + 1),
        };
        let mut bytes = serde_json::to_vec(&message).expect("large response should encode");
        assert!(bytes.len() > 1024 * 1024);
        bytes.push(b'\n');
        let (mut client_stream, mut server_stream) =
            UnixStream::pair().expect("socket pair should be created");

        let writer = tokio::spawn(async move { server_stream.write_all(&bytes).await });
        let response = super::read_server_message(&mut client_stream)
            .await
            .expect("server owns frame-size enforcement");
        writer
            .await
            .expect("writer task should complete")
            .expect("large fixture should be written");

        assert_eq!(response, message);
    }
}
