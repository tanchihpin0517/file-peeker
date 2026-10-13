//! Building the `ssh` commands that reach a machine.
//!
//! Every remote operation is one `ssh <target> <command>`: a probe, an install,
//! the bridge, a `--machine` command. With `[remote] manage_ssh_config` on,
//! they share one `ControlMaster` connection per host, as herdr's do, so only the
//! first of them authenticates.

use std::{
    ffi::OsString, fs, io, os::unix::fs::PermissionsExt as _, path::PathBuf, process::Stdio,
};

use tokio::process::Command;

use crate::{config::Config, home::Home};

/// Replaces the `ssh` program. Tests point it at a stand-in that runs the
/// remote command locally, under another home directory.
pub(crate) const SSH_ENV: &str = "FILE_PEEKER_SSH";

/// How long a shared connection stays up after its last user leaves.
const CONTROL_PERSIST_SECONDS: u32 = 600;
const CONNECT_TIMEOUT_SECONDS: u32 = 10;

/// The host's directory of this build's profile, as a remote shell expands it:
/// a dev client reaches a host's dev install and server, never its release ones.
pub(crate) fn remote_home() -> String {
    format!("$HOME/{}", crate::home::DIRECTORY_NAME)
}

/// The remote binary, quoted for a remote shell. Installed under the host's own
/// directory, never on its `PATH`.
pub(crate) fn remote_fpk() -> String {
    format!("\"{}/bin/fpk\"", remote_home())
}

/// Whether SSH may ask the user anything.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Prompt {
    /// Setup and `machine login`: a password or a keyboard-interactive login
    /// is answered on the terminal.
    Allowed,
    /// Background reconnects: never ask, fail instead, so a reconnect cannot
    /// hang on a prompt nobody sees.
    Never,
}

/// How to reach one machine.
#[derive(Clone, Debug)]
pub(crate) struct Ssh {
    program: OsString,
    target: String,
    options: Vec<String>,
}

impl Ssh {
    /// # Errors
    ///
    /// Returns an error when the control socket directory cannot be made.
    pub(crate) fn new(
        home: &Home,
        config: &Config,
        target: &str,
        prompt: Prompt,
    ) -> io::Result<Self> {
        let mut options = vec![
            "-o".to_owned(),
            format!("ConnectTimeout={CONNECT_TIMEOUT_SECONDS}"),
        ];
        if config.remote.manage_ssh_config {
            let dir = control_dir(home)?;
            options.extend([
                "-o".to_owned(),
                "ControlMaster=auto".to_owned(),
                "-o".to_owned(),
                // `%C` is a hash of the connection, so it fits the short limit
                // on a unix socket's path wherever home is.
                format!("ControlPath={}/%C", dir.display()),
                "-o".to_owned(),
                format!("ControlPersist={CONTROL_PERSIST_SECONDS}"),
            ]);
        }
        if prompt == Prompt::Never {
            options.extend(["-o".to_owned(), "BatchMode=yes".to_owned()]);
        }
        Ok(Self {
            program: std::env::var_os(SSH_ENV).unwrap_or_else(|| "ssh".into()),
            target: target.to_owned(),
            options,
        })
    }

    pub(crate) fn target(&self) -> &str {
        &self.target
    }

    /// `ssh <options> -T <target> <remote>`. `remote` is one shell command line
    /// for the host's shell; quote what goes into it with [`quote`].
    ///
    /// Standard input is closed unless the caller says otherwise; standard
    /// output and error are inherited.
    pub(crate) fn command(&self, remote: &str) -> Command {
        let mut command = Command::new(&self.program);
        command
            .args(&self.options)
            .arg("-T")
            .arg(&self.target)
            .arg(remote)
            .stdin(Stdio::null())
            .kill_on_drop(true);
        command
    }
}

/// What `%C` expands to in a `ControlPath`: a SHA-1 of the connection, in hex.
const CONTROL_NAME_LENGTH: usize = 40;
/// OpenSSH binds a new master at the control path plus `.` and sixteen random
/// characters, then renames it into place, so that suffix has to fit as well.
const CONTROL_TEMPORARY_SUFFIX: usize = 17;

