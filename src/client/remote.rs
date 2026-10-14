//! Reaching a server on another machine: probing what is installed there,
//! installing what this client needs, starting the server, and the bridge the
//! client protocol runs over.

use std::{
    io,
    path::Path,
    pin::Pin,
    process::Stdio,
    task::{Context, Poll},
};

use hyper_util::rt::TokioIo;
use sha2::{Digest as _, Sha256};
use tokio::{
    io::{AsyncRead, AsyncWrite, AsyncWriteExt as _, Join, ReadBuf},
    process::{Child, ChildStdin, ChildStdout, Command},
};
use tonic::transport::{Channel, Endpoint, Uri};
use tower::service_fn;

use super::{
    ClientError,
    ssh::{REMOTE_FPK, Ssh, quote},
};
use crate::{
    build_info,
    protocol::{CLIENT_PROTOCOL, VERSION, probe::Probe},
};

const RELEASE_SCRIPT: &str = include_str!("install-release.sh");
const DEV_SCRIPT: &str = include_str!("install-dev.sh");
/// Where release builds download from.
const REPOSITORY: &str = "https://github.com/tanchihpin0517/file-peeker";

/// What a probe found when it reached the host, or why it could not.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Reach {
    Probed(Probe),
    /// SSH wanted an answer it was not allowed to ask for, or authentication
    /// failed: someone has to sign in.
    NeedsLogin(String),
    /// The host could not be reached at all, for now.
    Unreachable(String),
}

/// Runs the probe on the host.
pub(crate) async fn probe(ssh: &Ssh) -> Reach {
    let remote = format!(
        "f={REMOTE_FPK}; if [ -x \"$f\" ]; then exec \"$f\" probe; else echo '{{\"installed\":false}}'; fi"
    );
    let output = ssh
        .command(&remote)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await;
    let output = match output {
        Ok(output) => output,
        Err(error) => return Reach::Unreachable(format!("cannot run ssh: {error}")),
    };
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if !output.status.success() {
        return classify_failure(output.status.code(), stderr);
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    match stdout
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .map(serde_json::from_str::<Probe>)
    {
        Some(Ok(probe)) => Reach::Probed(probe),
        _ => Reach::Unreachable(format!(
            "the host answered something other than a probe: {}",
            stdout.trim()
        )),
    }
}

/// SSH exits 255 when it is SSH that failed rather than the remote command;
/// among those, an authentication failure is one a person can fix by signing in.
fn classify_failure(code: Option<i32>, stderr: String) -> Reach {
    const LOGIN: [&str; 5] = [
        "Permission denied",
        "Host key verification failed",
        "keyboard-interactive",
        "Too many authentication failures",
        "password",
    ];
    if code == Some(255) && LOGIN.iter().any(|sign| stderr.contains(sign)) {
        Reach::NeedsLogin(stderr)
    } else if stderr.is_empty() {
        Reach::Unreachable(format!("ssh exited with {code:?}"))
    } else {
        Reach::Unreachable(stderr)
    }
}

/// The build this client installs on a host: its own release, or its own
/// source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Wanted {
    Release { version: String },
    Dev { source: Source },
}

/// A dev build's source, as it would be uploaded.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Source {
    pub dir: String,
    pub files: Vec<String>,
    pub hash: String,
}

impl Wanted {
    /// What this binary installs: decided by its build profile.
    ///
    /// # Errors
    ///
    /// Returns a message when a dev build's source cannot be read.
    pub(crate) fn for_this_build() -> Result<Self, String> {
        if build_info::RELEASE {
            Ok(Self::Release {
                version: VERSION.to_owned(),
            })
        } else {
            Source::read(Path::new(build_info::SOURCE_DIR)).map(|source| Self::Dev { source })
        }
    }

    pub(crate) fn describe(&self) -> String {
        match self {
            Self::Release { version } => format!("release v{version}"),
            Self::Dev { source } => {
                format!(
                    "dev, source {}",
                    crate::protocol::probe::short(&source.hash)
                )
            }
        }
    }

