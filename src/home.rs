//! Where a server's files live: its sockets, lock and logs.
//!
//! One directory per user, resolved from `HOME` alone: `~/.file-peeker/` for a
//! release build, `~/.file-peeker-dev/` for a dev build. Both roles resolve it
//! the same way, which is how a client finds the server it would have started.
//!
//! The two never share anything — binary, server, sockets, logs, saved
//! machines — so a dev build run from a checkout cannot meet the server a
//! release install is using, on this machine or on a host. The profile decides
//! at compile time, as it decides how a build installs itself.
//!
//! herdr splits its files between `~/.config/herdr/` and `~/.local/state/herdr/`.
//! This does not, because the XDG variables that would place them are often set
//! only in an interactive shell's startup files: a server started from a login
//! shell and a bridge started by `ssh host fpk bridge` would then resolve
//! different directories and never meet. `HOME` is the same in both.

use std::{
    env, fs, io,
    os::unix::fs::PermissionsExt as _,
    path::{Path, PathBuf},
};

/// Overrides the directory outright. Tests and development use it to run a
/// server that cannot meet the user's real one.
pub(crate) const HOME_ENV: &str = "FILE_PEEKER_HOME";

/// The directory's name under `HOME` for this build: what a host's `fpk` of
/// the same profile resolves too.
pub(crate) const DIRECTORY_NAME: &str = if crate::build_info::RELEASE {
    ".file-peeker"
} else {
    ".file-peeker-dev"
};
const DIRECTORY_MODE: u32 = 0o700;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Home {
    dir: PathBuf,
}

impl Home {
    /// Resolves the directory from `FILE_PEEKER_HOME`, then `HOME`.
    ///
    /// # Errors
    ///
    /// Returns an error when neither is set to an absolute path.
    pub(crate) fn from_env() -> io::Result<Self> {
        Self::resolve(|name| env::var_os(name).map(PathBuf::from))
    }

    fn resolve(var: impl Fn(&str) -> Option<PathBuf>) -> io::Result<Self> {
        let absolute = |name| var(name).filter(|path| path.is_absolute());
        if let Some(dir) = absolute(HOME_ENV) {
            return Ok(Self::at(dir));
        }
        if let Some(home) = absolute("HOME") {
            return Ok(Self::at(home.join(DIRECTORY_NAME)));
        }
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("cannot find a home directory: set HOME or {HOME_ENV} to an absolute path"),
        ))
    }

    pub(crate) fn at(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub(crate) fn dir(&self) -> &Path {
        &self.dir
    }

    /// The public API: newline-delimited JSON.
    pub(crate) fn api_socket(&self) -> PathBuf {
        self.dir.join("file-peeker.sock")
    }

    /// The private client protocol: gRPC.
    pub(crate) fn client_socket(&self) -> PathBuf {
        self.dir.join("file-peeker-client.sock")
    }

    pub(crate) fn lock(&self) -> PathBuf {
        self.dir.join("server.lock")
    }

    /// Client settings. Nothing on a remote host ever reads the client's.
    pub(crate) fn config_file(&self) -> PathBuf {
        self.dir.join("config.toml")
    }

    /// The SSH machines `fpk machine add` saved.
    pub(crate) fn machines_file(&self) -> PathBuf {
        self.dir.join("machines.toml")
    }

    /// Where the shared SSH connections' control sockets live.
    pub(crate) fn ssh_dir(&self) -> PathBuf {
        self.dir.join("ssh")
    }

    fn logs(&self) -> PathBuf {
        self.dir.join("logs")
    }

    pub(crate) fn server_log(&self) -> PathBuf {
        self.logs().join("server.log")
    }

    /// Fails, saying what to do, when either socket's path is longer than a unix
    /// socket can bind, as herdr refuses one rather than letting `bind` fail
    /// with a bare `EINVAL`.
    ///
    /// # Errors
    ///
    /// Returns a message naming the path, its length and the limit.
    pub(crate) fn check_sockets(&self) -> Result<(), String> {
        for path in [self.api_socket(), self.client_socket()] {
            check_socket_path(&path)
                .map_err(|why| format!("{why}; set {HOME_ENV} to a shorter directory"))?;
        }
        Ok(())
    }

    /// Creates the directory and its `logs/` if they are missing, and makes the
    /// directory private.
    ///
    /// Private is what protects the sockets: they are reachable only through
    /// this directory, so a socket's own mode is a second fence, not the first.
    ///
    /// # Errors
    ///
    /// Returns an error when the directory cannot be created or its mode set.
    pub(crate) fn create(&self) -> io::Result<()> {
        fs::create_dir_all(self.logs())?;
        fs::set_permissions(&self.dir, fs::Permissions::from_mode(DIRECTORY_MODE))
    }
}

