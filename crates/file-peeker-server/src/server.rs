use std::{io::Write as _, sync::Arc};

use file_peeker_core::FsService;
use file_peeker_server::protocol::v1::file_peeker_server::FilePeekerServer;
use thiserror::Error;
use tokio::{
    io::{AsyncReadExt as _, stdin},
    net::TcpListener,
    sync::oneshot,
};
use tokio_stream::wrappers::TcpListenerStream;
use tokio_util::sync::CancellationToken;
use tonic::{Request, Status, service::Interceptor, transport::Server};

use crate::ops;

const MAX_CONCURRENT_STREAMS: u32 = 128;
const SERVER_STARTUP_PREFIX: &str = "FILE_PEEKER_SERVER_STARTUP=";

#[derive(Debug, Error)]
pub(crate) enum ServerError {
    #[error("server protocol error: {message}")]
    Protocol { message: String },
    #[error("server I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("gRPC server error: {0}")]
    Grpc(#[from] tonic::transport::Error),
}

pub(crate) async fn serve() -> Result<(), ServerError> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let port = listener.local_addr()?.port();
    let token = Arc::<str>::from(generate_token()?);
    let cancellation_token = CancellationToken::new();
    let fs = FsService::new();
    let service = ops::FilePeekerService::new(fs.clone());
    let file_peeker = tonic::service::interceptor::InterceptedService::new(
        FilePeekerServer::new(service).max_encoding_message_size(ops::GRPC_BATCH_MAX_BYTES),
        AuthInterceptor::new(Arc::clone(&token)),
    );
    let (health_reporter, health_service) = tonic_health::server::health_reporter();
    health_reporter
        .set_serving::<FilePeekerServer<ops::FilePeekerService>>()
        .await;
    let health = tonic::service::interceptor::InterceptedService::new(
        health_service,
        AuthInterceptor::new(Arc::clone(&token)),
    );

    println!(
        "{SERVER_STARTUP_PREFIX}{}",
        serde_json::json!({ "port": port, "token": token.as_ref() })
    );
    std::io::stdout().flush()?;

    let (shutdown_result_sender, shutdown_result_receiver) = oneshot::channel();
    let server_cancellation_token = cancellation_token.clone();
    let fs_to_cancel = fs.clone();
    let shutdown = async move {
        let result = wait_for_shutdown().await;
        fs_to_cancel.cancel();
        server_cancellation_token.cancel();
        let _ = shutdown_result_sender.send(result);
    };

    Server::builder()
        .max_concurrent_streams(Some(MAX_CONCURRENT_STREAMS))
        .add_service(health)
        .add_service(file_peeker)
        .serve_with_incoming_shutdown(TcpListenerStream::new(listener), shutdown)
        .await?;

    shutdown_result_receiver
        .await
        .map_err(|_| ServerError::Protocol {
            message: "shutdown monitor stopped without a result".into(),
        })?
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

async fn wait_for_shutdown() -> Result<(), ServerError> {
    let mut stdin = stdin();
    let mut probe = [0_u8; 1];
    tokio::select! {
        result = stdin.read(&mut probe) => match result? {
            0 => Ok(()),
            _ => Err(ServerError::Protocol {
                message: "server stdin is a lifetime lease and does not accept data".into(),
            }),
        },
        result = termination_signal() => result,
    }
}

#[cfg(unix)]
async fn termination_signal() -> Result<(), ServerError> {
    use tokio::signal::unix::{SignalKind, signal};
    let mut terminate = signal(SignalKind::terminate())?;
    let mut interrupt = signal(SignalKind::interrupt())?;
    tokio::select! {
        _ = terminate.recv() => Ok(()),
        _ = interrupt.recv() => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use std::{path::Path, sync::Arc};

    use file_peeker_core::FsService;
    use file_peeker_server::protocol::{
        FILE_PEEKER_SERVICE_NAME,
        v1::{
            ListRequest, ReadRequest, ResolvePathRequest, file_peeker_client::FilePeekerClient,
            file_peeker_server::FilePeekerServer,
        },
    };
    use futures::TryStreamExt;
    use tokio::net::TcpListener;
    use tokio_stream::wrappers::TcpListenerStream;
    use tokio_util::sync::CancellationToken;
    use tonic::{
        Code, Request,
        metadata::MetadataValue,
        service::Interceptor,
        transport::{Channel, Endpoint, Server},
    };
    use tonic_health::pb::{HealthCheckRequest, health_client::HealthClient};

    use super::{AuthInterceptor, constant_time_eq};
    use crate::ops;

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

    #[tokio::test]
    async fn authenticated_health_unary_and_streaming_rpcs_work_together() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let cancellation_token = CancellationToken::new();
        let fs = FsService::new();
        let service = ops::FilePeekerService::new(fs.clone());
        let token: Arc<str> = Arc::from("expected-token");
        let file_peeker =
            FilePeekerServer::with_interceptor(service, AuthInterceptor::new(Arc::clone(&token)));
        let (health_reporter, health_service) = tonic_health::server::health_reporter();
        health_reporter
            .set_serving::<FilePeekerServer<ops::FilePeekerService>>()
            .await;
        let health = tonic::service::interceptor::InterceptedService::new(
            health_service,
            AuthInterceptor::new(Arc::clone(&token)),
        );
        let server_cancellation_token = cancellation_token.clone();
        let server = tokio::spawn(async move {
            Server::builder()
                .add_service(health)
                .add_service(file_peeker)
                .serve_with_incoming_shutdown(
                    TcpListenerStream::new(listener),
                    server_cancellation_token.cancelled_owned(),
                )
                .await
                .unwrap();
        });

        let channel = Endpoint::from_shared(format!("http://{address}"))
            .unwrap()
            .connect()
            .await
            .unwrap();
        let health_response = HealthClient::new(channel.clone())
            .check(authenticated(HealthCheckRequest {
                service: FILE_PEEKER_SERVICE_NAME.into(),
            }))
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
            .resolve_path(authenticated(ResolvePathRequest { path: ".".into() }))
            .await
            .unwrap()
            .into_inner();
        assert!(std::path::Path::new(&resolved.path).is_absolute());

        let fixture = tempfile::tempdir().unwrap();
        tokio::fs::write(fixture.path().join("entry.txt"), b"")
            .await
            .unwrap();
        let batches = client
            .list(authenticated(ListRequest {
                path: fixture.path().to_string_lossy().into_owned(),
            }))
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

        assert_read_rpc(&mut client, fixture.path()).await;

        cancellation_token.cancel();
        fs.cancel();
        server.await.unwrap();
    }

    async fn assert_read_rpc(client: &mut FilePeekerClient<Channel>, fixture: &Path) {
        let payload = b"streamed payload";
        let payload_path = fixture.join("payload.bin");
        tokio::fs::write(&payload_path, payload).await.unwrap();
        let chunks = client
            .read(authenticated(ReadRequest {
                path: payload_path.to_string_lossy().into_owned(),
            }))
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
            .read(authenticated(ReadRequest {
                path: fixture.to_string_lossy().into_owned(),
            }))
            .await
            .expect_err("directory read should fail before streaming");
        assert_eq!(error.code(), Code::FailedPrecondition);
    }

    fn authenticated<T>(message: T) -> Request<T> {
        let mut value = MetadataValue::try_from("Bearer expected-token").unwrap();
        value.set_sensitive(true);
        let mut request = Request::new(message);
        request.metadata_mut().insert("authorization", value);
        request
    }
}