    /// Why `probe` is not this build, or `None` when it is.
    pub(crate) fn mismatch(&self, probe: &Probe) -> Option<String> {
        if !probe.installed {
            return Some("nothing is installed".to_owned());
        }
        let found = probe.describe();
        let same = match self {
            Self::Release { version } => probe.build == "release" && &probe.version == version,
            Self::Dev { source } => {
                probe.build == "dev" && probe.source_hash.as_deref() == Some(source.hash.as_str())
            }
        };
        if same && probe.client_protocol == CLIENT_PROTOCOL {
            None
        } else {
            Some(format!("it has {found}"))
        }
    }
}

impl Source {
    /// The files `git` lists under `dir` — tracked, or untracked and not
    /// ignored — and their hash. The shipping crate under `old/` is a workspace
    /// of its own that this build never reads, so it is left behind.
    ///
    /// # Errors
    ///
    /// Returns a message when `dir` is not a git checkout or cannot be read.
    pub(crate) fn read(dir: &Path) -> Result<Self, String> {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args([
                "ls-files",
                "--cached",
                "--others",
                "--exclude-standard",
                "-z",
            ])
            .output()
            .map_err(|error| format!("cannot run git in {}: {error}", dir.display()))?;
        if !output.status.success() {
            return Err(format!(
                "a dev build installs the source it was built from, and {} is not a git checkout: {}",
                dir.display(),
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        let mut files: Vec<String> = output
            .stdout
            .split(|byte| *byte == 0)
            .filter(|path| !path.is_empty())
            .map(|path| String::from_utf8_lossy(path).into_owned())
            .filter(|path| !path.starts_with("old/"))
            // A tracked file deleted in the working tree is listed but gone.
            .filter(|path| dir.join(path).is_file())
            .collect();
        files.sort();
        let mut hasher = Sha256::new();
        for path in &files {
            let content = std::fs::read(dir.join(path))
                .map_err(|error| format!("cannot read {path}: {error}"))?;
            hasher.update(path.as_bytes());
            hasher.update([0]);
            hasher.update((content.len() as u64).to_le_bytes());
            hasher.update(&content);
        }
        Ok(Self {
            dir: dir.display().to_string(),
            files,
            hash: format!("{:x}", hasher.finalize()),
        })
    }
}

/// Installs `wanted` on the host, its output passed through to this terminal.
///
/// # Errors
///
/// Returns a message saying which part failed.
pub(crate) async fn install(ssh: &Ssh, wanted: &Wanted) -> Result<(), String> {
    match wanted {
        Wanted::Release { version } => {
            run_script(ssh, RELEASE_SCRIPT, &[version.as_str(), REPOSITORY]).await
        }
        Wanted::Dev { source } => {
            upload(ssh, source).await?;
            run_script(ssh, DEV_SCRIPT, &[source.hash.as_str()]).await
        }
    }
}

/// Runs `script` on the host with `sh -s`, feeding it on standard input.
async fn run_script(ssh: &Ssh, script: &str, args: &[&str]) -> Result<(), String> {
    let quoted: Vec<String> = args.iter().map(|arg| quote(arg)).collect();
    let mut child = ssh
        .command(&format!("sh -s -- {}", quoted.join(" ")))
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|error| format!("cannot run ssh: {error}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(script.as_bytes())
            .await
            .map_err(|error| format!("cannot send the install script: {error}"))?;
    }
    let status = child
        .wait()
        .await
        .map_err(|error| format!("ssh did not finish: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("installing on {} failed ({status})", ssh.target()))
    }
}

/// Streams the source to the host's dev directory's `src/` as a tar archive,
/// replacing whatever source was there.
async fn upload(ssh: &Ssh, source: &Source) -> Result<(), String> {
    println!(
        "uploading {} files of dev source {}",
        source.files.len(),
        crate::protocol::probe::short(&source.hash)
    );
    let mut tar = Command::new("tar")
        // macOS's tar would otherwise add `._` files for extended attributes.
        .env("COPYFILE_DISABLE", "1")
        .args([
            "--no-xattrs",
            "-czf",
            "-",
            "-C",
            &source.dir,
            "--null",
            "-T",
            "-",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| format!("cannot run tar: {error}"))?;
    let archive: Stdio = tar
        .stdout
        .take()
        .ok_or("tar has no output")?
        .try_into()
        .map_err(|error| format!("cannot pipe tar into ssh: {error}"))?;
    let mut list = tar.stdin.take().ok_or("tar takes no input")?;
    let names = source.files.join("\0");
    let feed = async move {
        list.write_all(names.as_bytes()).await?;
        list.write_all(b"\0").await
    };
    let remote = "umask 077; src=\"$FPK_HOST_DIR/src\"; rm -rf \"$src\" && mkdir -p \"$src\" && tar -xzf - -C \"$src\"";
    let receive = ssh.command(remote).stdin(archive).status();
    let (listed, received) = tokio::join!(feed, receive);
    listed.map_err(|error| format!("cannot list the source for tar: {error}"))?;
    let tarred = tar
        .wait()
        .await
        .map_err(|error| format!("tar did not finish: {error}"))?;
    let received = received.map_err(|error| format!("cannot run ssh: {error}"))?;
    if tarred.success() && received.success() {
        Ok(())
    } else {
        Err(format!(
            "uploading the source to {} failed (tar {tarred}, ssh {received})",
            ssh.target()
        ))
    }
}

/// Starts the host's server if none runs, passing its report through.
///
/// # Errors
///
/// Returns a message when the server cannot be started.
pub(crate) async fn start_server(ssh: &Ssh) -> Result<(), String> {
    let status = ssh
        .command(&format!("{REMOTE_FPK} server start >/dev/null"))
        .status()
        .await
        .map_err(|error| format!("cannot run ssh: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "starting the server on {} failed ({status})",
            ssh.target()
        ))
    }
}

/// A client-protocol channel to the host's server, through its bridge: each
/// connection the channel makes is one `ssh <target> fpk bridge`, whose
/// standard input and output carry the protocol.
///
/// # Errors
///
/// Returns an error when the first bridge cannot be opened.
pub(crate) async fn channel(ssh: &Ssh) -> Result<Channel, ClientError> {
    let ssh = ssh.clone();
    let channel = Endpoint::from_static("http://file-peeker")
        .connect_with_connector(service_fn(move |_: Uri| {
            let ssh = ssh.clone();
            async move { Bridge::open(&ssh).map(TokioIo::new) }
        }))
        .await?;
    Ok(channel)
}

/// One bridge process, read and written as the stream it relays.
struct Bridge {
    // Held so the process lives exactly as long as the stream: dropped with it,
    // `kill_on_drop` ends it.
    _child: Child,
    io: Join<ChildStdout, ChildStdin>,
}

impl Bridge {
    fn open(ssh: &Ssh) -> io::Result<Self> {
        let mut child = ssh
            .command(&format!("exec {REMOTE_FPK} bridge"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("no stdout"))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("no stdin"))?;
        Ok(Self {
            _child: child,
            io: tokio::io::join(stdout, stdin),
        })
    }
}

impl AsyncRead for Bridge {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().io).poll_read(cx, buf)
    }
}

impl AsyncWrite for Bridge {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().io).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().io).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().io).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probe(build: &str, version: &str, hash: Option<&str>, protocol: u32) -> Probe {
        Probe {
            installed: true,
            version: version.to_owned(),
            client_protocol: protocol,
            build: build.to_owned(),
            source_hash: hash.map(str::to_owned),
            server_running: true,
        }
    }

    #[test]
    fn a_release_client_wants_its_own_release() {
        let wanted = Wanted::Release {
            version: "0.1.0".to_owned(),
        };
        assert_eq!(
            wanted.mismatch(&probe("release", "0.1.0", None, CLIENT_PROTOCOL)),
            None
        );
        assert!(
            wanted
                .mismatch(&probe("release", "0.0.9", None, CLIENT_PROTOCOL))
                .unwrap()
                .contains("release v0.0.9")
        );
        assert!(
            wanted
                .mismatch(&probe("dev", "0.1.0", Some("abc"), CLIENT_PROTOCOL))
                .is_some()
        );
        let none = Probe {
            installed: false,
            ..probe("", "", None, 0)
        };
        assert_eq!(wanted.mismatch(&none).unwrap(), "nothing is installed");
    }

    #[test]
    fn a_dev_client_wants_its_own_source() {
        let wanted = Wanted::Dev {
            source: Source {
                dir: String::new(),
                files: Vec::new(),
                hash: "a1b2c3d4e5".to_owned(),
            },
        };
        assert_eq!(
            wanted.mismatch(&probe("dev", "0.0.0", Some("a1b2c3d4e5"), CLIENT_PROTOCOL)),
            None
        );
        let stale = wanted
            .mismatch(&probe("dev", "0.0.0", Some("ffff0000"), CLIENT_PROTOCOL))
            .unwrap();
        assert!(stale.contains("dev, source ffff0000"), "{stale}");
        assert!(
            wanted
                .mismatch(&probe(
                    "dev",
                    "0.0.0",
                    Some("a1b2c3d4e5"),
                    CLIENT_PROTOCOL + 1
                ))
                .is_some()
        );
    }

    #[test]
    fn ssh_failures_split_into_logins_and_the_rest() {
        assert!(matches!(
            classify_failure(
                Some(255),
                "me@box: Permission denied (publickey).".to_owned()
            ),
            Reach::NeedsLogin(_)
        ));
        assert!(matches!(
            classify_failure(
                Some(255),
                "ssh: connect to host box port 22: Connection refused".to_owned()
            ),
            Reach::Unreachable(_)
        ));
        assert!(matches!(
            classify_failure(Some(1), "Permission denied".to_owned()),
            Reach::Unreachable(_)
        ));
    }

    #[test]
    fn the_source_hash_follows_content_and_skips_the_shipping_crate() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path();
        let git = |args: &[&str]| {
            assert!(
                std::process::Command::new("git")
                    .arg("-C")
                    .arg(dir)
                    .args(args)
                    .output()
                    .unwrap()
                    .status
                    .success()
            );
        };
        git(&["init", "-q"]);
        std::fs::write(dir.join("Cargo.toml"), "[package]\n").unwrap();
        std::fs::create_dir(dir.join("old")).unwrap();
        std::fs::write(dir.join("old/Cargo.toml"), "old\n").unwrap();
        std::fs::write(dir.join(".gitignore"), "target/\n").unwrap();
        std::fs::create_dir(dir.join("target")).unwrap();
        std::fs::write(dir.join("target/junk"), "x").unwrap();

        let first = Source::read(dir).unwrap();
        assert_eq!(first.files, [".gitignore", "Cargo.toml"]);
        assert_eq!(Source::read(dir).unwrap().hash, first.hash);

        std::fs::write(dir.join("old/Cargo.toml"), "changed\n").unwrap();
        assert_eq!(Source::read(dir).unwrap().hash, first.hash);
        std::fs::write(dir.join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
        assert_ne!(Source::read(dir).unwrap().hash, first.hash);

        let not_git = tempfile::tempdir().unwrap();
        assert!(
            Source::read(not_git.path())
                .unwrap_err()
                .contains("git checkout")
        );
    }

    #[test]
    fn the_release_script_names_the_assets_the_release_publishes() {
        for asset in [
            "fpk-$os-$arch",
            "linux",
            "macos",
            "x86_64",
            "aarch64",
            "SHA256SUMS",
        ] {
            assert!(RELEASE_SCRIPT.contains(asset), "{asset}");
        }
        assert!(DEV_SCRIPT.contains("cargo install --debug --locked"));
    }
}