/// The longest path a unix socket can bind: the size of `sockaddr_un`'s
/// `sun_path`, less its terminating NUL — 104 bytes on macOS and the BSDs, 108
/// on Linux.
pub(crate) const SOCKET_PATH_MAX: usize = if cfg!(target_os = "linux") { 107 } else { 103 };

/// Fails when `path` is longer than a unix socket can bind.
///
/// # Errors
///
/// Returns a message naming the path, its length and the limit.
pub(crate) fn check_socket_path(path: &Path) -> Result<(), String> {
    let length = path.as_os_str().len();
    if length > SOCKET_PATH_MAX {
        Err(format!(
            "the socket {} is {length} bytes long, and a unix socket's path can be at most \
             {SOCKET_PATH_MAX}",
            path.display()
        ))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolve(vars: &[(&str, &str)]) -> io::Result<Home> {
        Home::resolve(|name| {
            vars.iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| PathBuf::from(value))
        })
    }

    #[test]
    fn override_wins() {
        let home = resolve(&[(HOME_ENV, "/tmp/fpk"), ("HOME", "/home/me")]).unwrap();
        assert_eq!(home.dir(), Path::new("/tmp/fpk"));
    }

    #[test]
    fn defaults_to_a_dot_directory_under_home() {
        let home = resolve(&[("HOME", "/home/me")]).unwrap();
        assert_eq!(home.dir(), Path::new("/home/me").join(DIRECTORY_NAME));
        assert_eq!(
            home.server_log(),
            Path::new("/home/me")
                .join(DIRECTORY_NAME)
                .join("logs/server.log")
        );
    }

    #[test]
    fn xdg_variables_do_not_move_it() {
        let home = resolve(&[("XDG_CONFIG_HOME", "/xdg"), ("HOME", "/home/me")]).unwrap();
        assert_eq!(home.dir(), Path::new("/home/me").join(DIRECTORY_NAME));
    }

    #[test]
    fn a_relative_path_is_ignored() {
        let home = resolve(&[(HOME_ENV, "relative"), ("HOME", "/home/me")]).unwrap();
        assert_eq!(home.dir(), Path::new("/home/me").join(DIRECTORY_NAME));
        assert!(resolve(&[("HOME", "relative")]).is_err());
    }

    #[test]
    fn a_socket_path_over_the_limit_is_refused_with_what_to_do() {
        let fits = Home::at("/home/me");
        assert_eq!(fits.check_sockets(), Ok(()));

        let deep = "d".repeat(SOCKET_PATH_MAX);
        let error = Home::at(format!("/{deep}")).check_sockets().unwrap_err();
        assert!(error.contains("bytes long"), "{error}");
        assert!(error.contains(&SOCKET_PATH_MAX.to_string()), "{error}");
        assert!(error.contains(HOME_ENV), "{error}");

        // The limit itself binds; one more byte does not.
        let at = |length: usize| PathBuf::from(format!("/{}", "s".repeat(length - 1)));
        assert_eq!(check_socket_path(&at(SOCKET_PATH_MAX)), Ok(()));
        assert!(check_socket_path(&at(SOCKET_PATH_MAX + 1)).is_err());
    }

    #[test]
    fn create_makes_the_directory_private() {
        let root = tempfile::tempdir().unwrap();
        let home = Home::at(root.path().join("nested").join("home"));
        home.create().unwrap();
        assert!(home.server_log().parent().unwrap().is_dir());
        let mode = fs::metadata(home.dir()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, DIRECTORY_MODE);
    }
}
