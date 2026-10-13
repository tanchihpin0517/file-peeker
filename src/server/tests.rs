//! The server run in-process, reached through the same client code `fpk` uses.

use std::{fs, os::unix::fs::PermissionsExt as _, time::Duration};

use tempfile::TempDir;
use tokio::{net::UnixListener, task::JoinHandle};
use tokio_util::sync::CancellationToken;
use tonic::Code;

use super::{ServerError, serve};
use crate::{
    client::{
        self, ClientError,
        api::{self, ApiClient},
    },
    home::Home,
    protocol::{
        CLIENT_PROTOCOL, VERSION,
        api::{API_GENERATION, ERROR_UNKNOWN_METHOD},
        client_v1::{AttachRequest, client_protocol_client::ClientProtocolClient},
    },
};

struct Running {
    _root: TempDir,
    home: Home,
    shutdown: CancellationToken,
    task: JoinHandle<Result<(), ServerError>>,
}

impl Running {
    async fn start() -> Self {
        let root = tempfile::tempdir().unwrap();
        let home = Home::at(root.path().join("home"));
        let (shutdown, task) = spawn(&home);
        wait_for_socket(&home).await;
        Self {
            _root: root,
            home,
            shutdown,
            task,
        }
    }

    async fn stop(self) -> Result<(), ServerError> {
        self.shutdown.cancel();
        self.task.await.unwrap()
    }
}

fn spawn(home: &Home) -> (CancellationToken, JoinHandle<Result<(), ServerError>>) {
    let shutdown = CancellationToken::new();
    let task = tokio::spawn({
        let home = home.clone();
        let shutdown = shutdown.clone();
        async move { serve(&home, shutdown).await }
    });
    (shutdown, task)
}

async fn wait_for_socket(home: &Home) {
    for _ in 0..200 {
        if client::is_running(home).await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the server never opened its client socket");
}

#[tokio::test]
async fn status_describes_the_running_server() {
    let server = Running::start().await;
    let status = ApiClient::connect(&server.home)
        .await
        .unwrap()
        .status()
        .await
        .unwrap();
    assert_eq!(status.version, VERSION);
    assert_eq!(status.pid, std::process::id());
    assert_eq!(status.client_protocol, CLIENT_PROTOCOL);
    assert_eq!(status.api_generation, API_GENERATION);
    assert_eq!(status.home, server.home.dir());
    server.stop().await.unwrap();
}

#[tokio::test]
async fn an_unknown_method_is_an_error_not_a_hangup() {
    let server = Running::start().await;
    let mut api = ApiClient::connect(&server.home).await.unwrap();
    match api.call("server.nope").await {
        Err(api::ApiClientError::Api(error)) => assert_eq!(error.code, ERROR_UNKNOWN_METHOD),
        other => panic!("expected an API error, got {other:?}"),
    }
    // The connection survives it.
    api.status().await.unwrap();
    server.stop().await.unwrap();
}

#[tokio::test]
async fn attach_completes_the_handshake() {
    let server = Running::start().await;
    let attached = client::attach(&server.home).await.unwrap();
    assert_eq!(attached.server_version, VERSION);
    assert_eq!(attached.pid, std::process::id());
    server.stop().await.unwrap();
}

#[tokio::test]
async fn attach_refuses_a_different_protocol() {
    let server = Running::start().await;
    let mut raw = ClientProtocolClient::new(client::channel(&server.home).await.unwrap());
    let status = raw
        .attach(AttachRequest {
            client_version: "9.9.9".to_owned(),
            client_protocol: CLIENT_PROTOCOL + 1,
        })
        .await
        .unwrap_err();
    assert_eq!(status.code(), Code::FailedPrecondition);
    assert!(status.message().contains("does not match"), "{status:?}");
    server.stop().await.unwrap();
}

#[tokio::test]
async fn stop_through_the_api_ends_the_server_and_removes_its_sockets() {
    let server = Running::start().await;
    assert!(api::stop(&server.home).await.unwrap());
    server.task.await.unwrap().unwrap();
    assert!(!server.home.api_socket().exists());
    assert!(!server.home.client_socket().exists());
    // Stopping again finds nothing to stop.
    assert!(!api::stop(&server.home).await.unwrap());
}

#[tokio::test]
async fn a_second_server_loses_the_lock() {
    let server = Running::start().await;
    let error = serve(&server.home, CancellationToken::new())
        .await
        .unwrap_err();
    assert!(matches!(error, ServerError::AlreadyRunning(_)), "{error}");
    // The first one is untouched by the attempt.
    client::attach(&server.home).await.unwrap();
    server.stop().await.unwrap();
}

#[tokio::test]
async fn a_stopped_server_releases_the_lock_for_the_next() {
    let Running {
        _root: root,
        home,
        shutdown,
        task,
    } = Running::start().await;
    shutdown.cancel();
    task.await.unwrap().unwrap();

    let (shutdown, task) = spawn(&home);
    wait_for_socket(&home).await;
    client::attach(&home).await.unwrap();
    shutdown.cancel();
    task.await.unwrap().unwrap();
    drop(root);
}

#[tokio::test]
async fn stale_sockets_from_a_crashed_server_are_replaced() {
    let root = tempfile::tempdir().unwrap();
    let home = Home::at(root.path().join("home"));
    home.create().unwrap();
    // A listener dropped without unlinking leaves its socket file behind, as a
    // crashed server's would.
    drop(UnixListener::bind(home.api_socket()).unwrap());
    drop(UnixListener::bind(home.client_socket()).unwrap());
    assert!(!client::is_running(&home).await);

    let (shutdown, task) = spawn(&home);
    wait_for_socket(&home).await;
    client::attach(&home).await.unwrap();
    shutdown.cancel();
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn the_sockets_are_private() {
    let server = Running::start().await;
    for socket in [server.home.api_socket(), server.home.client_socket()] {
        let mode = fs::metadata(&socket).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "{}", socket.display());
    }
    server.stop().await.unwrap();
}

#[tokio::test]
async fn a_client_detaching_does_not_stop_the_server() {
    let server = Running::start().await;
    for _ in 0..3 {
        client::attach(&server.home).await.unwrap();
    }
    assert!(!server.task.is_finished());
    assert!(client::is_running(&server.home).await);
    server.stop().await.unwrap();
}

#[tokio::test]
async fn attach_without_a_server_is_a_transport_error() {
    let root = tempfile::tempdir().unwrap();
    let home = Home::at(root.path());
    assert!(matches!(
        client::attach(&home).await,
        Err(ClientError::Transport(_))
    ));
}

#[tokio::test]
async fn watch_opens_with_hello_and_ends_when_the_server_stops() {
    let server = Running::start().await;
    let watching = client::watch(&server.home).await.unwrap();
    assert_eq!(watching.attached.pid, std::process::id());
    let closed = tokio::spawn(watching.closed());
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        !closed.is_finished(),
        "the stream ended while the server ran"
    );

    server.stop().await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), closed)
        .await
        .expect("the stream outlived the server")
        .unwrap();
}

#[tokio::test]
async fn an_open_watch_does_not_hold_up_a_stop() {
    let server = Running::start().await;
    let _watching = client::watch(&server.home).await.unwrap();
    let started = std::time::Instant::now();
    server.stop().await.unwrap();
    // Well inside the shutdown timeout: the stream ended rather than being
    // waited out.
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn watch_never_starts_a_server() {
    let root = tempfile::tempdir().unwrap();
    let home = Home::at(root.path().join("home"));
    assert!(client::watch(&home).await.is_err());
    assert!(!client::is_running(&home).await);
}
