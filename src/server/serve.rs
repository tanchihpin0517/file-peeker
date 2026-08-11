use std::sync::Arc;

use crate::fs::FsService;
use crate::protocol::v1::file_peeker_server::FilePeekerServer;
use thiserror::Error;
use tokio::{net::TcpListener, task::JoinHandle};
use tokio_stream::wrappers::TcpListenerStream;
use tokio_util::sync::CancellationToken;
use tonic::{Request, Status, service::Interceptor, transport::Server};

use super::ops;

const MAX_CONCURRENT_STREAMS: u32 = 128;
const SHUTDOWN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

#[derive(Debug, Error)]
pub(crate) enum ServerError {
    #[error("server protocol error: {message}")]
    Protocol { message: String },
    #[error("server I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("gRPC server error: {0}")]
    Grpc(#[from] tonic::transport::Error),
}

/// One running File Peeker server and the credentials a client needs to reach
/// it.
///
/// The server runs on its own task, so the same code serves the standalone
/// `file-peeker-server` process and a client that embeds its own server for a
/// local Session. Both learn where to connect the same way: an ephemeral
/// loopback port plus a bearer token.
#[derive(Debug)]
pub(crate) struct ServerHandle {
    port: u16,
    token: Arc<str>,
    fs: FsService,
    shutdown: CancellationToken,
    task: JoinHandle<Result<(), ServerError>>,
}

/// Binds an ephemeral IPv4 loopback port, mints a token, and starts serving.
///
/// Returns once the listener is bound and the serving task is spawned, so a
/// caller can connect to the reported port immediately.
///
/// # Errors
///
/// Returns an error when the loopback listener cannot be bound or the
/// authentication token cannot be generated.
pub(crate) async fn start() -> Result<ServerHandle, ServerError> {
    let fs = FsService::new();
    let service = ops::FilePeekerService::new(fs.clone());
    start_with(service, fs).await
}

/// Serves `service` instead of the real filesystem adapter.
///
/// `fs` is what [`ServerHandle::shutdown`] cancels; a service that does not use
/// one can pass a throwaway. Tests use this to script protocol responses while
/// keeping the real listener, interceptor, and channel.
///
/// # Errors
///
/// Returns an error when the loopback listener cannot be bound or the
/// authentication token cannot be generated.
pub(crate) async fn start_with<S>(service: S, fs: FsService) -> Result<ServerHandle, ServerError>
where
    S: crate::protocol::v1::file_peeker_server::FilePeeker,
{
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let port = listener.local_addr()?.port();
    let token = Arc::<str>::from(generate_token()?);
    let file_peeker = tonic::service::interceptor::InterceptedService::new(
        FilePeekerServer::new(service)
            .max_encoding_message_size(ops::GRPC_BATCH_MAX_BYTES)
            .max_decoding_message_size(ops::GRPC_BATCH_MAX_BYTES),
        AuthInterceptor::new(Arc::clone(&token)),
    );
    let (health_reporter, health_service) = tonic_health::server::health_reporter();
    health_reporter.set_serving::<FilePeekerServer<S>>().await;
    let health = tonic::service::interceptor::InterceptedService::new(
        health_service,
        AuthInterceptor::new(Arc::clone(&token)),
    );

    let shutdown = CancellationToken::new();
    let serving_shutdown = shutdown.clone();
    let task = tokio::spawn(async move {
        Server::builder()
            .max_concurrent_streams(Some(MAX_CONCURRENT_STREAMS))
            .add_service(health)
            .add_service(file_peeker)
            .serve_with_incoming_shutdown(
                TcpListenerStream::new(listener),
                serving_shutdown.cancelled_owned(),
            )
            .await?;
        Ok(())
    });

    Ok(ServerHandle {
        port,
        token,
        fs,
        shutdown,
        task,
    })
}

impl ServerHandle {
    pub(crate) fn port(&self) -> u16 {
        self.port
    }

    pub(crate) fn token(&self) -> &str {
        &self.token
    }

    /// Stops the server and waits for its task to finish.
    ///
    /// The filesystem service is cancelled before the listener stops, so an
    /// in-flight listing, walk, or read fails with a `Cancelled` status rather
    /// than racing the transport into an ambiguous connection error.
    ///
    /// # Errors
    ///
    /// Returns an error when the serving task failed, panicked, or missed the
    /// bounded shutdown deadline.
    pub(crate) async fn shutdown(self) -> Result<(), ServerError> {
        self.fs.cancel();
        self.shutdown.cancel();
        match tokio::time::timeout(SHUTDOWN_TIMEOUT, self.task).await {
            Ok(Ok(result)) => result,
            Ok(Err(error)) => Err(ServerError::Protocol {
                message: format!("serving task did not finish cleanly: {error}"),
            }),
            Err(_) => Err(ServerError::Protocol {
                message: "server did not stop within 5 seconds".into(),
            }),
        }
    }

