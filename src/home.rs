//! Where a machine's files live: its home directory, its connections, its ID.
//!
//! Per user and per machine. Nothing is shared between two machines, even when
//! they share one home directory, as a cluster's nodes do over NFS: a unix
//! socket connects only on the machine that bound it, `flock` on NFS may or may
//! not reach across machines, and one machine's saved SSH machines, binary and
//! build are not another's. Two places, both private:
//!
//! - **The host directory**, `~/.file-peeker/hosts/<machine-id>/`: settings,
//!   saved machines, the lock, the binary and the logs — what lasts.
//! - **The connections directory**, `/tmp/fpk-<uid>/`: the two sockets and the
//!   shared SSH connections' control sockets, whose paths must stay short, as
//!   tmux keeps its own in `/tmp/tmux-<uid>/`.
//!
//! The lock stays in the host directory because it is what makes one server per
//! machine, and `/tmp` may be cleaned or seen differently by another session.
//!
//! A release build uses `~/.file-peeker/` and `/tmp/fpk-<uid>/`, a dev build
//! `~/.file-peeker-dev/` and `/tmp/fpk-dev-<uid>/`, so a dev build run from a
//! checkout cannot meet the server a release install is using, on this machine
//! or on a host. The profile decides at compile time.
//!
//! herdr splits its files between `~/.config/herdr/` and `~/.local/state/herdr/`.
//! This does not, because the XDG variables that would place them are often set
//! only in an interactive shell's startup files: a server started from a login
//! shell and a bridge started by `ssh host fpk bridge` would then resolve
//! different directories and never meet. `HOME`, the machine ID and the uid are
//! the same in both.

use std::{
    env,
    ffi::OsStr,
    fs, io,
    os::unix::{
        ffi::OsStrExt as _,
        fs::{DirBuilderExt as _, MetadataExt as _, PermissionsExt as _},
    },
    path::{Path, PathBuf},
};

/// Overrides both places outright: the host directory is this one, and the
/// connections are in its `run/`. Tests and development use it to run a server
/// that cannot meet the user's real one.
pub(crate) const HOME_ENV: &str = "FILE_PEEKER_HOME";

