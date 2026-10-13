//! The submitter's view of one task.

use std::{
    fmt,
    future::{Future, IntoFuture},
    io,
    pin::Pin,
    sync::Arc,
};

use tokio::sync::{mpsc, oneshot};

use super::{TaskId, TaskKind};

/// How a task that did not succeed ended.
#[derive(Clone, Debug)]
pub(crate) enum TaskError {
    Failed(Arc<io::Error>),
    Cancelled,
}

impl fmt::Display for TaskError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Failed(error) => error.fmt(f),
            Self::Cancelled => f.write_str("the task was cancelled"),
        }
    }
}

impl std::error::Error for TaskError {}

/// One task, as the code that submitted it sees it: its ID, the items it
/// emits, and how it ends.
///
/// Dropping a handle detaches: the runtime still owns the task and
/// [`super::Runtime::cancel`] still reaches it. A streaming task notices on its
/// next emit, because nothing receives its items any more, and stops there.
pub(crate) struct Handle<K: TaskKind> {
    id: TaskId,
    items: mpsc::Receiver<Vec<K::Item>>,
    outcome: oneshot::Receiver<Result<K::Output, TaskError>>,
}

impl<K: TaskKind> Handle<K> {
    pub(super) fn new(
        id: TaskId,
        items: mpsc::Receiver<Vec<K::Item>>,
        outcome: oneshot::Receiver<Result<K::Output, TaskError>>,
    ) -> Self {
        Self { id, items, outcome }
    }

    pub(crate) fn id(&self) -> TaskId {
        self.id
    }

    /// The next batch of items, or `None` once the task has stopped emitting.
    pub(crate) async fn next(&mut self) -> Option<Vec<K::Item>> {
        self.items.recv().await
    }

    /// Waits for the task to settle. Items not yet received are dropped.
    ///
    /// # Errors
    ///
    /// Returns how the task ended when it did not succeed.
    pub(crate) async fn outcome(self) -> Result<K::Output, TaskError> {
        drop(self.items);
        self.outcome.await.unwrap_or(Err(TaskError::Cancelled))
    }
}

/// A task that emits nothing is awaited like the future it replaced:
/// `runtime.submit(kind).await?`.
impl<K: TaskKind<Item = ()>> IntoFuture for Handle<K> {
    type Output = Result<K::Output, TaskError>;
    type IntoFuture = Pin<Box<dyn Future<Output = Self::Output> + Send>>;

    fn into_future(self) -> Self::IntoFuture {
        Box::pin(self.outcome())
    }
}
