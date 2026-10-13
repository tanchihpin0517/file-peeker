//! The client protocol's service: the handshake, and the stream a client holds
//! for as long as it is attached, which carries the server's tasks.

use std::sync::Arc;

use tokio::sync::{broadcast, mpsc};
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;
use tonic::{Request, Response, Status};
use tracing::info;

use super::{ServerInfo, tasks::task_message};
use crate::{
    protocol::{
        CLIENT_PROTOCOL, VERSION,
        client_v1::{
            AttachRequest, AttachResponse, Hello, ServerEvent, TaskChanged, TaskSnapshot,
            WatchRequest, client_protocol_server::ClientProtocol, server_event,
        },
    },
    task::{Runtime, TaskEvent, TaskId},
};

pub(super) struct Attach {
    info: Arc<ServerInfo>,
    runtime: Runtime,
    shutdown: CancellationToken,
}

impl Attach {
    pub(super) fn new(
        info: Arc<ServerInfo>,
        runtime: Runtime,
        shutdown: CancellationToken,
    ) -> Self {
        Self {
            info,
            runtime,
            shutdown,
        }
    }
}

#[tonic::async_trait]
impl ClientProtocol for Attach {
    type WatchStream = ReceiverStream<Result<ServerEvent, Status>>;

    async fn attach(
        &self,
        request: Request<AttachRequest>,
    ) -> Result<Response<AttachResponse>, Status> {
        let request = request.into_inner();
        if request.client_protocol != CLIENT_PROTOCOL {
            info!(
                client_version = request.client_version,
                client_protocol = request.client_protocol,
                "refused a client with a different protocol"
            );
            return Err(Status::failed_precondition(format!(
                "client protocol {} does not match server protocol {CLIENT_PROTOCOL}",
                request.client_protocol
            )));
        }
        info!(client_version = request.client_version, "client attached");
        Ok(Response::new(AttachResponse {
            server_version: VERSION.to_owned(),
            client_protocol: CLIENT_PROTOCOL,
            pid: self.info.pid,
        }))
    }

    /// Sends `Hello`, then holds the stream until the client lets go or the
    /// server stops. Ending it on stop is also what lets the transport's graceful
    /// shutdown finish: it waits for open streams, and this one would never end
    /// on its own.
    async fn watch(
        &self,
        _request: Request<WatchRequest>,
    ) -> Result<Response<Self::WatchStream>, Status> {
        let (events, stream) = mpsc::channel(8);
        let hello = ServerEvent {
            event: Some(server_event::Event::Hello(Hello {})),
        };
        events
            .send(Ok(hello))
            .await
            .map_err(|_| Status::cancelled("the client left before the stream opened"))?;
        tokio::spawn(forward_tasks(
            self.runtime.clone(),
            events,
            self.shutdown.clone(),
        ));
        Ok(Response::new(ReceiverStream::new(stream)))
    }
}

/// Sends a snapshot, then every change, until the client lets go or the server
/// stops.
///
/// Subscribed before the snapshot is taken, so nothing falls between the two.
/// Each change is sent as the task is *now* rather than as the event described
/// it: an event queued behind a snapshot that already shows a later state would
/// otherwise move the client's copy backwards.
async fn forward_tasks(
    runtime: Runtime,
    events: mpsc::Sender<Result<ServerEvent, Status>>,
    shutdown: CancellationToken,
) {
    let mut changes = runtime.subscribe();
    if events.send(Ok(snapshot(&runtime))).await.is_err() {
        return;
    }
    loop {
        let change = tokio::select! {
            () = shutdown.cancelled() => return,
            () = events.closed() => {
                info!("client detached");
                return;
            }
            change = changes.recv() => change,
        };
        let event = match change {
            Ok(change) => {
                let Some(info) = runtime.task(task_of(&change)) else {
                    // Evicted since: there is nothing current to say about it.
                    continue;
                };
                server_event::Event::Changed(TaskChanged {
                    task: Some(task_message(&info)),
                })
            }
            Err(broadcast::error::RecvError::Lagged(missed)) => {
                info!(missed, "a watcher fell behind; resending the snapshot");
                let ServerEvent { event: Some(event) } = snapshot(&runtime) else {
                    continue;
                };
                event
            }
            Err(broadcast::error::RecvError::Closed) => return,
        };
        if events
            .send(Ok(ServerEvent { event: Some(event) }))
            .await
            .is_err()
        {
            return;
        }
    }
}

fn snapshot(runtime: &Runtime) -> ServerEvent {
    ServerEvent {
        event: Some(server_event::Event::Snapshot(TaskSnapshot {
            tasks: runtime.tasks().iter().map(task_message).collect(),
        })),
    }
}

fn task_of(event: &TaskEvent) -> TaskId {
    match event {
        TaskEvent::Submitted(info) => info.id,
        TaskEvent::StateChanged { id, .. } | TaskEvent::Progressed { id, .. } => *id,
    }
}
