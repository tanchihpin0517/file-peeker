//! Reaching the server on this machine: finding it, starting it, attaching.
//!
//! This module never names `server`. It finds a server the way any other
//! process would — through the sockets under [`Home`] — and starts one by running
//! this same binary as `fpk server`.

pub(crate) mod api;
pub(crate) mod machines;
pub(crate) mod remote;
pub(crate) mod ssh;
pub(crate) mod tasks;

use std::{
    fs::File,
    io,
    process::{Command, Stdio},
    time::Duration,
};

use hyper_util::rt::TokioIo;
use thiserror::Error;
use tokio::net::UnixStream;
use tonic::{
    Code,
    transport::{Channel, Endpoint, Uri},
};
use tower::service_fn;

use self::tasks::{TaskUpdate, TaskView};
use crate::{
    home::{HOME_ENV, Home},
    protocol::{
        CLIENT_PROTOCOL, VERSION,
        client_v1::{
            AttachRequest, ServerEvent, WatchRequest, client_protocol_client::ClientProtocolClient,
            server_event,
        },
    },
};

/// How long a client waits for a server it started to open its socket.
const START_TIMEOUT: Duration = Duration::from_secs(5);
const START_POLL: Duration = Duration::from_millis(25);

#[derive(Debug, Error)]
pub(crate) enum ClientError {
    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: io::Error,
    },
    #[error("cannot start a server here: {0}")]
    Unstartable(String),
    #[error("the server did not start within {}s; see {}", START_TIMEOUT.as_secs(), .log)]
    StartTimeout { log: String },
    /// The running server is a different build. Never replaced behind the
    /// user's back, as herdr never replaces a running server: they stop it.
    #[error("the server refused to attach: {0}; stop it with `fpk server stop` and run this again")]
    Incompatible(String),
    #[error("the server failed the handshake: {0}")]
    Handshake(#[from] tonic::Status),
    #[error("cannot open the client protocol channel: {0}")]
    Transport(#[from] tonic::transport::Error),
}

/// A completed handshake.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Attached {
    pub server_version: String,
    pub pid: u32,
    /// Whether this client started the server it attached to.
    pub started: bool,
}

/// Attaches to the server for `home`, starting one first if none is running.
///
/// # Errors
///
/// Returns an error when a server cannot be reached or started, or refuses the
/// handshake.
pub(crate) async fn attach_or_start(home: &Home) -> Result<Attached, ClientError> {
    let started = if is_running(home).await {
        false
    } else {
        start_server(home).await?;
        true
    };
    let mut attached = attach(home).await?;
    attached.started = started;
    Ok(attached)
}

/// Whether a server answers on `home`'s client socket.
pub(crate) async fn is_running(home: &Home) -> bool {
    UnixStream::connect(home.client_socket()).await.is_ok()
}

/// Starts a server for `home` and waits until it answers.
///
/// # Errors
///
/// Returns an error when the server cannot be spawned or does not open its
/// socket in time.
pub(crate) async fn start_server(home: &Home) -> Result<(), ClientError> {
    start(home)?;
    wait_until_running(home).await
}

/// Runs this binary as `fpk server`, detached.
///
/// The server gets its own process group, so neither the terminal's `SIGINT`
/// nor a hangup aimed at this client's job reaches it. Its standard error goes
/// to the server log, which is where anything it says before its own logging is
/// up has to land.
fn start(home: &Home) -> Result<(), ClientError> {
    // Checked here as well as in the server, so the reason reaches the user
    // rather than a server log and a start that only times out.
    home.check_sockets().map_err(ClientError::Unstartable)?;
    home.create().map_err(|source| ClientError::Io {
        context: format!("cannot create {}", home.dir().display()),
        source,
    })?;
    let log = File::options()
        .create(true)
        .append(true)
        .open(home.server_log())
        .map_err(|source| ClientError::Io {
            context: format!("cannot open {}", home.server_log().display()),
            source,
        })?;
    let executable = std::env::current_exe().map_err(|source| ClientError::Io {
        context: "cannot find this executable".to_owned(),
        source,
    })?;
    let mut server = Command::new(executable);
    server
        .arg("server")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log);
    // A directory resolved from `HOME` resolves the same way in the server; one
    // an override placed has to be passed on.
    if home.is_overridden() {
        server.env(HOME_ENV, home.dir());
    }
    std::os::unix::process::CommandExt::process_group(&mut server, 0)
        .spawn()
        .map_err(|source| ClientError::Io {
            context: "cannot start the server".to_owned(),
            source,
        })?;
    Ok(())
}

