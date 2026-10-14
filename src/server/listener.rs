//! A socket that binds itself again when its file goes missing.
//!
//! The sockets live in `/tmp`, which systemd-tmpfiles and its like clean of
//! what has not been touched. A server whose socket file is gone still runs and
//! still holds the lock, but no new client can reach it, so it checks now and
//! then and binds a fresh socket at the same path, as tmux recreates its own.

use std::{
    fs, io,
    os::unix::fs::{MetadataExt as _, PermissionsExt as _},
    path::{Path, PathBuf},
    time::Duration,
};

use tokio::{
    net::{UnixListener, UnixStream},
    time::{Interval, MissedTickBehavior},
};
use tracing::warn;

use super::{ServerError, io_error};
use crate::home;

const SOCKET_MODE: u32 = 0o600;
/// How often a server looks for its socket files.
const CHECK_EVERY: Duration = if cfg!(test) {
    Duration::from_millis(50)
} else {
    Duration::from_secs(60)
};

pub(super) struct Listener {
    path: PathBuf,
    socket: UnixListener,
    /// The inode bound, to tell this server's socket from a file put in its place.
    inode: u64,
    check: Interval,
}

impl Listener {
    pub(super) fn bind(path: &Path) -> Result<Self, ServerError> {
        let (socket, inode) =
            bind(path).map_err(io_error(format!("cannot bind {}", path.display())))?;
        let mut check =
            tokio::time::interval_at(tokio::time::Instant::now() + CHECK_EVERY, CHECK_EVERY);
        check.set_missed_tick_behavior(MissedTickBehavior::Delay);
        Ok(Self {
            path: path.to_owned(),
            socket,
            inode,
            check,
        })
    }

    /// The next connection. Between connections, binds again if the socket file
    /// has gone.
    pub(super) async fn accept(&mut self) -> io::Result<UnixStream> {
        loop {
            tokio::select! {
                accepted = self.socket.accept() => return accepted.map(|(stream, _)| stream),
                _ = self.check.tick() => self.bind_again_if_gone(),
            }
        }
    }

    fn bind_again_if_gone(&mut self) {
        match fs::symlink_metadata(&self.path) {
            Ok(metadata) if metadata.ino() == self.inode => return,
            Ok(_) => warn!(path = %self.path.display(), "another file took the socket's place"),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                warn!(path = %self.path.display(), "the socket was removed");
            }
            Err(error) => {
                warn!(path = %self.path.display(), %error, "cannot check the socket");
                return;
            }
        }
        // A cleaner may have taken the directory as well.
        let rebound = self
            .path
            .parent()
            .map_or(Ok(()), home::private_dir)
            .and_then(|()| match fs::remove_file(&self.path) {
                Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
                _ => Ok(()),
            })
            .and_then(|()| bind(&self.path));
        match rebound {
            Ok((socket, inode)) => {
                self.socket = socket;
                self.inode = inode;
                warn!(path = %self.path.display(), "bound the socket again");
            }
            // Tried again at the next check.
            Err(error) => {
                warn!(path = %self.path.display(), %error, "cannot bind the socket again");
            }
        }
    }
}

fn bind(path: &Path) -> io::Result<(UnixListener, u64)> {
    let listener = UnixListener::bind(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(SOCKET_MODE))?;
    Ok((listener, fs::symlink_metadata(path)?.ino()))
}
