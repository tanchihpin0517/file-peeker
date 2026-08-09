use std::sync::Arc;

use file_peeker_client::{Session, SessionTarget};
use uuid::Uuid;

use crate::browser_context::BrowserContextId;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct SessionRecordId(Uuid);

impl SessionRecordId {
    pub(crate) fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum SessionStatus {
    Starting,
    Ready,
    Failed(String),
    Closing,
}

#[derive(Debug)]
pub(crate) struct SessionRecord {
    pub(crate) id: SessionRecordId,
    pub(crate) target: SessionTarget,
    pub(crate) label: String,
    pub(crate) status: SessionStatus,
    pub(crate) session: Option<Arc<Session>>,
    pub(crate) last_active_context_id: Option<BrowserContextId>,
}

impl SessionRecord {
    pub(crate) fn target_summary(&self) -> String {
        match &self.target {
            SessionTarget::Local => "Local".into(),
            SessionTarget::Remote { destination } => format!("SSH {destination}"),
        }
    }
}
