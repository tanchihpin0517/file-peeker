//! Private wire types for the File Peeker server protocol.
//!
//! The server and client compile against the same private protocol contract.
//! Transport-specific process I/O lives in the runtime crates; shared async
//! message framing helpers are available in [`io`].

use serde::{Deserialize, Serialize, de::DeserializeOwned};

pub mod io;

/// A serializable message exchanged by the File Peeker client and server.
pub trait Message: Serialize + DeserializeOwned {}

/// The only protocol version understood by this client and server.
pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    File,
    Directory,
    Symlink,
    Other,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    AuthenticationFailed,
    NotFound,
    PermissionDenied,
    NotDirectory,
    InvalidPath,
    Io,
    UnsupportedVersion,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    Auth { token: String },
    Hello { version: u32 },
    Heartbeat,
    Shutdown,
    List { path: String },
    CurrentRoot,
    GetMetadata { path: String },
}

impl Message for ClientMessage {}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ListingEntry {
    pub name: String,
    pub kind: EntryKind,
    pub navigable: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    HelloOk {
        version: u32,
    },
    HeartbeatOk,
    ShutdownOk,
    ListBatch {
        entries: Vec<ListingEntry>,
    },
    ListEnd,
    Metadata {
        path: String,
        kind: EntryKind,
        size: u64,
        readonly: bool,
        modified: Option<String>,
    },
    CurrentRoot {
        path: String,
    },
    Error {
        code: ErrorCode,
        message: String,
    },
}

impl Message for ServerMessage {}

#[cfg(test)]
mod tests {
    use super::{ClientMessage, EntryKind, ListingEntry, Message, PROTOCOL_VERSION, ServerMessage};

    fn assert_message<T: Message>() {}

    #[test]
    fn client_and_server_messages_implement_message() {
        assert_message::<ClientMessage>();
        assert_message::<ServerMessage>();
    }

    #[test]
    fn hello_shape_matches_protocol_document() {
        let auth = ClientMessage::Auth {
            token: "secret".into(),
        };
        let hello = ClientMessage::Hello {
            version: PROTOCOL_VERSION,
        };

        let auth_json = serde_json::to_string(&auth).expect("auth should serialize");
        let hello_json = serde_json::to_string(&hello).expect("hello should serialize");

        assert_eq!(auth_json, r#"{"type":"auth","token":"secret"}"#);
        assert_eq!(hello_json, r#"{"type":"hello","version":1}"#);
    }

    #[test]
    fn list_batch_shape_matches_protocol_document() {
        let message = ServerMessage::ListBatch {
            entries: vec![ListingEntry {
                name: "docs".into(),
                kind: EntryKind::Directory,
                navigable: true,
            }],
        };

        let json = serde_json::to_string(&message).expect("list result should serialize");

        assert_eq!(
            json,
            r#"{"type":"list_batch","entries":[{"name":"docs","kind":"directory","navigable":true}]}"#
        );
    }

    #[test]
    fn list_end_shape_matches_protocol_document() {
        let message = ServerMessage::ListEnd;

        let json = serde_json::to_string(&message).expect("list end should serialize");

        assert_eq!(json, r#"{"type":"list_end"}"#);
    }

    #[test]
    fn control_shapes_match_protocol_document() {
        assert_eq!(
            serde_json::to_string(&ClientMessage::Heartbeat).unwrap(),
            r#"{"type":"heartbeat"}"#
        );
        assert_eq!(
            serde_json::to_string(&ClientMessage::Shutdown).unwrap(),
            r#"{"type":"shutdown"}"#
        );
        assert_eq!(
            serde_json::to_string(&ServerMessage::ShutdownOk).unwrap(),
            r#"{"type":"shutdown_ok"}"#
        );
    }
}
