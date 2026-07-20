use file_peeker_protocol::{ClientMessage, ServerMessage, read_message, send_message};

use super::{connection_read_error, connection_write_error, map_server_error};
use crate::{FilePeekerError, session::Session};

pub(crate) async fn current_root(session: &Session) -> Result<String, FilePeekerError> {
    let mut connection = session.connect().await?;
    if let Err(error) = send_message(&mut connection, &ClientMessage::CurrentRoot)
        .await
        .map_err(|error| connection_write_error(&error))
    {
        session.fail_terminal(&error);
        return Err(error);
    }

    let response = match read_message::<ServerMessage, _>(&mut connection)
        .await
        .map_err(|error| connection_read_error(&error))
    {
        Ok(response) => response,
        Err(error) => {
            session.fail_terminal(&error);
            return Err(error);
        }
    };
    session.mark_activity();

    let result = match response {
        ServerMessage::CurrentRoot { path } => Ok(path),
        ServerMessage::Error { code, message } => Err(map_server_error(code, message)),
        message => Err(FilePeekerError::Protocol {
            message: format!("unexpected current-root response: {message:?}"),
        }),
    };
    if let Err(error) = &result {
        session.fail_terminal(error);
    }
    result
}