/// The directory's name under `HOME` for this build: what a host's `fpk` of
/// the same profile resolves too.
pub(crate) const DIRECTORY_NAME: &str = if crate::build_info::RELEASE {
    ".file-peeker"
} else {
    ".file-peeker-dev"
};
/// The connections directory is `/tmp/<this>-<uid>`.
const RUN_PREFIX: &str = if crate::build_info::RELEASE {
    "fpk"
} else {
    "fpk-dev"
};
const HOSTS: &str = "hosts";
const BY_NAME: &str = "by-name";
/// The connections directory under an overriding [`HOME_ENV`].
const RUN: &str = "run";
const DIRECTORY_MODE: u32 = 0o700;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Home {
    dir: PathBuf,
    run: PathBuf,
    /// Where this machine's `by-name/` link goes; `None` when [`HOME_ENV`] or a
    /// test placed the directory, which no machine ID names.
    name: Option<Name>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Name {
    by_name: PathBuf,
    id: String,
}

impl Home {
    /// Resolves both places from `FILE_PEEKER_HOME`, or else from `HOME`, this
    /// machine's ID and this process's uid.
    ///
    /// # Errors
    ///
    /// Returns an error when neither variable is set to an absolute path, or
    /// when this machine has no readable ID.
    pub(crate) fn from_env() -> io::Result<Self> {
        Self::resolve(
            |name| env::var_os(name).map(PathBuf::from),
            machine_id,
            uid(),
        )
    }

    fn resolve(
        var: impl Fn(&str) -> Option<PathBuf>,
        machine_id: impl FnOnce() -> io::Result<String>,
        uid: u32,
    ) -> io::Result<Self> {
        let absolute = |name| var(name).filter(|path| path.is_absolute());
        if let Some(dir) = absolute(HOME_ENV) {
            return Ok(Self::at(dir));
        }
        let Some(home) = absolute("HOME") else {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("cannot find a home directory: set HOME or {HOME_ENV} to an absolute path"),
            ));
        };
        let id = machine_id()?;
        let root = home.join(DIRECTORY_NAME);
        Ok(Self {
            dir: root.join(HOSTS).join(&id),
            run: PathBuf::from(format!("/tmp/{RUN_PREFIX}-{uid}")),
            name: Some(Name {
                by_name: root.join(BY_NAME),
                id,
            }),
        })
    }

    /// Both places under `dir`, as [`HOME_ENV`] puts them.
    pub(crate) fn at(dir: impl Into<PathBuf>) -> Self {
        let dir = dir.into();
        Self {
            run: dir.join(RUN),
            dir,
            name: None,
        }
    }

    /// A host directory and a connections directory placed apart, as a machine
    /// sees them; or as two sessions of one machine see them when one has a
    /// `/tmp` of its own.
    #[cfg(test)]
    pub(crate) fn split(dir: impl Into<PathBuf>, run: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            run: run.into(),
            name: None,
        }
    }

    /// The host directory.
    pub(crate) fn dir(&self) -> &Path {
        &self.dir
    }

    /// Whether [`HOME_ENV`] or a test placed the directory, so a server this
    /// process starts must be told where it is.
    pub(crate) fn is_overridden(&self) -> bool {
        self.name.is_none()
    }

    /// The public API: newline-delimited JSON.
    pub(crate) fn api_socket(&self) -> PathBuf {
        self.run.join("api.sock")
    }

    /// The private client protocol: gRPC.
    pub(crate) fn client_socket(&self) -> PathBuf {
        self.run.join("client.sock")
    }

    /// The connections directory, which a cleaner may remove under a running
    /// server.
    #[cfg(test)]
    pub(crate) fn run_dir(&self) -> &Path {
        &self.run
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
        self.run.join("ssh")
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

    /// Creates both places and the host directory's `logs/` if they are
    /// missing, and checks that both are this user's alone.
    ///
    /// Private is what protects the sockets: they are reachable only through
    /// their directory, so a socket's own mode is a second fence, not the first.
    ///
    /// # Errors
    ///
    /// Returns an error when a directory cannot be created, or is another
    /// user's, a symlink, or open to others.
    pub(crate) fn create(&self) -> io::Result<()> {
        private_dir(&self.dir)?;
        private_dir(&self.run)?;
        fs::create_dir_all(self.logs())
    }

    /// Links `by-name/<hostname>` to this machine's directory, for a person
    /// reading the tree, and removes every other name linked to it. Nothing in
    /// File Peeker follows these links, so a stale or colliding one costs
    /// nothing; a directory [`HOME_ENV`] placed has none.
    ///
    /// # Errors
    ///
    /// Returns an error when the link cannot be written.
    pub(crate) fn name_this_machine(&self) -> io::Result<()> {
        self.name_as(&hostname())
    }

    fn name_as(&self, hostname: &OsStr) -> io::Result<()> {
        let Some(name) = &self.name else {
            return Ok(());
        };
        let bytes = hostname.as_bytes();
        if bytes.is_empty() || bytes.starts_with(b".") || bytes.contains(&b'/') {
            return Ok(());
        }
        private_dir(&name.by_name)?;
        // Relative, since nodes may mount one home at different paths.
        let target = Path::new("..").join(HOSTS).join(&name.id);
        for entry in fs::read_dir(&name.by_name)? {
            let entry = entry?;
            if entry.file_name() != hostname
                && fs::read_link(entry.path()).is_ok_and(|link| link == target)
            {
                fs::remove_file(entry.path())?;
            }
        }
        // Written aside and renamed over, so the name is never missing.
        let mut temporary = OsStr::new(".").to_owned();
        temporary.push(hostname);
        temporary.push(format!(".{}.tmp", std::process::id()));
        let temporary = name.by_name.join(temporary);
        let _ = fs::remove_file(&temporary);
        std::os::unix::fs::symlink(&target, &temporary)?;
        fs::rename(&temporary, name.by_name.join(hostname))
    }
}

