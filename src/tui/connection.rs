//! One machine's connection: attach, hold the `Watch` stream, and reconnect
//! with backoff when it ends.

use std::time::{Duration, Instant};

use tokio::sync::mpsc;

use super::app::{Attention, Command, ConnectionEvent};
use crate::{
    client::{
        self, Watching,
        remote::{self, Reach},
        ssh::Ssh,
    },
    home::Home,
    protocol::CLIENT_PROTOCOL,
};

/// Reconnect delays: doubling from `base`, capped at `max`.
#[derive(Clone, Copy, Debug)]
pub(super) struct Backoff {
    pub base: Duration,
    pub max: Duration,
}

impl Backoff {
    /// As herdr backs off a lost machine: from one second up to two minutes.
    pub(super) const DEFAULT: Self = Self {
        base: Duration::from_secs(1),
        max: Duration::from_secs(120),
    };

    /// The delay before attempt `attempt`, counting from zero.
    pub(super) fn delay(self, attempt: u32) -> Duration {
        let factor = 1u32.checked_shl(attempt).unwrap_or(u32::MAX);
        self.base.saturating_mul(factor).min(self.max)
    }
}

/// What a connection reaches.
#[derive(Clone, Debug)]
pub(super) enum Target {
    /// This machine's server, on its socket.
    Local(Home),
    /// A saved machine's server, through its bridge over SSH that never
    /// prompts.
    Remote(Ssh),
}

/// Why an attempt to attach did not end attached.
enum Lost {
    /// Try again after the backoff; `reason` says why, when there is more to
    /// say than that the server is gone.
    Retry(Option<String>),
    /// Try again after the backoff, but someone has to act first.
    Attention(Attention),
}

/// Attaches to `target` and opens its `Watch` stream. A remote machine is
/// probed first, so a missing or incompatible install, a host that wants a
/// login, and a server that is simply not running each say so rather than all
/// looking like a dropped connection.
async fn attach(target: &Target) -> Result<Watching, Lost> {
    let channel = match target {
        Target::Local(home) => {
            return client::watch(home).await.map_err(|_| Lost::Retry(None));
        }
        Target::Remote(ssh) => match remote::probe(ssh).await {
            Reach::NeedsLogin(_) => return Err(Lost::Attention(Attention::NeedsLogin)),
            Reach::Unreachable(why) => return Err(Lost::Retry(Some(why))),
            Reach::Probed(probe) if !probe.installed => {
                return Err(Lost::Attention(Attention::NotInstalled));
            }
            Reach::Probed(probe) if probe.client_protocol != CLIENT_PROTOCOL => {
                return Err(Lost::Attention(Attention::Incompatible {
                    found: probe.describe(),
                }));
            }
            Reach::Probed(probe) if !probe.server_running => {
                return Err(Lost::Retry(Some("its server is not running".to_owned())));
            }
            Reach::Probed(_) => remote::channel(ssh)
                .await
                .map_err(|error| Lost::Retry(Some(error.to_string())))?,
        },
    };
    client::watch_on(channel)
        .await
        .map_err(|error| Lost::Retry(Some(error.to_string())))
}

async fn start_server(target: &Target) {
    // A failure here is the next attach failing, which backs off and is shown
    // like any other.
    let _ = match target {
        Target::Local(home) => client::start_server(home)
            .await
            .map_err(|error| error.to_string()),
        Target::Remote(ssh) => remote::start_server(ssh).await,
    };
}