    /// Stops the server without waiting, for a drop path that cannot await.
    ///
    /// Only an embedded server needs this: the binary always shuts down from an
    /// async context.
    #[cfg(feature = "client")]
    pub(crate) fn abort(self) {
        self.fs.cancel();
        self.shutdown.cancel();
    }
}

#[derive(Clone, Debug)]
struct AuthInterceptor {
    token: Arc<str>,
}

impl AuthInterceptor {
    fn new(token: Arc<str>) -> Self {
        Self { token }
    }
}

impl Interceptor for AuthInterceptor {
    fn call(&mut self, request: Request<()>) -> Result<Request<()>, Status> {
        let candidate = request
            .metadata()
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "));
        if candidate
            .is_some_and(|candidate| constant_time_eq(candidate.as_bytes(), self.token.as_bytes()))
        {
            Ok(request)
        } else {
            Err(Status::unauthenticated("Authentication failed"))
        }
    }
}

fn generate_token() -> Result<String, ServerError> {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes).map_err(|error| ServerError::Protocol {
        message: format!("cannot generate authentication token: {error}"),
    })?;
    let mut token = String::with_capacity(64);
    for byte in bytes {
        token.push(char::from(HEX[usize::from(byte >> 4)]));
        token.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    Ok(token)
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut difference = 0_u8;
    for (&left, &right) in left.iter().zip(right) {
        difference |= left ^ right;
    }
    difference == 0
}

#[cfg(test)]
mod tests {
    use std::{path::Path, sync::Arc};

    use crate::protocol::{
        FILE_PEEKER_SERVICE_NAME,
        v1::{
            ListRequest, ReadRequest, ResolvePathRequest, WalkRequest, WriteCommit, WriteConflict,
            WriteFrame, WriteHeader, file_peeker_client::FilePeekerClient, write_frame,
        },
    };
    use futures::{TryStreamExt, stream};
    use tonic::{
        Code, Request,
        metadata::MetadataValue,
        service::Interceptor,
        transport::{Channel, Endpoint},
    };
    use tonic_health::pb::{HealthCheckRequest, health_client::HealthClient};

    use super::{AuthInterceptor, ServerHandle, constant_time_eq, start};

    #[test]
    fn token_is_256_bits_of_hex() {
        let token = super::generate_token().unwrap();
        assert_eq!(token.len(), 64);
        assert!(token.bytes().all(|byte| byte.is_ascii_hexdigit()));
    }

    #[test]
    fn authentication_is_generic_and_exact() {
        let mut interceptor = AuthInterceptor::new(Arc::from("expected-token"));
        let mut valid = Request::new(());
        valid
            .metadata_mut()
            .insert("authorization", "Bearer expected-token".parse().unwrap());
        assert!(interceptor.call(valid).is_ok());

        let error = interceptor.call(Request::new(())).unwrap_err();
        assert_eq!(error.code(), Code::Unauthenticated);
        assert_eq!(error.message(), "Authentication failed");
    }

    #[test]
    fn token_comparison_checks_every_byte() {
        assert!(constant_time_eq(b"token", b"token"));
        assert!(!constant_time_eq(b"token", b"taken"));
        assert!(!constant_time_eq(b"token", b"short"));
    }

