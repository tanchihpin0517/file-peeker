//! Thin gRPC adapter for the transport-independent filesystem service.

mod entry;
mod list;
mod read;
mod status;
mod walk;
mod write;

use crate::fs::FsService;
use crate::protocol::v1::{
    ListBatch, ListRequest, ReadChunk, ReadRequest, ResolvePathRequest, ResolvePathResponse,
    WalkBatch, WalkRequest, WriteFrame, WriteResponse, file_peeker_server::FilePeeker,
};
use futures::stream::BoxStream;
use tonic::{Request, Response, Status};

use self::{
    list::list_batches,
    read::read_chunks,
    status::{fs_status, wire_path},
    walk::walk_batches,
};

/// The core-to-wire entry conversion, reused by the scripted server in
/// `test_support` so a canned listing is encoded exactly as a real one is.
#[cfg(test)]
pub(crate) use entry::convert_entry;

pub(crate) const GRPC_BATCH_MAX_BYTES: usize = crate::protocol::GRPC_MESSAGE_MAX_BYTES;
pub(crate) const GRPC_BATCH_MAX_ENTRIES: usize = 1024;

#[derive(Clone, Debug)]
pub(crate) struct FilePeekerService {
    fs: FsService,
}

impl FilePeekerService {
    pub(crate) fn new(fs: FsService) -> Self {
        Self { fs }
    }
}

#[tonic::async_trait]
impl FilePeeker for FilePeekerService {
    type ListStream = BoxStream<'static, Result<ListBatch, Status>>;
    type ReadStream = BoxStream<'static, Result<ReadChunk, Status>>;
    type WalkStream = BoxStream<'static, Result<WalkBatch, Status>>;

    async fn resolve_path(
        &self,
        request: Request<ResolvePathRequest>,
    ) -> Result<Response<ResolvePathResponse>, Status> {
        let path = wire_path(request.into_inner().path)?;
        self.fs
            .resolve_path(&path)
            .map(|path| {
                Response::new(ResolvePathResponse {
                    path: path.into_escaped(),
                })
            })
            .map_err(|error| fs_status(&error))
    }

    async fn list(
        &self,
        request: Request<ListRequest>,
    ) -> Result<Response<Self::ListStream>, Status> {
        let path = wire_path(request.into_inner().path)?;
        let stream = self
            .fs
            .list_dir(&path)
            .await
            .map_err(|error| fs_status(&error))?;
        Ok(Response::new(list_batches(stream)))
    }

    async fn read(
        &self,
        request: Request<ReadRequest>,
    ) -> Result<Response<Self::ReadStream>, Status> {
        let path = wire_path(request.into_inner().path)?;
        let stream = self
            .fs
            .read_file(&path)
            .await
            .map_err(|error| fs_status(&error))?;
        Ok(Response::new(read_chunks(stream)))
    }

    async fn walk(
        &self,
        request: Request<WalkRequest>,
    ) -> Result<Response<Self::WalkStream>, Status> {
        let path = wire_path(request.into_inner().path)?;
        let stream = self
            .fs
            .walk_dir(&path)
            .await
            .map_err(|error| fs_status(&error))?;
        Ok(Response::new(walk_batches(stream)))
    }

    async fn write(
        &self,
        request: Request<tonic::Streaming<WriteFrame>>,
    ) -> Result<Response<WriteResponse>, Status> {
        write::write_file(&self.fs, request).await
    }
}