/// Creates `dir` mode `0700` if it is missing, and refuses it unless it is a
/// directory, not a symlink, owned by this user and closed to everyone else.
/// `/tmp` is shared with every user, so a directory someone else made there
/// first is never used; and a `sudo` that kept `HOME` cannot write root's files
/// into a user's directory.
///
/// # Errors
///
/// Returns an error saying which of those `dir` is not.
pub(crate) fn private_dir(dir: &Path) -> io::Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(DIRECTORY_MODE)
        .create(dir)?;
    let metadata = fs::symlink_metadata(dir)?;
    let refuse = |why: String| {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("refusing {}: {why}", dir.display()),
        ))
    };
    if !metadata.is_dir() {
        return refuse("it is not a directory".to_owned());
    }
    if metadata.uid() != uid() {
        return refuse(format!(
            "it belongs to uid {}, not this user's {}",
            metadata.uid(),
            uid()
        ));
    }
    let mode = metadata.permissions().mode() & 0o777;
    if mode != DIRECTORY_MODE {
        return refuse(format!(
            "its mode is {mode:o}, not {DIRECTORY_MODE:o}; `chmod {DIRECTORY_MODE:o}` it if it is yours"
        ));
    }
    Ok(())
}

/// The real uid of this process, from the kernel rather than any variable.
pub(crate) fn uid() -> u32 {
    rustix::process::getuid().as_raw()
}

/// The node name `uname -n` prints, which is the hostname.
fn hostname() -> std::ffi::OsString {
    OsStr::from_bytes(rustix::system::uname().nodename().to_bytes()).to_owned()
}

/// This machine's ID, as 32 lowercase hex digits: Linux's `machine-id`, or the
/// hardware UUID macOS reports as `IOPlatformUUID`. Never the hostname, which
/// macOS changes with the network.
///
/// # Errors
///
/// Returns an error saying where it looked when there is none to read; a
/// machine without one does not fall back to another key.
pub(crate) fn machine_id() -> io::Result<String> {
    platform_machine_id().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("cannot read this machine's ID from {MACHINE_ID_SOURCES}"),
        )
    })
}

#[cfg(target_os = "linux")]
const MACHINE_ID_SOURCES: &str = "/etc/machine-id or /var/lib/dbus/machine-id";
#[cfg(target_os = "macos")]
const MACHINE_ID_SOURCES: &str = "IOPlatformUUID";

#[cfg(target_os = "linux")]
fn platform_machine_id() -> Option<String> {
    ["/etc/machine-id", "/var/lib/dbus/machine-id"]
        .into_iter()
        .filter_map(|path| fs::read_to_string(path).ok())
        .find_map(|raw| normalize_machine_id(&raw))
}

/// Read as the shell reads it, through `ioreg`, so the two cannot disagree on
/// how to ask.
#[cfg(target_os = "macos")]
fn platform_machine_id() -> Option<String> {
    let output = std::process::Command::new("ioreg")
        .args(["-rd1", "-c", "IOPlatformExpertDevice"])
        .output()
        .ok()?;
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .find(|line| line.contains("\"IOPlatformUUID\""))
        .and_then(|line| line.rsplit('"').nth(1))
        .and_then(normalize_machine_id)
}

/// 32 hex digits, lowercased, any dashes and surrounding whitespace dropped; an
/// all-zero ID is no ID.
fn normalize_machine_id(raw: &str) -> Option<String> {
    let id: String = raw
        .trim()
        .chars()
        .filter(|c| *c != '-')
        .map(|c| c.to_ascii_lowercase())
        .collect();
    (id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit()) && id.bytes().any(|b| b != b'0'))
        .then_some(id)
}

