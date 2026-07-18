//! Private wire types for the File Peeker server protocol.
//!
//! The protocol is specified now so the server and client can compile against the
//! same contract. Encoding, decoding, framing, and transport are intentionally
//! not implemented in the v1 skeleton.

use serde::{Deserialize, Serialize};

/// The only protocol version understood by the v1 skeleton.
pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionRole {
    Control,
    Operation,
}

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
    Hello { version: u32, role: ConnectionRole },
    List { path: String },
    CurrentRoot,
    GetMetadata { path: String },
}

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

#[cfg(test)]
mod tests {
    use super::{
        ClientMessage, ConnectionRole, EntryKind, ListingEntry, PROTOCOL_VERSION, ServerMessage,
    };

    #[test]
    fn hello_shape_matches_protocol_document() {
        let message = ClientMessage::Hello {
            version: PROTOCOL_VERSION,
            role: ConnectionRole::Control,
        };

        let json = serde_json::to_string(&message).expect("hello should serialize");

        assert_eq!(json, r#"{"type":"hello","version":1,"role":"control"}"#);
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
}
