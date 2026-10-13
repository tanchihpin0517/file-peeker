//! Everything a kind's body can reach: the whole contract between a kind and
//! the runtime.

use std::{any::Any, sync::Arc};

use tokio::sync::mpsc;
use tokio_util::sync::{CancellationToken, WaitForCancellationFuture};

use super::{Handle, Progress, ResourceId, TaskId, TaskKind, runtime::Runtime};

/// Why [`TaskContext::emit`] could not deliver: either nothing receives the
/// items any more, or the task was cancelled while it waited. Either way the
/// body should stop producing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Stop;

pub(crate) struct TaskContext<I> {
    runtime: Runtime,
    id: TaskId,
    token: CancellationToken,
    items: mpsc::Sender<Vec<I>>,
}

impl<I: Send + 'static> TaskContext<I> {
    pub(super) fn new(
        runtime: Runtime,
        id: TaskId,
        token: CancellationToken,
        items: mpsc::Sender<Vec<I>>,
    ) -> Self {
        Self {
            runtime,
            id,
            token,
            items,
        }
    }

    pub(crate) fn id(&self) -> TaskId {
        self.id
    }

    /// Resolves when this task, or any ancestor, is cancelled. Bodies select on
    /// it at their natural cancellation points.
    pub(crate) fn cancelled(&self) -> WaitForCancellationFuture<'_> {
        self.token.cancelled()
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.token.is_cancelled()
    }

    /// Records progress. Observers see it coalesced: a body may call this per
    /// entry without flooding them.
    pub(crate) fn progress(&self, progress: Progress) {
        self.runtime.report_progress(self.id, progress);
    }

    /// Hands a batch to the submitter. The channel is bounded, so awaiting this
    /// is backpressure: a slow reader slows the task rather than buffering
    /// without limit.
    ///
    /// # Errors
    ///
    /// Returns [`Stop`] when nothing receives the items any more, or when the
    /// task is cancelled while waiting for room.
    pub(crate) async fn emit(&self, items: Vec<I>) -> Result<(), Stop> {
        tokio::select! {
            sent = self.items.send(items) => sent.map_err(|_| Stop),
            () = self.token.cancelled() => Err(Stop),
        }
    }

    /// Starts a child. It is cancelled with this task, and this task does not
    /// settle until it has.
    pub(crate) fn spawn<K: TaskKind>(&self, kind: K) -> Handle<K> {
        self.runtime.submit_child(self.id, &self.token, kind)
    }

    /// The value of a resource this task declared in `requires`, if it is
    /// ready and holds a value of that type.
    pub(crate) fn resource<R: Any + Send + Sync>(&self, id: ResourceId) -> Option<Arc<R>> {
        self.runtime.resource_value(id)
    }
}