/// Shell that sets `FPK_HOST_DIR` to this build's host directory on whatever
/// machine runs it, exported for the commands after it. It runs ahead of every
/// command a client sends over SSH, and computes the ID each time, since one
/// SSH target may reach a different node each time. It mirrors
/// [`Home::from_env`], and a test holds the two together.
pub(crate) fn host_dir_script() -> String {
    format!(
        r#"if [ -n "${{{HOME_ENV}:-}}" ]; then
    FPK_HOST_DIR=${HOME_ENV}
else
    fpk_id() {{
        id=$(printf %s "$1" | tr -d ' \t\n-' | tr 'A-F' 'a-f')
        case "$id" in *[!0-9a-f]*) return 1 ;; esac
        # Empty, or all zeros: no ID.
        case "$id" in *[!0]*) ;; *) return 1 ;; esac
        [ ${{#id}} -eq 32 ]
    }}
    id=
    case "$(uname -s)" in
    Darwin)
        fpk_id "$(ioreg -rd1 -c IOPlatformExpertDevice | sed -n 's/.*"IOPlatformUUID" = "\(.*\)".*/\1/p')" || id=
        ;;
    *)
        for f in /etc/machine-id /var/lib/dbus/machine-id; do
            if [ -r "$f" ] && fpk_id "$(cat "$f")"; then break; fi
            id=
        done
        ;;
    esac
    if [ -z "$id" ]; then
        echo "fpk: cannot read this machine's ID" >&2
        exit 2
    fi
    FPK_HOST_DIR="$HOME/{DIRECTORY_NAME}/{HOSTS}/$id"
