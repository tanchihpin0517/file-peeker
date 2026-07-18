//! Operation-connection handshake and request dispatch.
//!
//! Each operation uses its own connection and carries exactly one request. This
//! keeps streamed responses ordered without adding request identifiers to the
//! protocol.

mod current_root;
mod list;

use file_peeker_protocol::{
    ClientMessage, ConnectionRole, ErrorCode, PROTOCOL_VERSION, ServerMessage,
};
use tokio::net::UnixStream;

use crate::{ServerError, read_client_message, write_server_message};

pub(crate) async fn handle(mut stream: UnixStream) -> Result<(), ServerError> {
    let message = read_client_message(&mut stream).await?;
    match message {
        ClientMessage::Hello {
            version,
            role: ConnectionRole::Operation,
        } if version == PROTOCOL_VERSION => {
            write_server_message(
                &mut stream,
                &ServerMessage::HelloOk {
                    version: PROTOCOL_VERSION,
                },
            )
            .await?;
        }
        ClientMessage::Hello { version, .. } if version != PROTOCOL_VERSION => {
            write_server_message(
                &mut stream,
                &ServerMessage::Error {
                    code: ErrorCode::UnsupportedVersion,
                    message: "Unsupported protocol version".into(),
                },
            )
            .await?;
            return Ok(());
        }
        _ => {
            return Err(ServerError::Protocol {
                message: "operation connection must begin with operation hello".into(),
            });
        }
    }

    match read_client_message(&mut stream).await? {
        ClientMessage::List { path } => list::handle(&mut stream, &path).await,
        ClientMessage::CurrentRoot => current_root::handle(&mut stream).await,
        _ => Err(ServerError::Protocol {
            message: "operation connection must contain one request".into(),
        }),
    }
}

async fn write_error(
    stream: &mut UnixStream,
    code: ErrorCode,
    message: &str,
) -> Result<(), ServerError> {
    write_server_message(
        stream,
        &ServerMessage::Error {
            code,
            message: message.into(),
        },
    )
    .await
}
