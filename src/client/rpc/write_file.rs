use std::{
    io,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use crate::protocol::{
    FILE_DATA_CHUNK_MAX_BYTES,
    v1::{
        WriteCommit, WriteConflict as ProtocolWriteConflict, WriteFrame, WriteHeader,
        file_peeker_client::FilePeekerClient, write_frame,
    },
};
use bytes::Bytes;
use futures::{FutureExt as _, StreamExt as _, stream};

use super::{entry::wire_path, error::operation_status_error};
use crate::{
    client::{
        connection::ServerConnection,
        rpc::{ReadStream, WriteConflict, WriteFuture, WriteResult},
    },
    host_path::HostPath,
};

enum RequestState {
    Header {
        source: ReadStream,
        header: WriteHeader,
    },
    Data {
        source: ReadStream,
        pending: Option<Bytes>,
    },
    Done,
}

pub(crate) fn write_file(
    connection: &ServerConnection,
    directory: HostPath,
    name: HostPath,
    conflict: WriteConflict,
    source: ReadStream,
) -> WriteFuture {
    let channel = connection.channel();
    let header = WriteHeader {
        directory: directory.into_escaped(),
        name: name.into_escaped(),
        conflict: match conflict {
            WriteConflict::FailIfExists => ProtocolWriteConflict::FailIfExists as i32,
            WriteConflict::Replace => ProtocolWriteConflict::Replace as i32,
        },
    };
    let source_error = Arc::new(Mutex::new(None));
    let bytes_sent = Arc::new(AtomicU64::new(0));
    let frames = request_stream(
        source,
        header,
        Arc::clone(&source_error),
        Arc::clone(&bytes_sent),
    );
    let request = match connection.request(frames) {
        Ok(request) => request,
        Err(error) => return futures::future::ready(Err(error)).boxed(),
    };

    async move {
        let response = FilePeekerClient::new(channel).write(request).await;
        if let Some(error) = source_error.lock().unwrap().take() {
            return Err(error);
        }
        let response = response
            .map_err(|status| operation_status_error(&status))?
            .into_inner();
        if !Path::new(&response.path).is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "server returned a non-absolute write path",
            ));
        }
        let sent = bytes_sent.load(Ordering::Relaxed);
        if response.bytes_written != sent {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "server reported {} written bytes after client sent {sent}",
                    response.bytes_written
                ),
            ));
        }
        Ok(WriteResult {
            path: wire_path(response.path, "written path")?,
            bytes_written: response.bytes_written,
        })
    }
    .boxed()
}

fn request_stream(
    source: ReadStream,
    header: WriteHeader,
    source_error: Arc<Mutex<Option<io::Error>>>,
    bytes_sent: Arc<AtomicU64>,
) -> impl futures::Stream<Item = WriteFrame> + Send + 'static {
    stream::unfold(RequestState::Header { source, header }, move |state| {
        let source_error = Arc::clone(&source_error);
        let bytes_sent = Arc::clone(&bytes_sent);
        async move {
            match state {
                RequestState::Header { source, header } => Some((
                    WriteFrame {
                        payload: Some(write_frame::Payload::Header(header)),
                    },
                    RequestState::Data {
                        source,
                        pending: None,
                    },
                )),
                RequestState::Data {
                    mut source,
                    mut pending,
                } => {
                    if pending.is_none() {
                        match source.next().await {
                            Some(Ok(bytes)) if bytes.is_empty() => {
                                *source_error.lock().unwrap() = Some(io::Error::new(
                                    io::ErrorKind::InvalidData,
                                    "source emitted an empty data chunk",
                                ));
                                return None;
                            }
                            Some(Ok(bytes)) => pending = Some(bytes),
                            Some(Err(error)) => {
                                *source_error.lock().unwrap() = Some(error);
                                return None;
                            }
                            None => {
                                return Some((
                                    WriteFrame {
                                        payload: Some(write_frame::Payload::Commit(WriteCommit {})),
                                    },
                                    RequestState::Done,
                                ));
                            }
                        }
                    }
                    let mut bytes = pending.expect("pending data was populated");
                    let chunk = bytes.split_to(bytes.len().min(FILE_DATA_CHUNK_MAX_BYTES));
                    let chunk_len = u64::try_from(chunk.len()).expect("chunk limit fits u64");
                    if bytes_sent
                        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                            value.checked_add(chunk_len)
                        })
                        .is_err()
                    {
                        *source_error.lock().unwrap() =
                            Some(io::Error::other("source byte count overflowed"));
                        return None;
                    }
                    let pending = (!bytes.is_empty()).then_some(bytes);
                    Some((
                        WriteFrame {
                            payload: Some(write_frame::Payload::Data(chunk.to_vec())),
                        },
                        RequestState::Data { source, pending },
                    ))
                }
                RequestState::Done => None,
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use std::{
        io,
        sync::{
            Arc, Mutex,
            atomic::{AtomicU64, Ordering},
        },
    };

    use crate::protocol::{
        FILE_DATA_CHUNK_MAX_BYTES,
        v1::{WriteConflict, WriteHeader, write_frame},
    };
    use bytes::Bytes;
    use futures::{StreamExt as _, stream};

    use super::request_stream;

    #[tokio::test]
    async fn emits_header_split_data_and_commit() {
        let source =
            stream::iter([Ok(Bytes::from(vec![7; FILE_DATA_CHUNK_MAX_BYTES + 3]))]).boxed();
        let error = Arc::new(Mutex::new(None));
        let bytes = Arc::new(AtomicU64::new(0));
        let frames = request_stream(
            source,
            WriteHeader {
                directory: "/tmp".into(),
                name: "file".into(),
                conflict: WriteConflict::FailIfExists as i32,
            },
            Arc::clone(&error),
            Arc::clone(&bytes),
        )
        .collect::<Vec<_>>()
        .await;

        assert!(matches!(
            frames[0].payload,
            Some(write_frame::Payload::Header(_))
        ));
        assert!(matches!(
            frames[1].payload,
            Some(write_frame::Payload::Data(ref data))
                if data.len() == FILE_DATA_CHUNK_MAX_BYTES
        ));
        assert!(matches!(
            frames[2].payload,
            Some(write_frame::Payload::Data(ref data)) if data.len() == 3
        ));
        assert!(matches!(
            frames[3].payload,
            Some(write_frame::Payload::Commit(_))
        ));
        assert!(error.lock().unwrap().is_none());
        assert_eq!(
            bytes.load(Ordering::Relaxed),
            u64::try_from(FILE_DATA_CHUNK_MAX_BYTES + 3).unwrap()
        );
    }

    #[tokio::test]
    async fn source_error_ends_without_commit() {
        let source = stream::iter([Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "denied",
        ))])
        .boxed();
        let error = Arc::new(Mutex::new(None));
        let frames = request_stream(
            source,
            WriteHeader {
                directory: "/tmp".into(),
                name: "file".into(),
                conflict: WriteConflict::Replace as i32,
            },
            Arc::clone(&error),
            Arc::new(AtomicU64::new(0)),
        )
        .collect::<Vec<_>>()
        .await;

        assert_eq!(frames.len(), 1);
        assert_eq!(
            error.lock().unwrap().as_ref().unwrap().kind(),
            io::ErrorKind::PermissionDenied
        );
    }
}
