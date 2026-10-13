//! The task runtime: the top of each process.
//!
//! > Nothing asynchronous happens except as a task.
//!
//! A task is submitted, admitted, run, observed and settled through one
//! lifecycle that [`Runtime`] owns. What a task *does* is a [`TaskKind`], an open
//! trait: the runtime names no filesystem type, and each layer implements the
//! kinds it owns.
//!
//! ```text
//! submit ──> Queued ──> Running ──> Succeeded
//!              │         │  ^  │
//!              │         └──┘  └──> Failed
//!              │       progress
//!              │ cancel  │ cancel
//!              v         v
//!          Cancelled <── Cancelling ──> Failed   (cleanup itself failed)
//! ```
//!
//! - [`runtime`] holds the registry, admission, the tree and retention.
//! - [`context`] is everything a kind's body can reach.
//! - [`handle`] is the submitter's view of one task.
//!
//! The runtime owns lifecycle, never payload: it stores what a task is and how
//! it is going, never what it produced. Items go to the submitter's handle and
//! are not retained.

mod context;
mod handle;
mod runtime;

use std::{fmt, future::Future, io, sync::Arc, time::SystemTime};

use uuid::Uuid;

pub(crate) use self::{
    context::{Stop, TaskContext},
    handle::{Handle, TaskError},
    runtime::{Config, Runtime},
};

/// Names one task on every side that sees it.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct TaskId(Uuid);

impl TaskId {
    fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl fmt::Display for TaskId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Names one resource tasks can wait on.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct ResourceId(Uuid);

impl ResourceId {
    fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

/// What a task does.
///
/// `run` is the whole body. It sees the runtime only through its
/// [`TaskContext`]: it watches for cancellation there, reports progress there,
/// and emits items there.
pub(crate) trait TaskKind: Send + 'static {
    /// Delivered while the task runs: entries, chunks. `()` for none.
    type Item: Send + 'static;
    /// Delivered once, on success.
    type Output: Send + 'static;

    fn describe(&self) -> TaskDescriptor;

    /// The resources this task waits on before it is admitted. The first one is
    /// also its admission scope: caps count tasks per class per that resource.
    fn requires(&self) -> Vec<ResourceId> {
        Vec::new()
    }

    fn run(
        self,
        cx: TaskContext<Self::Item>,
    ) -> impl Future<Output = io::Result<Self::Output>> + Send;
}

/// What a task is, for every observer that did not submit it. The one closed
/// shape, so a task list renders any kind without knowing it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TaskDescriptor {
    /// Stable, dotted: `fs.list_dir`.
    pub kind: &'static str,
    pub subject: Vec<Operand>,
    pub admission: AdmissionClass,
}

impl TaskDescriptor {
    pub(crate) fn new(kind: &'static str, admission: AdmissionClass) -> Self {
        Self {
            kind,
            subject: Vec::new(),
            admission,
        }
    }

    #[must_use]
    pub(crate) fn with(mut self, name: &'static str, value: impl Into<String>) -> Self {
        self.subject.push(Operand {
            name,
            value: value.into(),
        });
        self
    }
}

/// One named operand: `path = /tmp`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Operand {
    pub name: &'static str,
    pub value: String,
}

/// Which admission cap a root task counts against. A child runs under its
/// parent's admission, so a parent holding the last slot of its class can still
/// start children rather than deadlock waiting for itself.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum AdmissionClass {
    /// What the user is waiting on: listings, previews. Unbounded by default.
    Interactive,
    /// Copies. Bandwidth is shared, so a few at a time per machine.
    Transfer,
    /// Directory sizes, watches: must not starve the interactive work.
    Background,
}

#[derive(Clone, Debug)]
pub(crate) enum TaskState {
    Queued {
        waiting_on: Option<ResourceId>,
    },
    Running,
    Cancelling,
    Succeeded,
    /// Shared, because several observers read one failure: the submitter, a
    /// task list, and a status line.
    Failed(Arc<io::Error>),
    Cancelled,
}

impl TaskState {
    pub(crate) fn is_terminal(&self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed(_) | Self::Cancelled)
    }
}

/// Two independent axes: a copy is in a phase *and* has a byte count.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Progress {
    pub phase: Option<&'static str>,
    pub measure: Measure,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum Measure {
    #[default]
    Indeterminate,
    /// `total` when the count is known up front, so a client can show a
    /// fraction rather than a tally.
    Items {
        done: u64,
        total: Option<u64>,
    },
    Bytes {
        done: u64,
        total: Option<u64>,
    },
}

/// Everything the runtime knows about one task.
#[derive(Clone, Debug)]
pub(crate) struct TaskInfo {
    pub id: TaskId,
    pub parent: Option<TaskId>,
    pub descriptor: TaskDescriptor,
    pub submitted: SystemTime,
    pub settled: Option<SystemTime>,
    pub state: TaskState,
    pub progress: Progress,
}

#[derive(Clone, Debug)]
pub(crate) enum TaskEvent {
    Submitted(TaskInfo),
    StateChanged {
        id: TaskId,
        state: TaskState,
    },
    /// Coalesced: at most one per task per [`Config::progress_interval`], and
    /// the latest value always arrives before the task settles.
    Progressed {
        id: TaskId,
        progress: Progress,
    },
}

/// Where a resource is in its life.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ResourceState {
    /// Its producing work is under way; tasks requiring it queue.
    Pending,
    Ready,
    /// Being taken down; tasks requiring it queue rather than start against it.
    Closing,
    /// Closed, or never came up; tasks requiring it fail at admission.
    Gone,
}

#[cfg(test)]
mod tests;