    async fn connect(handle: &ServerHandle) -> Channel {
        Endpoint::from_shared(format!("http://127.0.0.1:{}", handle.port()))
            .unwrap()
            .connect()
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn authenticated_health_unary_and_streaming_rpcs_work_together() {
        let handle = start().await.unwrap();
        let token = handle.token().to_owned();
        let channel = connect(&handle).await;

        let health_response = HealthClient::new(channel.clone())
            .check(authenticated(
                HealthCheckRequest {
                    service: FILE_PEEKER_SERVICE_NAME.into(),
                },
                &token,
            ))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(
            health_response.status,
            tonic_health::pb::health_check_response::ServingStatus::Serving as i32
        );

        let mut client = FilePeekerClient::new(channel);
        assert_eq!(
            client
                .resolve_path(Request::new(ResolvePathRequest { path: ".".into() }))
                .await
                .unwrap_err()
                .code(),
            Code::Unauthenticated
        );

        let resolved = client
            .resolve_path(authenticated(
                ResolvePathRequest { path: ".".into() },
                &token,
            ))
            .await
            .unwrap()
            .into_inner();
        assert!(std::path::Path::new(&resolved.path).is_absolute());

        let fixture = tempfile::tempdir().unwrap();
        tokio::fs::write(fixture.path().join("entry.txt"), b"")
            .await
            .unwrap();
        let batches = client
            .list(authenticated(
                ListRequest {
                    path: fixture.path().to_string_lossy().into_owned(),
                },
                &token,
            ))
            .await
            .unwrap()
            .into_inner()
            .try_collect::<Vec<_>>()
            .await
            .unwrap();
        assert_eq!(
            batches
                .iter()
                .map(|batch| batch.entries.len())
                .sum::<usize>(),
            1
        );

        assert_walk_rpc(&mut client, fixture.path(), &token).await;
        assert_read_rpc(&mut client, fixture.path(), &token).await;
        assert_write_rpc(&mut client, fixture.path(), &token).await;

        handle.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn shutdown_releases_the_loopback_port() {
        let handle = start().await.unwrap();
        let port = handle.port();
        connect(&handle).await;

        handle.shutdown().await.unwrap();

        assert!(
            tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .is_err(),
            "a stopped server should not accept connections"
        );
    }

    #[tokio::test]
    async fn every_server_mints_its_own_port_and_token() {
        let first = start().await.unwrap();
        let second = start().await.unwrap();

        assert_ne!(first.port(), second.port());
        assert_ne!(first.token(), second.token());

        first.shutdown().await.unwrap();
        second.shutdown().await.unwrap();
    }

    async fn assert_walk_rpc(client: &mut FilePeekerClient<Channel>, fixture: &Path, token: &str) {
        tokio::fs::create_dir(fixture.join("nested")).await.unwrap();
        tokio::fs::write(fixture.join("nested/child.txt"), b"")
            .await
            .unwrap();
        let batches = client
            .walk(authenticated(
                WalkRequest {
                    path: fixture.to_string_lossy().into_owned(),
                },
                token,
            ))
            .await
            .unwrap()
            .into_inner()
            .try_collect::<Vec<_>>()
            .await
            .unwrap();
        assert!(
            batches
                .iter()
                .flat_map(|batch| batch.entries.iter())
                .any(|entry| entry.relative_path == "nested/child.txt" && entry.depth == 2)
        );
    }

    async fn assert_read_rpc(client: &mut FilePeekerClient<Channel>, fixture: &Path, token: &str) {
        let payload = b"streamed payload";
        let payload_path = fixture.join("payload.bin");
        tokio::fs::write(&payload_path, payload).await.unwrap();
        let chunks = client
            .read(authenticated(
                ReadRequest {
                    path: payload_path.to_string_lossy().into_owned(),
                },
                token,
            ))
            .await
            .unwrap()
            .into_inner()
            .try_collect::<Vec<_>>()
            .await
            .unwrap();
        assert_eq!(
            chunks
                .into_iter()
                .flat_map(|chunk| chunk.data)
                .collect::<Vec<_>>(),
            payload
        );

        let error = client
            .read(authenticated(
                ReadRequest {
                    path: fixture.to_string_lossy().into_owned(),
                },
                token,
            ))
            .await
            .expect_err("directory read should fail before streaming");
        assert_eq!(error.code(), Code::FailedPrecondition);
    }

    async fn assert_write_rpc(client: &mut FilePeekerClient<Channel>, fixture: &Path, token: &str) {
        let frames = stream::iter([
            WriteFrame {
                payload: Some(write_frame::Payload::Header(WriteHeader {
                    directory: fixture.to_string_lossy().into_owned(),
                    name: "written.bin".into(),
                    conflict: WriteConflict::FailIfExists as i32,
                })),
            },
            WriteFrame {
                payload: Some(write_frame::Payload::Data(b"streamed ".to_vec())),
            },
            WriteFrame {
                payload: Some(write_frame::Payload::Data(b"write".to_vec())),
            },
            WriteFrame {
                payload: Some(write_frame::Payload::Commit(WriteCommit {})),
            },
        ]);
        let response = client
            .write(authenticated(frames, token))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(response.bytes_written, 14);
        assert_eq!(
            tokio::fs::read(fixture.join("written.bin")).await.unwrap(),
            b"streamed write"
        );

        let incomplete = stream::iter([
            WriteFrame {
                payload: Some(write_frame::Payload::Header(WriteHeader {
                    directory: fixture.to_string_lossy().into_owned(),
                    name: "incomplete.bin".into(),
                    conflict: WriteConflict::FailIfExists as i32,
                })),
            },
            WriteFrame {
                payload: Some(write_frame::Payload::Data(b"prefix".to_vec())),
            },
        ]);
        let error = client
            .write(authenticated(incomplete, token))
            .await
            .unwrap_err();
        assert_eq!(error.code(), Code::InvalidArgument);
        assert!(!fixture.join("incomplete.bin").exists());
    }

    fn authenticated<T>(message: T, token: &str) -> Request<T> {
        let mut value = MetadataValue::try_from(format!("Bearer {token}")).unwrap();
        value.set_sensitive(true);
        let mut request = Request::new(message);
        request.metadata_mut().insert("authorization", value);
        request
    }
}