/// Polls until a server answers. It need not be the one this client started:
/// when two clients start at once, one server loses the lock and exits, and both
/// clients attach to the winner.
async fn wait_until_running(home: &Home) -> Result<(), ClientError> {
    let deadline = tokio::time::Instant::now() + START_TIMEOUT;
    while tokio::time::Instant::now() < deadline {
        if is_running(home).await {
            return Ok(());
        }
        tokio::time::sleep(START_POLL).await;
    }
    Err(ClientError::StartTimeout {
        log: home.server_log().display().to_string(),
    })
}

/// Performs the client-protocol handshake with a running server.
///
/// # Errors
///
/// Returns [`ClientError::Incompatible`] when the server's protocol differs.
pub(crate) async fn attach(home: &Home) -> Result<Attached, ClientError> {
    let mut client = ClientProtocolClient::new(channel(home).await?);
    handshake(&mut client).await
}

async fn handshake(client: &mut ClientProtocolClient<Channel>) -> Result<Attached, ClientError> {
    let response = client
        .attach(AttachRequest {
            client_version: VERSION.to_owned(),
            client_protocol: CLIENT_PROTOCOL,
        })
        .await
        .map_err(|status| match status.code() {
            Code::FailedPrecondition => ClientError::Incompatible(status.message().to_owned()),
            _ => ClientError::Handshake(status),
        })?
        .into_inner();
    Ok(Attached {
        server_version: response.server_version,
        pid: response.pid,
        started: false,
    })
}

/// A gRPC channel over `home`'s client socket. The URI is a placeholder: the
/// connector ignores it and dials the socket.
pub(crate) async fn channel(home: &Home) -> Result<Channel, ClientError> {
    let socket = home.client_socket();
    let channel = Endpoint::from_static("http://file-peeker")
        .connect_with_connector(service_fn(move |_: Uri| {
            let socket = socket.clone();
            async move { UnixStream::connect(socket).await.map(TokioIo::new) }
        }))
        .await?;
    Ok(channel)
}

/// An attachment held open: the handshake's answer, and the `Watch` stream whose
/// end means the server is gone.
pub(crate) struct Watching {
    pub attached: Attached,
    events: tonic::Streaming<ServerEvent>,
}

impl Watching {
    /// The next task update, or `None` once the server ends the stream or the
    /// connection drops. Either way the attachment is over.
    pub(crate) async fn next(&mut self) -> Option<TaskUpdate> {
        loop {
            let event = self.events.message().await.ok()??;
            match event.event {
                Some(server_event::Event::Snapshot(snapshot)) => {
                    return Some(TaskUpdate::Snapshot(
                        snapshot.tasks.into_iter().map(TaskView::from).collect(),
                    ));
                }
                Some(server_event::Event::Changed(changed)) => {
                    if let Some(task) = changed.task {
                        return Some(TaskUpdate::Changed(task.into()));
                    }
                }
                // `Hello`, or an event a newer server sends: nothing to apply.
                Some(server_event::Event::Hello(_)) | None => {}
            }
        }
    }

    /// Resolves when the attachment is over, discarding what arrives first.
    #[cfg(test)]
    pub(crate) async fn closed(mut self) {
        while self.next().await.is_some() {}
    }
}

/// Attaches to a running server and opens its `Watch` stream on the same
/// channel. Never starts a server: a client that lost one reconnects through
/// this, and reconnecting must not resurrect a server the user stopped.
///
/// # Errors
///
/// Returns an error when no server answers, the handshake fails, or the stream
/// does not open.
pub(crate) async fn watch(home: &Home) -> Result<Watching, ClientError> {
    watch_on(channel(home).await?).await
}

/// [`watch`] over any channel to a server: the local socket, or a remote
/// machine's bridge.
///
/// # Errors
///
/// Returns an error when the handshake fails or the stream does not open.
pub(crate) async fn watch_on(channel: Channel) -> Result<Watching, ClientError> {
    let mut client = ClientProtocolClient::new(channel);
    let attached = handshake(&mut client).await?;
    let events = client.watch(WatchRequest {}).await?.into_inner();
    Ok(Watching { attached, events })
}
