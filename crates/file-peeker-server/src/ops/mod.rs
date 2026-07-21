//! gRPC filesystem service implementation.

mod current_root;
mod list;

use file_peeker_protocol::v1::{
    CurrentRootRequest, CurrentRootResponse, ListBatch, ListRequest, file_peeker_server::FilePeeker,
};
use futures::stream::BoxStream;
use tokio_util::sync::CancellationToken;
use tonic::{Request, Response, Status};

#[derive(Clone, Debug)]
pub(crate) struct FilePeekerService {
    cancellation: CancellationToken,
}

impl FilePeekerService {
    pub(crate) fn new(cancellation: CancellationToken) -> Self {
        Self { cancellation }
    }
}

#[tonic::async_trait]
impl FilePeeker for FilePeekerService {
    type ListStream = BoxStream<'static, Result<ListBatch, Status>>;

    async fn current_root(
        &self,
        _request: Request<CurrentRootRequest>,
    ) -> Result<Response<CurrentRootResponse>, Status> {
        current_root::current_root().map(Response::new)
    }

    async fn list(
        &self,
        request: Request<ListRequest>,
    ) -> Result<Response<Self::ListStream>, Status> {
        let stream = list::list(request.into_inner().path, self.cancellation.clone()).await?;
        Ok(Response::new(stream))
    }
}
