//! One machine's connection: attach, hold the `Watch` stream, and reconnect
//! with backoff when it ends.

use std::time::{Duration, Instant};

use tokio::sync::mpsc;

use super::app::{Command, ConnectionEvent};
use crate::{client, home::Home};

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

/// Runs until the UI goes away: `events` closing, or `commands` ending.
///
/// Reconnecting only ever attaches. A server is started only on
/// [`Command::StartServer`], so a server the user stopped on purpose stays
/// stopped until they say otherwise.
pub(super) async fn run(
    home: Home,
    backoff: Backoff,
    events: mpsc::Sender<ConnectionEvent>,
    mut commands: mpsc::Receiver<Command>,
) {
    let mut attempt = 0;
    loop {
        if events.send(ConnectionEvent::Attaching).await.is_err() {
            return;
        }
        if let Ok(watching) = client::watch(&home).await {
            attempt = 0;
            let attached = ConnectionEvent::Attached {
                server_version: watching.attached.server_version.clone(),
                pid: watching.attached.pid,
            };
            if events.send(attached).await.is_err() {
                return;
            }
            tokio::select! {
                () = watching.closed() => {}
                () = events.closed() => return,
            }
        }

        let delay = backoff.delay(attempt);
        attempt = attempt.saturating_add(1);
        let retry_at = Instant::now() + delay;
        if events
            .send(ConnectionEvent::Disconnected { retry_at })
            .await
            .is_err()
        {
            return;
        }
        tokio::select! {
            () = tokio::time::sleep(delay) => {}
            command = commands.recv() => match command {
                Some(Command::StartServer) => {
                    if events.send(ConnectionEvent::Starting).await.is_err() {
                        return;
                    }
                    // A failure here is the next attach failing, which backs
                    // off and is shown like any other.
                    let _ = client::start_server(&home).await;
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
    use crate::server;

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
        let connection = tokio::spawn(run(home.clone(), backoff, events_tx, commands_rx));
        assert_eq!(next(&mut events).await, ConnectionEvent::Attaching);
        until_disconnected(&mut events).await;
        assert!(!client::is_running(&home).await);

        let (shutdown, task) = serve(home.clone());
        assert_eq!(until_attached(&mut events).await, std::process::id());

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
