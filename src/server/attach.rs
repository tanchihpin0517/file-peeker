//! The client protocol's service: the handshake, and the stream a client holds
//! for as long as it is attached.

use std::sync::Arc;

use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;
use tonic::{Request, Response, Status};
use tracing::info;

use super::ServerInfo;
use crate::protocol::{
    CLIENT_PROTOCOL, VERSION,
    client_v1::{
        AttachRequest, AttachResponse, Hello, ServerEvent, WatchRequest,
        client_protocol_server::ClientProtocol, server_event,
    },
};

pub(super) struct Attach {
    info: Arc<ServerInfo>,
    shutdown: CancellationToken,
}

impl Attach {
    pub(super) fn new(info: Arc<ServerInfo>, shutdown: CancellationToken) -> Self {
        Self { info, shutdown }
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
        let shutdown = self.shutdown.clone();
        tokio::spawn(async move {
            tokio::select! {
                () = shutdown.cancelled() => {}
                () = events.closed() => info!("client detached"),
            }
        });
        Ok(Response::new(ReceiverStream::new(stream)))
    }
}
