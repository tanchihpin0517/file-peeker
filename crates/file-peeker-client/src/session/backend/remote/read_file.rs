use std::{io, pin::Pin};

use bytes::Bytes;
use file_peeker_server::protocol::v1::{
    ReadChunk, ReadRequest, file_peeker_client::FilePeekerClient,
};
use futures::{Stream, StreamExt as _, stream};

use super::error::read_status_error;
use crate::session::backend::{ReadStream, connection::RemoteConnection};

pub(super) async fn read_file(connection: &RemoteConnection, path: &str) -> io::Result<ReadStream> {
    let channel = connection.channel()?;
    let request = connection.request(ReadRequest {
        path: path.to_owned(),
    })?;
    let stream = FilePeekerClient::new(channel)
        .read(request)
        .await
        .map_err(|status| read_status_error(&status))?
        .into_inner();
    Ok(network_read_stream(stream))
}

#[allow(dead_code, reason = "backend-only operation awaiting Session exposure")]
fn network_read_stream<S>(stream: S) -> ReadStream
where
    S: Stream<Item = Result<ReadChunk, tonic::Status>> + Send + 'static,
{
    stream::unfold(Some(Box::pin(stream) as Pin<Box<S>>), |state| async move {
        let mut stream = state?;
        match stream.next().await {
            Some(Ok(chunk)) if chunk.data.is_empty() => Some((
                Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "server returned an empty read chunk",
                )),
                None,
            )),
            Some(Ok(chunk)) => Some((Ok(Bytes::from(chunk.data)), Some(stream))),
            Some(Err(status)) => Some((Err(read_status_error(&status)), None)),
            None => None,
        }
    })
    .boxed()
}

#[cfg(test)]
mod tests {
    use std::io;

    use file_peeker_server::protocol::v1::ReadChunk;
    use futures::{StreamExt as _, TryStreamExt as _, stream};
    use tonic::Status;

    use super::network_read_stream;

    #[tokio::test]
    async fn network_read_stream_preserves_chunks_and_maps_terminal_errors() {
        let chunks = network_read_stream(stream::iter([
            Ok(ReadChunk {
                data: b"first".to_vec(),
            }),
            Ok(ReadChunk {
                data: b" second".to_vec(),
            }),
        ]))
        .try_collect::<Vec<_>>()
        .await
        .unwrap();
        let contents = chunks.into_iter().flatten().collect::<Vec<_>>();
        assert_eq!(contents, b"first second");

        let mut stream = network_read_stream(stream::iter([
            Ok(ReadChunk {
                data: b"kept".to_vec(),
            }),
            Err(Status::permission_denied("denied")),
            Ok(ReadChunk {
                data: b"discarded".to_vec(),
            }),
        ]));
        assert_eq!(stream.next().await.unwrap().unwrap(), b"kept".as_slice());
        let error = stream.next().await.unwrap().unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(error.to_string(), "denied");
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn network_read_stream_rejects_empty_chunks_terminally() {
        let mut stream = network_read_stream(stream::iter([
            Ok(ReadChunk { data: Vec::new() }),
            Ok(ReadChunk {
                data: b"discarded".to_vec(),
            }),
        ]));

        let error = stream.next().await.unwrap().unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(error.to_string(), "server returned an empty read chunk");
        assert!(stream.next().await.is_none());
    }
}
