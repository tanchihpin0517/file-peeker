use std::io;

use file_peeker_protocol::{
    ClientMessage, ListingEntry, ServerMessage,
    io::{read_message, send_message},
};
use futures::{
    StreamExt,
    stream::{BoxStream, try_unfold},
};
use tokio::io::{AsyncBufRead, AsyncWrite};

pub type ListStream = BoxStream<'static, io::Result<ListingEntry>>;

struct ListState<S> {
    stream: S,
    pending: std::vec::IntoIter<ListingEntry>,
}

pub(crate) async fn list<S>(mut stream: S, path: &str) -> io::Result<ListStream>
where
    S: AsyncBufRead + AsyncWrite + Send + Unpin + 'static,
{
    send_message(
        &mut stream,
        &ClientMessage::List {
            path: path.to_owned(),
        },
    )
    .await?;

    Ok(try_unfold(
        ListState {
            stream,
            pending: Vec::new().into_iter(),
        },
        |mut state| async move {
            loop {
                if let Some(entry) = state.pending.next() {
                    return Ok(Some((entry, state)));
                }

                match read_message::<ServerMessage, _>(&mut state.stream).await? {
                    ServerMessage::ListBatch { entries } if entries.is_empty() => {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "server returned an empty listing batch",
                        ));
                    }
                    ServerMessage::ListBatch { entries } => {
                        state.pending = entries.into_iter();
                    }
                    ServerMessage::ListEnd => return Ok(None),
                    ServerMessage::Error { code, message } => {
                        return Err(io::Error::other(format!(
                            "server rejected list ({code:?}): {message}"
                        )));
                    }
                    message => {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!("server returned unexpected list response: {message:?}"),
                        ));
                    }
                }
            }
        },
    )
    .boxed())
}

#[cfg(test)]
mod tests {
    use std::io;

    use file_peeker_protocol::{
        ClientMessage, EntryKind, ErrorCode, ListingEntry, ServerMessage,
        io::{read_message, send_message},
    };
    use futures::TryStreamExt;
    use tokio::{
        io::{BufStream, DuplexStream, duplex},
        sync::oneshot,
    };

    use super::list;

    async fn exchange(
        path: &str,
        responses: Vec<ServerMessage>,
    ) -> (io::Result<Vec<ListingEntry>>, ClientMessage) {
        let (client, server) = duplex(4096);
        let server = tokio::spawn(async move {
            let mut server = BufStream::new(server);
            let request = read_message::<ClientMessage, _>(&mut server).await.unwrap();
            for response in responses {
                send_message(&mut server, &response).await.unwrap();
            }
            request
        });
        let result = match list(BufStream::new(client), path).await {
            Ok(entries) => entries.try_collect().await,
            Err(error) => Err(error),
        };
        let request = server.await.unwrap();
        (result, request)
    }

    fn entry(name: &str, kind: EntryKind, navigable: bool) -> ListingEntry {
        ListingEntry {
            name: name.into(),
            kind,
            navigable,
        }
    }

    #[tokio::test]
    async fn sends_path_and_collects_all_batches_in_order() {
        let first = entry("notes.txt", EntryKind::File, false);
        let second = entry("docs", EntryKind::Directory, true);
        let (result, request) = exchange(
            "/fixture",
            vec![
                ServerMessage::ListBatch {
                    entries: vec![first.clone()],
                },
                ServerMessage::ListBatch {
                    entries: vec![second.clone()],
                },
                ServerMessage::ListEnd,
            ],
        )
        .await;

        assert_eq!(
            request,
            ClientMessage::List {
                path: "/fixture".into()
            }
        );
        assert_eq!(result.unwrap(), vec![first, second]);
    }

    #[tokio::test]
    async fn yields_entries_before_list_end() {
        let expected = entry("notes.txt", EntryKind::File, false);
        let (client, server) = duplex(4096);
        let (release_server, wait_for_release) = oneshot::channel();
        let server_entry = expected.clone();
        let server = tokio::spawn(async move {
            let mut server = BufStream::new(server);
            let request = read_message::<ClientMessage, _>(&mut server).await.unwrap();
            send_message(
                &mut server,
                &ServerMessage::ListBatch {
                    entries: vec![server_entry],
                },
            )
            .await
            .unwrap();
            wait_for_release.await.unwrap();
            send_message(&mut server, &ServerMessage::ListEnd)
                .await
                .unwrap();
            request
        });
        let mut entries = list(BufStream::new(client), "/fixture").await.unwrap();

        assert_eq!(entries.try_next().await.unwrap(), Some(expected));
        release_server.send(()).unwrap();
        assert_eq!(entries.try_next().await.unwrap(), None);
        assert_eq!(
            server.await.unwrap(),
            ClientMessage::List {
                path: "/fixture".into()
            }
        );
    }

    #[tokio::test]
    async fn list_end_without_batches_returns_an_empty_list() {
        let (result, _) = exchange("relative/path", vec![ServerMessage::ListEnd]).await;

        assert!(result.unwrap().is_empty());
    }

    #[tokio::test]
    async fn rejects_empty_batches() {
        let (result, _) = exchange(
            "/fixture",
            vec![ServerMessage::ListBatch { entries: vec![] }],
        )
        .await;

        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn returns_server_errors() {
        let (result, _) = exchange(
            "/missing",
            vec![ServerMessage::Error {
                code: ErrorCode::NotFound,
                message: "directory is missing".into(),
            }],
        )
        .await;

        let error = result.unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Other);
        assert_eq!(
            error.to_string(),
            "server rejected list (NotFound): directory is missing"
        );
    }

    #[tokio::test]
    async fn rejects_unexpected_responses() {
        let (result, _) = exchange("/fixture", vec![ServerMessage::HeartbeatOk]).await;

        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn rejects_connection_close_before_list_end() {
        let (client, server) = duplex(4096);
        let server = tokio::spawn(read_request_and_close(server));
        let entries = list(BufStream::new(client), "/fixture").await.unwrap();
        let error = entries.try_collect::<Vec<_>>().await.unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
        assert_eq!(
            server.await.unwrap(),
            ClientMessage::List {
                path: "/fixture".into()
            }
        );
    }

    async fn read_request_and_close(server: DuplexStream) -> ClientMessage {
        let mut server = BufStream::new(server);
        read_message(&mut server).await.unwrap()
    }
}
