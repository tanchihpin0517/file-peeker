//! Where a server's files live: its sockets, lock and logs.
//!
//! One directory per user, `~/.file-peeker/`, resolved from `HOME` alone. Both
//! roles resolve it the same way, which is how a client finds the server it would
//! have started.
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

const DIRECTORY_NAME: &str = ".file-peeker";
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

    fn logs(&self) -> PathBuf {
        self.dir.join("logs")
    }

    pub(crate) fn server_log(&self) -> PathBuf {
        self.logs().join("server.log")
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
        assert_eq!(home.dir(), Path::new("/home/me/.file-peeker"));
        assert_eq!(
            home.server_log(),
            Path::new("/home/me/.file-peeker/logs/server.log")
        );
    }

    #[test]
    fn xdg_variables_do_not_move_it() {
        let home = resolve(&[("XDG_CONFIG_HOME", "/xdg"), ("HOME", "/home/me")]).unwrap();
        assert_eq!(home.dir(), Path::new("/home/me/.file-peeker"));
    }

    #[test]
    fn a_relative_path_is_ignored() {
        let home = resolve(&[(HOME_ENV, "relative"), ("HOME", "/home/me")]).unwrap();
        assert_eq!(home.dir(), Path::new("/home/me/.file-peeker"));
        assert!(resolve(&[("HOME", "relative")]).is_err());
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