/// Runs until the UI goes away: `events` closing, or `commands` ending.
///
/// Reconnecting only ever attaches. A server is started only on
/// [`Command::StartServer`], so a server the user stopped on purpose stays
/// stopped until they say otherwise, and nothing is ever installed from here.
pub(super) async fn run(
    target: Target,
    backoff: Backoff,
    events: mpsc::Sender<ConnectionEvent>,
    mut commands: mpsc::Receiver<Command>,
) {
    let mut attempt = 0;
    loop {
        if events.send(ConnectionEvent::Attaching).await.is_err() {
            return;
        }
        let lost = match attach(&target).await {
            Ok(mut watching) => {
                attempt = 0;
                let attached = ConnectionEvent::Attached {
                    server_version: watching.attached.server_version.clone(),
                    pid: watching.attached.pid,
                };
                if events.send(attached).await.is_err() {
                    return;
                }
                loop {
                    tokio::select! {
                        update = watching.next() => match update {
                            Some(update) => {
                                if events.send(ConnectionEvent::Tasks(update)).await.is_err() {
                                    return;
                                }
                            }
                            None => break,
                        },
                        () = events.closed() => return,
                    }
                }
                Lost::Retry(None)
            }
            Err(lost) => lost,
        };

        let delay = backoff.delay(attempt);
        attempt = attempt.saturating_add(1);
        let retry_at = Instant::now() + delay;
        let event = match lost {
            Lost::Retry(reason) => ConnectionEvent::Disconnected { retry_at, reason },
            Lost::Attention(attention) => ConnectionEvent::NeedsAttention {
                attention,
                retry_at,
            },
        };
        if events.send(event).await.is_err() {
            return;
        }
        tokio::select! {
            () = tokio::time::sleep(delay) => {}
            command = commands.recv() => match command {
                Some(Command::StartServer) => {
                    if events.send(ConnectionEvent::Starting).await.is_err() {
                        return;
                    }
                    start_server(&target).await;
                    attempt = 0;
                }
                None => return,
            },
            () = events.closed() => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::{
        client::tasks::{TaskUpdate, TaskView},
        server,
    };

    #[test]
    fn backoff_doubles_up_to_its_cap() {
        let delays: Vec<u64> = (0..10)
            .map(|attempt| Backoff::DEFAULT.delay(attempt).as_secs())
            .collect();
        assert_eq!(delays, [1, 2, 4, 8, 16, 32, 64, 120, 120, 120]);
        assert_eq!(Backoff::DEFAULT.delay(u32::MAX), Duration::from_secs(120));
    }

    async fn next(events: &mut mpsc::Receiver<ConnectionEvent>) -> ConnectionEvent {
        tokio::time::timeout(Duration::from_secs(5), events.recv())
            .await
            .expect("no connection event in time")
            .expect("the connection task ended")
    }

    async fn until_snapshot(events: &mut mpsc::Receiver<ConnectionEvent>) -> Vec<TaskView> {
        loop {
            if let ConnectionEvent::Tasks(TaskUpdate::Snapshot(tasks)) = next(events).await {
                return tasks;
            }
        }
    }

    async fn until_attached(events: &mut mpsc::Receiver<ConnectionEvent>) -> u32 {
        loop {
            if let ConnectionEvent::Attached { pid, .. } = next(events).await {
                return pid;
            }
        }
    }

    async fn until_disconnected(events: &mut mpsc::Receiver<ConnectionEvent>) {
        loop {
            if let ConnectionEvent::Disconnected { .. } = next(events).await {
                return;
            }
        }
    }

    /// The server half runs in-process through `server::serve`, which only a
    /// test may name from here: in the binary the two meet through a socket.
    #[tokio::test]
    async fn follows_a_server_that_stops_and_comes_back() {
        let root = tempfile::tempdir().unwrap();
        let home = Home::at(root.path().join("home"));
        let serve = |home: Home| {
            let shutdown = CancellationToken::new();
            let task = tokio::spawn({
                let shutdown = shutdown.clone();
                async move { server::serve(&home, shutdown).await }
            });
            (shutdown, task)
        };

        let (events_tx, mut events) = mpsc::channel(16);
        let (_commands, commands_rx) = mpsc::channel(1);
        let backoff = Backoff {
            base: Duration::from_millis(20),
            max: Duration::from_millis(80),
        };

        // No server yet: the connection backs off rather than starting one.
        let connection = tokio::spawn(run(
            Target::Local(home.clone()),
            backoff,
            events_tx,
            commands_rx,
        ));
        assert_eq!(next(&mut events).await, ConnectionEvent::Attaching);
        until_disconnected(&mut events).await;
        assert!(!client::is_running(&home).await);

        let (shutdown, task) = serve(home.clone());
        assert_eq!(until_attached(&mut events).await, std::process::id());
        // A fresh server has no tasks, and says so rather than saying nothing.
        assert_eq!(until_snapshot(&mut events).await, []);

        shutdown.cancel();
        task.await.unwrap().unwrap();
        until_disconnected(&mut events).await;

        let (shutdown, task) = serve(home.clone());
        until_attached(&mut events).await;

        drop(events);
        tokio::time::timeout(Duration::from_secs(5), connection)
            .await
            .expect("the connection task outlived its UI")
            .unwrap();
        shutdown.cancel();
        task.await.unwrap().unwrap();
    }
}