fn control_dir(home: &Home) -> io::Result<PathBuf> {
    let dir = home.ssh_dir();
    // As herdr refuses a control socket path over the limit itself, rather than
    // leaving ssh to fail on it with a message about something else.
    let added = 1 + CONTROL_NAME_LENGTH + CONTROL_TEMPORARY_SUFFIX;
    let longest = dir.as_os_str().len() + added;
    if longest > crate::home::SOCKET_PATH_MAX {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "the shared SSH connection's control socket would be {longest} bytes long — {} \
                 and the {added} characters ssh adds — and a unix socket's path can be at most \
                 {}; set `[remote] manage_ssh_config = false` in config.toml, or {} to a \
                 shorter directory",
                dir.display(),
                crate::home::SOCKET_PATH_MAX,
                crate::home::HOME_ENV
            ),
        ));
    }
    home.create()?;
    let dir = home.ssh_dir();
    fs::create_dir_all(&dir)?;
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
    Ok(dir)
}

/// One argument, quoted for a POSIX shell.
pub(crate) fn quote(argument: &str) -> String {
    if !argument.is_empty()
        && argument
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_./=:@%+,".contains(&byte))
    {
        return argument.to_owned();
    }
    format!("'{}'", argument.replace('\'', r#"'"'"'"#))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(ssh: &Ssh, remote: &str) -> Vec<String> {
        ssh.command(remote)
            .as_std()
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn a_managed_connection_shares_a_master_and_a_background_one_never_prompts() {
        // Under /tmp: the control socket's path must fit a unix socket's limit,
        // and the system's per-user temporary directory is too deep for it.
        let root = tempfile::Builder::new().tempdir_in("/tmp").unwrap();
        let home = Home::at(root.path().join("home"));
        let ssh = Ssh::new(&home, &Config::default(), "me@box", Prompt::Never).unwrap();
        let args = args(&ssh, "true");
        assert!(args.contains(&"ControlMaster=auto".to_owned()), "{args:?}");
        assert!(args.contains(&"BatchMode=yes".to_owned()), "{args:?}");
        assert_eq!(&args[args.len() - 3..], ["-T", "me@box", "true"]);
        let mode = fs::metadata(home.ssh_dir()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
    }

    #[test]
    fn an_unmanaged_connection_leaves_ssh_configuration_alone() {
        let root = tempfile::tempdir().unwrap();
        let home = Home::at(root.path());
        let mut config = Config::default();
        config.remote.manage_ssh_config = false;
        let ssh = Ssh::new(&home, &config, "nano4", Prompt::Allowed).unwrap();
        let args = args(&ssh, "true");
        assert!(
            !args.iter().any(|arg| arg.starts_with("Control")),
            "{args:?}"
        );
        assert!(
            !args.iter().any(|arg| arg.starts_with("BatchMode")),
            "{args:?}"
        );
    }

    #[test]
    fn a_control_socket_that_would_not_fit_is_refused_and_unmanaged_ssh_is_not() {
        let deep = Home::at(format!("/{}", "d".repeat(60)));
        let error = Ssh::new(&deep, &Config::default(), "box", Prompt::Never).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("manage_ssh_config = false"), "{message}");
        assert!(message.contains("bytes long"), "{message}");
        assert!(message.contains("characters ssh adds"), "{message}");
        // Nothing was created on the way to refusing.
        assert!(!deep.dir().exists());

        let mut config = Config::default();
        config.remote.manage_ssh_config = false;
        assert!(Ssh::new(&deep, &config, "box", Prompt::Never).is_ok());
    }

    #[test]
    fn quoting_survives_a_shell() {
        assert_eq!(quote("task"), "task");
        assert_eq!(quote("--seconds=3"), "--seconds=3");
        assert_eq!(quote(""), "''");
        assert_eq!(quote("two words"), "'two words'");
        assert_eq!(quote("it's"), r#"'it'"'"'s'"#);
        for argument in ["plain", "with space", "it's", "$HOME", "a\"b", ""] {
            let output = std::process::Command::new("sh")
                .arg("-c")
                .arg(format!("printf %s {}", quote(argument)))
                .output()
                .unwrap();
            assert_eq!(String::from_utf8(output.stdout).unwrap(), argument);
        }
    }
}
