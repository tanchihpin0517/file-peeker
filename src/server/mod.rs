//! The persistent per-user server, `fpk server`.
//!
//! Step 1 of the build order: the process model with nothing inside it. The
//! server takes the single-instance lock, binds both sockets, answers the
//! client-protocol handshake and the API's `server.status` and `server.stop`, and
//! runs until it is told to stop. A client detaching does not stop it.

mod api;
mod attach;

use std::{
    fs::{self, File, TryLockError},
    io,
    os::unix::fs::PermissionsExt as _,
    path::{Path, PathBuf},
    process::ExitCode,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use thiserror::Error;
use tokio::{
    net::UnixListener,
    signal::unix::{SignalKind, signal},
};
use tokio_stream::wrappers::UnixListenerStream;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

use crate::{
    home::Home,
    protocol::{
        CLIENT_PROTOCOL, VERSION,
        api::{API_GENERATION, ServerStatus},
        client_v1::client_protocol_server::ClientProtocolServer,
    },
};

const SOCKET_MODE: u32 = 0o600;
/// How long a stop waits for attached clients to let go before closing anyway.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
/// How long a starting server waits on a lock held by a server with no socket.
const LOCK_GRACE: Duration = Duration::from_secs(1);
const LOCK_POLL: Duration = Duration::from_millis(10);
/// Read once, like `RUST_LOG`, to set the server log's filter.
const LOG_ENV: &str = "FILE_PEEKER_LOG";

#[derive(Debug, Error)]
pub(crate) enum ServerError {
    #[error("a server is already running for {}", .0.display())]
    AlreadyRunning(PathBuf),
    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: io::Error,
    },
    #[error("client protocol server failed: {0}")]
    Grpc(#[from] tonic::transport::Error),
}

fn io_error(context: impl Into<String>) -> impl FnOnce(io::Error) -> ServerError {
    let context = context.into();
    move |source| ServerError::Io { context, source }
}

/// What a running server says about itself.
#[derive(Clone, Debug)]
pub(crate) struct ServerInfo {
    pid: u32,
    started: SystemTime,
    home: PathBuf,
}

impl ServerInfo {
    fn new(home: &Home) -> Self {
        Self {
            pid: std::process::id(),
            started: SystemTime::now(),
            home: home.dir().to_owned(),
        }
    }

    fn status(&self) -> ServerStatus {
        ServerStatus {
            version: VERSION.to_owned(),
            pid: self.pid,
            client_protocol: CLIENT_PROTOCOL,
            api_generation: API_GENERATION,
            started_unix_seconds: self
                .started
                .duration_since(UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_secs()),
            home: self.home.clone(),
        }
    }
}

/// Runs the server in the foreground until a stop request or a signal.
///
/// This is the process entry point: it owns the runtime, the log and the signal
/// handlers. [`serve`] is the part tests run in-process.
pub(crate) fn run(home: &Home) -> ExitCode {
    if let Err(error) = home.create().and_then(|()| init_logging(home)) {
        eprintln!(
            "fpk server: cannot prepare {}: {error}",
            home.dir().display()
        );
        return ExitCode::FAILURE;
    }
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            error!(%error, "cannot start the runtime");
            return ExitCode::FAILURE;
        }
    };
    runtime.block_on(async {
        let shutdown = CancellationToken::new();
        if let Err(error) = watch_signals(shutdown.clone()) {
            error!(%error, "cannot install signal handlers");
            return ExitCode::FAILURE;
        }
        match serve(home, shutdown).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                error!(%error, "server stopped");
                eprintln!("fpk server: {error}");
                ExitCode::FAILURE
            }
        }
    })
}

fn init_logging(home: &Home) -> io::Result<()> {
    let log = File::options()
        .create(true)
        .append(true)
        .open(home.server_log())?;
    let filter = EnvFilter::try_from_env(LOG_ENV).unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(false)
        .with_writer(Mutex::new(log))
        .init();
    Ok(())
}

/// `SIGTERM` and `SIGINT` stop the server. `SIGHUP` is caught and ignored: a
/// server outlives the terminal that happened to start it.
fn watch_signals(shutdown: CancellationToken) -> io::Result<()> {
    let mut terminate = signal(SignalKind::terminate())?;
    let mut interrupt = signal(SignalKind::interrupt())?;
    let mut hangup = signal(SignalKind::hangup())?;
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = terminate.recv() => { info!("SIGTERM received"); break; }
                _ = interrupt.recv() => { info!("SIGINT received"); break; }
                _ = hangup.recv() => info!("SIGHUP ignored"),
            }
        }
        shutdown.cancel();
    });
    Ok(())
}