fi
export FPK_HOST_DIR"#
    )
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

    const ID: &str = "0123456789abcdef0123456789abcdef";

    fn resolve(vars: &[(&str, &str)]) -> io::Result<Home> {
        Home::resolve(
            |name| {
                vars.iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| PathBuf::from(value))
            },
            || Ok(ID.to_owned()),
            501,
        )
    }

    #[test]
    fn override_wins_and_holds_the_connections_too() {
        let home = resolve(&[(HOME_ENV, "/tmp/fpk"), ("HOME", "/home/me")]).unwrap();
        assert_eq!(home.dir(), Path::new("/tmp/fpk"));
        assert_eq!(home.client_socket(), Path::new("/tmp/fpk/run/client.sock"));
        assert!(home.is_overridden());
    }

    #[test]
    fn defaults_to_this_machines_directory_and_a_short_one_in_tmp() {
        let home = resolve(&[("HOME", "/home/me")]).unwrap();
        let dir = Path::new("/home/me")
            .join(DIRECTORY_NAME)
            .join("hosts")
            .join(ID);
        assert_eq!(home.dir(), dir);
        assert_eq!(home.lock(), dir.join("server.lock"));
        assert_eq!(home.server_log(), dir.join("logs/server.log"));
        assert_eq!(home.machines_file(), dir.join("machines.toml"));
        let run = PathBuf::from(format!("/tmp/{RUN_PREFIX}-501"));
        assert_eq!(home.api_socket(), run.join("api.sock"));
        assert_eq!(home.client_socket(), run.join("client.sock"));
        assert_eq!(home.ssh_dir(), run.join("ssh"));
        assert!(!home.is_overridden());
    }

    #[test]
    fn xdg_variables_do_not_move_it() {
        let home = resolve(&[("XDG_CONFIG_HOME", "/xdg"), ("HOME", "/home/me")]).unwrap();
        assert!(
            home.dir()
                .starts_with(Path::new("/home/me").join(DIRECTORY_NAME))
        );
    }

    #[test]
    fn a_relative_path_is_ignored() {
        let home = resolve(&[(HOME_ENV, "relative"), ("HOME", "/home/me")]).unwrap();
        assert!(
            home.dir()
                .starts_with(Path::new("/home/me").join(DIRECTORY_NAME))
        );
        assert!(resolve(&[("HOME", "relative")]).is_err());
    }

    #[test]
    fn a_machine_without_an_id_is_refused_rather_than_keyed_otherwise() {
        let error = Home::resolve(
            |name| (name == "HOME").then(|| PathBuf::from("/home/me")),
            || Err(io::Error::new(io::ErrorKind::NotFound, "no ID")),
            501,
        )
        .unwrap_err();
        assert_eq!(error.to_string(), "no ID");
    }

    #[test]
    fn machine_ids_are_normalized_and_empty_ones_refused() {
        assert_eq!(
            normalize_machine_id(&format!("{ID}\n")).as_deref(),
            Some(ID)
        );
        assert_eq!(
            normalize_machine_id("01234567-89AB-CDEF-0123-456789ABCDEF").as_deref(),
            Some(ID)
        );
        for bad in [
            "",
            "\n",
            "uninitialized",
            &"0".repeat(32),
            &ID[..31],
            "g".repeat(32).as_str(),
        ] {
            assert_eq!(normalize_machine_id(bad), None, "{bad:?}");
        }
        assert!(machine_id().is_ok_and(|id| normalize_machine_id(&id) == Some(id.clone())));
    }

    #[test]
    fn the_shell_finds_the_directory_this_does() {
        let root = tempfile::tempdir().unwrap();
        // Every POSIX shell a host may have that this one does too: `dash` is
        // Debian's `sh`, and stricter than bash's.
        let shells: Vec<&str> = ["sh", "/bin/dash"]
            .into_iter()
            .filter(|shell| *shell == "sh" || Path::new(shell).exists())
            .collect();
        let run = |shell: &str, vars: &[(&str, &Path)]| {
            let mut command = std::process::Command::new(shell);
            command
                .arg("-c")
                .arg(format!(
                    "{}\nprintf %s \"$FPK_HOST_DIR\"",
                    host_dir_script()
                ))
                .env_remove(HOME_ENV);
            for (key, value) in vars {
                command.env(key, value);
            }
            let output = command.output().unwrap();
            assert!(output.status.success(), "{output:?}");
            PathBuf::from(String::from_utf8(output.stdout).unwrap())
        };
        let home = Home::resolve(
            |name| (name == "HOME").then(|| root.path().to_owned()),
            machine_id,
            uid(),
        )
        .unwrap();
        let elsewhere = root.path().join("elsewhere");
        for shell in shells {
            assert_eq!(run(shell, &[("HOME", root.path())]), home.dir(), "{shell}");
            assert_eq!(
                run(shell, &[("HOME", root.path()), (HOME_ENV, &elsewhere)]),
                elsewhere,
                "{shell}"
            );
        }
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
    fn create_makes_both_places_private() {
        let root = tempfile::tempdir().unwrap();
        let home = Home::at(root.path().join("nested").join("home"));
        home.create().unwrap();
        assert!(home.server_log().parent().unwrap().is_dir());
        for dir in [home.dir(), home.run_dir()] {
            let mode = fs::metadata(dir).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, DIRECTORY_MODE, "{}", dir.display());
        }
    }

    #[test]
    fn a_directory_open_to_others_or_a_symlink_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let open = root.path().join("open");
        fs::create_dir(&open).unwrap();
        fs::set_permissions(&open, fs::Permissions::from_mode(0o755)).unwrap();
        let error = private_dir(&open).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(error.to_string().contains("mode is 755"), "{error}");

        let real = root.path().join("real");
        private_dir(&real).unwrap();
        let link = root.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let error = private_dir(&link).unwrap_err();
        assert!(error.to_string().contains("not a directory"), "{error}");
    }

    #[test]
    fn a_renamed_machine_leaves_only_its_current_name() {
        let root = tempfile::tempdir().unwrap();
        let home = Home::resolve(
            |name| (name == "HOME").then(|| root.path().to_owned()),
            || Ok(ID.to_owned()),
            uid(),
        )
        .unwrap();
        home.create().ok();
        let by_name = root.path().join(DIRECTORY_NAME).join("by-name");
        let ours = Path::new("..").join("hosts").join(ID);
        let theirs = Path::new("..").join("hosts").join("f".repeat(32));

        home.name_as(OsStr::new("old-name")).unwrap();
        std::os::unix::fs::symlink(&theirs, by_name.join("other")).unwrap();
        home.name_as(OsStr::new("new-name")).unwrap();

        assert_eq!(fs::read_link(by_name.join("new-name")).unwrap(), ours);
        assert!(fs::symlink_metadata(by_name.join("old-name")).is_err());
        assert_eq!(fs::read_link(by_name.join("other")).unwrap(), theirs);
        assert!(by_name.join("new-name").join("logs").is_dir());
        let names: Vec<_> = fs::read_dir(&by_name)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names.len(), 2, "{names:?}");

        // A directory an override placed has no name to give.
        Home::at(root.path().join("plain"))
            .name_as(OsStr::new("x"))
            .unwrap();
    }
}
