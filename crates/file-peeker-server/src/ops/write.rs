use bytes::Bytes;
use file_peeker_core::{FsError, FsErrorKind, FsService, ReadStream, WriteConflict};
use file_peeker_server::protocol::{
    FILE_DATA_CHUNK_MAX_BYTES,
    v1::{WriteFrame, WriteResponse, write_frame},
};
use futures::{StreamExt as _, stream};
use tonic::{Request, Response, Status};

use super::status::fs_status;

pub(super) async fn write_file(
    fs: &FsService,
    request: Request<tonic::Streaming<WriteFrame>>,
) -> Result<Response<WriteResponse>, Status> {
    let mut frames = request.into_inner();
    let first = frames
        .next()
        .await
        .ok_or_else(|| Status::invalid_argument("write stream is missing a header"))?
        .map_err(stream_status)?;
    let Some(write_frame::Payload::Header(header)) = first.payload else {
        return Err(Status::invalid_argument(
            "first write frame must be a header",
        ));
    };
    let conflict = match file_peeker_server::protocol::v1::WriteConflict::try_from(header.conflict)
    {
        Ok(file_peeker_server::protocol::v1::WriteConflict::FailIfExists) => {
            WriteConflict::FailIfExists
        }
        Ok(file_peeker_server::protocol::v1::WriteConflict::Replace) => WriteConflict::Replace,
        _ => return Err(Status::invalid_argument("unknown write conflict policy")),
    };

    let source = validated_data_stream(frames);
    let result = fs
        .write_file(&header.directory, &header.name, conflict, source)
        .await
        .map_err(|error| fs_status(&error))?;
    Ok(Response::new(WriteResponse {
        path: result.path,
        bytes_written: result.bytes_written,
    }))
}

fn validated_data_stream<S>(frames: S) -> ReadStream
where
    S: futures::Stream<Item = Result<WriteFrame, Status>> + Send + Unpin + 'static,
{
    stream::unfold(Some(frames), |state| async move {
        let mut frames = state?;
        match frames.next().await {
            Some(Ok(frame)) => match frame.payload {
                Some(write_frame::Payload::Data(data))
                    if !data.is_empty() && data.len() <= FILE_DATA_CHUNK_MAX_BYTES =>
                {
                    Some((Ok(Bytes::from(data)), Some(frames)))
                }
                Some(write_frame::Payload::Data(data)) if data.is_empty() => Some((
                    Err(protocol_error("write data frames must be non-empty")),
                    None,
                )),
                Some(write_frame::Payload::Data(_)) => Some((
                    Err(protocol_error("write data frame exceeds the wire limit")),
                    None,
                )),
                Some(write_frame::Payload::Commit(_)) => match frames.next().await {
                    None => None,
                    Some(Ok(_)) => Some((
                        Err(protocol_error("write frame received after commit")),
                        None,
                    )),
                    Some(Err(status)) => Some((Err(stream_fs_error(&status)), None)),
                },
                Some(write_frame::Payload::Header(_)) => {
                    Some((Err(protocol_error("duplicate write header")), None))
                }
                None => Some((Err(protocol_error("write frame has no payload")), None)),
            },
            Some(Err(status)) => Some((Err(stream_fs_error(&status)), None)),
            None => Some((
                Err(protocol_error("write stream ended without commit")),
                None,
            )),
        }
    })
    .boxed()
}

fn protocol_error(message: &str) -> FsError {
    FsError::new(FsErrorKind::InvalidArgument, message)
}

fn stream_fs_error(status: &Status) -> FsError {
    let kind = if status.code() == tonic::Code::Cancelled {
        FsErrorKind::Cancelled
    } else {
        FsErrorKind::Internal
    };
    FsError::new(kind, status.message())
}

fn stream_status(status: Status) -> Status {
    if status.code() == tonic::Code::Cancelled {
        status
    } else {
        Status::invalid_argument(format!("cannot read write stream: {status}"))
    }
}

#[cfg(test)]
mod tests {
    use file_peeker_core::FsErrorKind;
    use file_peeker_server::protocol::v1::{WriteCommit, WriteFrame, WriteHeader, write_frame};
    use futures::{StreamExt as _, TryStreamExt as _, stream};

    use super::validated_data_stream;

    fn frame(payload: write_frame::Payload) -> WriteFrame {
        WriteFrame {
            payload: Some(payload),
        }
    }

    #[tokio::test]
    async fn accepts_data_followed_by_commit_and_exact_eof() {
        let bytes = validated_data_stream(stream::iter([
            Ok(frame(write_frame::Payload::Data(b"one".to_vec()))),
            Ok(frame(write_frame::Payload::Data(b"two".to_vec()))),
            Ok(frame(write_frame::Payload::Commit(WriteCommit {}))),
        ]))
        .try_collect::<Vec<_>>()
        .await
        .unwrap()
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
        assert_eq!(bytes, b"onetwo");
    }

    #[tokio::test]
    async fn rejects_missing_commit_and_frames_after_commit() {
        let missing = validated_data_stream(stream::iter([Ok(frame(write_frame::Payload::Data(
            b"prefix".to_vec(),
        )))]))
        .collect::<Vec<_>>()
        .await;
        assert_eq!(
            missing.last().unwrap().as_ref().unwrap_err().kind(),
            FsErrorKind::InvalidArgument
        );

        let after = validated_data_stream(stream::iter([
            Ok(frame(write_frame::Payload::Commit(WriteCommit {}))),
            Ok(frame(write_frame::Payload::Header(WriteHeader {
                directory: "/tmp".into(),
                name: "duplicate".into(),
                conflict: 1,
            }))),
        ]))
        .collect::<Vec<_>>()
        .await;
        assert_eq!(after.len(), 1);
        assert_eq!(
            after[0].as_ref().unwrap_err().kind(),
            FsErrorKind::InvalidArgument
        );
    }
}