/// Serves `home` until `shutdown` is cancelled, then removes its sockets and
/// releases the lock.
///
/// # Errors
///
/// Returns [`ServerError::AlreadyRunning`] when another server holds the lock,
/// and an I/O or transport error when the sockets cannot be served.
pub(crate) async fn serve(home: &Home, shutdown: CancellationToken) -> Result<(), ServerError> {
    home.create()
        .map_err(io_error(format!("cannot create {}", home.dir().display())))?;
    // Declared first so it is dropped last: the sockets are gone before another
    // server can take the lock.
    let _lock = acquire_lock(home).await?;
    let sockets = Sockets::claim(home)?;

    let info = Arc::new(ServerInfo::new(home));
    info!(
        pid = info.pid,
        version = VERSION,
        home = %home.dir().display(),
        "server started"
    );

    let api_listener = Sockets::bind(&sockets.api)?;
    let client_listener = Sockets::bind(&sockets.client)?;

    let api = tokio::spawn(api::serve(
        api_listener,
        Arc::clone(&info),
        shutdown.clone(),
    ));
    let client = tonic::transport::Server::builder()
        .add_service(ClientProtocolServer::new(attach::Attach::new(
            Arc::clone(&info),
            shutdown.clone(),
        )))
        .serve_with_incoming_shutdown(
            UnixListenerStream::new(client_listener),
            shutdown.clone().cancelled_owned(),
        );
    let client = tokio::spawn(client);

    shutdown.cancelled().await;
    info!("server stopping");
    let finished = tokio::time::timeout(SHUTDOWN_TIMEOUT, async {
        let api = api.await;
        let client = client.await;
        (api, client)
    })
    .await;
    match finished {
        Ok((_, Ok(Err(error)))) => return Err(error.into()),
        Ok(_) => {}
        Err(_) => warn!("clients did not detach in time; closing anyway"),
    }
    drop(sockets);
    info!("server stopped");
    Ok(())
}

/// Takes `server.lock` for the life of the returned file.
///
/// The lock, not the sockets, is what says a server is running: a socket file
/// can outlive a crashed server, a lock cannot outlive its process.
///
/// A held lock with no client socket is a server between removing its sockets
/// and exiting, or one that has not bound them yet. Either resolves within
/// moments, so this retries for [`LOCK_GRACE`] before giving up — which is what
/// lets `fpk server stop && fpk` start a fresh server rather than lose to the
/// old one's last instant.
async fn acquire_lock(home: &Home) -> Result<File, ServerError> {
    let path = home.lock();
    let file = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(io_error(format!("cannot open {}", path.display())))?;
    let deadline = tokio::time::Instant::now() + LOCK_GRACE;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(TryLockError::WouldBlock)
                if !home.client_socket().exists() && tokio::time::Instant::now() < deadline =>
            {
                tokio::time::sleep(LOCK_POLL).await;
            }
            Err(TryLockError::WouldBlock) => {
                return Err(ServerError::AlreadyRunning(home.dir().to_owned()));
            }
            Err(TryLockError::Error(source)) => {
                return Err(ServerError::Io {
                    context: format!("cannot lock {}", path.display()),
                    source,
                });
            }
        }
    }
}

/// The two socket paths, removed again when this is dropped.
///
/// Only constructed while the lock is held, so any socket file already there is
/// a crashed server's and safe to replace.
struct Sockets {
    api: PathBuf,
    client: PathBuf,
}

impl Sockets {
    fn claim(home: &Home) -> Result<Self, ServerError> {
        let sockets = Self {
            api: home.api_socket(),
            client: home.client_socket(),
        };
        for path in [&sockets.api, &sockets.client] {
            match fs::remove_file(path) {
                Ok(()) => warn!(path = %path.display(), "replaced a stale socket"),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(source) => {
                    return Err(ServerError::Io {
                        context: format!("cannot remove stale {}", path.display()),
                        source,
                    });
                }
            }
        }
        Ok(sockets)
    }

    fn bind(path: &Path) -> Result<UnixListener, ServerError> {
        let listener = UnixListener::bind(path)
            .map_err(io_error(format!("cannot bind {}", path.display())))?;
        fs::set_permissions(path, fs::Permissions::from_mode(SOCKET_MODE))
            .map_err(io_error(format!("cannot restrict {}", path.display())))?;
        Ok(listener)
    }
}

impl Drop for Sockets {
    fn drop(&mut self) {
        for path in [&self.api, &self.client] {
            if let Err(error) = fs::remove_file(path)
                && error.kind() != io::ErrorKind::NotFound
            {
                warn!(path = %path.display(), %error, "cannot remove socket");
            }
        }
    }
}

#[cfg(test)]
mod tests;
