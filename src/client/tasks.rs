//! A server's tasks, as a client holds them: owned values decoded from the
//! client protocol, with nothing of the runtime's own types.

use crate::protocol::client_v1::{self, progress};

/// One task on a server.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TaskView {
    pub id: String,
    pub parent: Option<String>,
    pub kind: String,
    pub subject: Vec<(String, String)>,
    pub status: TaskStatus,
    pub phase: Option<String>,
    pub measure: Measure,
    pub submitted_unix_millis: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum TaskStatus {
    /// Waiting on a resource when `on_resource`, on its class's cap otherwise.
    Queued {
        on_resource: bool,
    },
    Running,
    Cancelling,
    Succeeded,
    Failed(String),
    Cancelled,
}

impl TaskStatus {
    pub(crate) fn is_terminal(&self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed(_) | Self::Cancelled)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Measure {
    Indeterminate,
    Items { done: u64, total: Option<u64> },
    Bytes { done: u64, total: Option<u64> },
}

impl From<client_v1::Task> for TaskView {
    fn from(task: client_v1::Task) -> Self {
        let status = match client_v1::TaskState::try_from(task.state) {
            Ok(client_v1::TaskState::Queued) => TaskStatus::Queued {
                on_resource: task.waiting_on_resource,
            },
            Ok(client_v1::TaskState::Running) => TaskStatus::Running,
            Ok(client_v1::TaskState::Cancelling) => TaskStatus::Cancelling,
            Ok(client_v1::TaskState::Succeeded) => TaskStatus::Succeeded,
            Ok(client_v1::TaskState::Failed) => TaskStatus::Failed(task.error),
            Ok(client_v1::TaskState::Cancelled) => TaskStatus::Cancelled,
            // A state this build does not know is a newer server's; the
            // protocol version check makes it unreachable in practice.
            Ok(client_v1::TaskState::Unspecified) | Err(_) => {
                TaskStatus::Queued { on_resource: false }
            }
        };
        let progress = task.progress.unwrap_or_default();
        let measure = match progress.measure {
            None => Measure::Indeterminate,
            Some(progress::Measure::Items(count)) => Measure::Items {
                done: count.done,
                total: count.total,
            },
            Some(progress::Measure::Bytes(count)) => Measure::Bytes {
                done: count.done,
                total: count.total,
            },
        };
        Self {
            id: task.id,
            parent: Some(task.parent).filter(|parent| !parent.is_empty()),
            kind: task.kind,
            subject: task
                .subject
                .into_iter()
                .map(|operand| (operand.name, operand.value))
                .collect(),
            status,
            phase: Some(progress.phase).filter(|phase| !phase.is_empty()),
            measure,
            submitted_unix_millis: task.submitted_unix_millis,
        }
    }
}

/// What a `Watch` stream says about tasks.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum TaskUpdate {
    /// Every task the server retains, replacing what was held.
    Snapshot(Vec<TaskView>),
    /// One task as it is now.
    Changed(TaskView),
}
