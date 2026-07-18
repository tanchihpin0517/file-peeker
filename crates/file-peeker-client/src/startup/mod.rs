use std::{
    future::Future,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use tokio::{
    sync::{Notify, mpsc},
    time::timeout,
};

use crate::{FilePeekerError, SessionConfig, SessionTarget};

mod diagnostics;
mod local;
mod protocol;
mod remote;
mod runtime;
mod ssh;

pub(super) const STARTUP_TIMEOUT: Duration = Duration::from_secs(5);
pub(super) const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);
pub(super) const CONNECT_RETRY_DELAY: Duration = Duration::from_millis(100);

#[derive(Debug)]
pub(super) struct LifecycleHandle {
    shutdown: mpsc::UnboundedSender<()>,
    socket_path: PathBuf,
    closed: Arc<AtomicBool>,
    closed_notify: Arc<Notify>,
}

impl LifecycleHandle {
    fn spawn<F, Fut>(socket_path: PathBuf, build_supervisor: F) -> Self
    where
        F: FnOnce(mpsc::UnboundedReceiver<()>) -> Fut,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let (shutdown, shutdown_receiver) = mpsc::unbounded_channel();
        let closed = Arc::new(AtomicBool::new(false));
        let closed_notify = Arc::new(Notify::new());
        let supervisor = build_supervisor(shutdown_receiver);
        let supervisor_closed = Arc::clone(&closed);
        let supervisor_notify = Arc::clone(&closed_notify);
        tokio::spawn(async move {
            supervisor.await;
            supervisor_closed.store(true, Ordering::Release);
            supervisor_notify.notify_waiters();
        });
        Self {
            shutdown,
            socket_path,
            closed,
            closed_notify,
        }
    }

    pub(super) fn shutdown(&self) {
        let _ = self.shutdown.send(());
    }

    pub(super) async fn close(&self) -> Result<(), FilePeekerError> {
        if self.is_closed() {
            return Ok(());
        }
        let notified = self.closed_notify.notified();
        self.shutdown();
        if self.is_closed() {
            return Ok(());
        }
        timeout(SHUTDOWN_TIMEOUT + Duration::from_secs(1), notified)
            .await
            .map_err(|_| FilePeekerError::ConnectionClosed {
                message: "timed out waiting for server shutdown".into(),
            })
    }

    pub(super) fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    pub(super) fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }
}

pub(super) async fn start(config: SessionConfig) -> Result<LifecycleHandle, FilePeekerError> {
    match config.target {
        SessionTarget::Local {
            server_executable_path,
        } => local::start(server_executable_path).await,
        SessionTarget::Ssh { destination } => remote::start(destination).await,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    #[test]
    fn lifecycle_timeouts_are_bounded() {
        assert!(super::STARTUP_TIMEOUT <= Duration::from_secs(10));
        assert!(super::SHUTDOWN_TIMEOUT <= Duration::from_secs(5));
        assert_eq!(super::CONNECT_RETRY_DELAY, Duration::from_millis(100));
    }
}
