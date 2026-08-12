use crate::{ConnectionId, ServerLocation};
use uuid::Uuid;

use crate::tui::browser_context::BrowserContextId;

/// Names one row of the connection manager.
///
/// Distinct from [`ConnectionId`]: a row exists in `Starting` before the Client
/// has a connection to name, and outlives a failed open that never produced one.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct ConnectionRecordId(Uuid);

impl ConnectionRecordId {
    pub(crate) fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ConnectionStatus {
    Starting,
    Ready,
    Failed(String),
    Closing,
}

#[derive(Debug)]
pub(crate) struct ConnectionRecord {
    pub(crate) id: ConnectionRecordId,
    pub(crate) target: ServerLocation,
    pub(crate) label: String,
    pub(crate) status: ConnectionStatus,
    pub(crate) connection: Option<ConnectionId>,
    pub(crate) last_active_context_id: Option<BrowserContextId>,
}

impl ConnectionRecord {
    pub(crate) fn target_summary(&self) -> String {
        match &self.target {
            ServerLocation::Embedded => "Local".into(),
            ServerLocation::Ssh { destination, .. } => format!("SSH {destination}"),
        }
    }
}
