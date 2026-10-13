//! The client protocol's service. Step 1 carries only the handshake.

use std::sync::Arc;

use tonic::{Request, Response, Status};
use tracing::info;

use super::ServerInfo;
use crate::protocol::{
    CLIENT_PROTOCOL, VERSION,
    client_v1::{AttachRequest, AttachResponse, client_protocol_server::ClientProtocol},
};

pub(super) struct Attach {
    info: Arc<ServerInfo>,
}

impl Attach {
    pub(super) fn new(info: Arc<ServerInfo>) -> Self {
        Self { info }
    }
}

#[tonic::async_trait]
impl ClientProtocol for Attach {
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
}
