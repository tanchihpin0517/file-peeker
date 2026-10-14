//! Saved machines, end to end, without a real SSH host.
//!
//! `FILE_PEEKER_SSH` points at a stand-in for `ssh` that skips its options and
//! runs the remote command locally under a second home directory: the "host".
//! Everything else — the probe, starting the host's server, `--machine` — is
//! the real binary on both sides.

use std::{
    fs,
    os::unix::fs::PermissionsExt as _,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

const FAKE_SSH: &str = r#"#!/bin/sh
# Stands in for ssh: skips its options, then runs the remote command here, as
# a remote login shell would, with the fake host's directory as its override so
# the host's server binds its sockets there rather than in this machine's /tmp.
while [ $# -gt 0 ]; do
    case "$1" in
        -o) shift 2 ;;
        -T) shift ;;
        *) break ;;
    esac
done
shift
exec env FILE_PEEKER_HOME="$FAKE_HOST_HOME" HOME="$FAKE_HOST_HOME" sh -c "$*"
"#;

/// A client home and a host home, and the stand-in `ssh` between them.
struct World {
    _root: tempfile::TempDir,
    client: PathBuf,
    host: PathBuf,
    ssh: PathBuf,
}

impl World {
    fn new() -> Self {
        // Under /tmp rather than the system's per-user temporary directory: a
        // unix socket's path has a hard limit of about a hundred bytes, and the
        // host's socket sits three directories below this.
        let root = tempfile::Builder::new()
            .prefix("fpk")
            .tempdir_in("/tmp")
            .unwrap();
        let client = root.path().join("client");
        let host = root.path().join("host");
        fs::create_dir_all(&host).unwrap();
        // As private as `fpk` makes its own, or the host's server refuses it.
        fs::set_permissions(&host, fs::Permissions::from_mode(0o700)).unwrap();
        let ssh = root.path().join("ssh");
        fs::write(&ssh, FAKE_SSH).unwrap();
        fs::set_permissions(&ssh, fs::Permissions::from_mode(0o755)).unwrap();
        Self {
            _root: root,
            client,
            host,
            ssh,
        }
    }

    fn fpk(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_fpk"))
            .args(args)
            .env("FILE_PEEKER_HOME", &self.client)
            .env("FILE_PEEKER_SSH", &self.ssh)
            .env("FAKE_HOST_HOME", &self.host)
            .stdin(Stdio::null())
            .output()
            .unwrap()
    }

    /// Puts this very binary on the host, as a dev install of this source
    /// would have left it.
    fn install_on_host(&self) {
        let info = self.build_info();
        let bin = self.host_dir().join("bin");
        fs::create_dir_all(&bin).unwrap();
        fs::copy(env!("CARGO_BIN_EXE_fpk"), bin.join("fpk")).unwrap();
        if let Some(hash) = info["source_hash"].as_str() {
            fs::write(bin.join("fpk.source"), format!("{hash}\n")).unwrap();
        }
    }

    fn build_info(&self) -> serde_json::Value {
        serde_json::from_str(&stdout(&self.fpk(&["build-info"]))).unwrap()
    }

    /// The host's directory, which the stand-in `ssh` names as its override.
    fn host_dir(&self) -> PathBuf {
        self.host.clone()
    }

    fn host_server_running(&self) -> bool {
        self.host_dir().join("run/client.sock").exists()
    }
}

impl Drop for World {
    fn drop(&mut self) {
        let host_fpk = self.host_dir().join("bin/fpk");
        if host_fpk.exists() {
            let _ = Command::new(host_fpk)
                .args(["server", "stop"])
                .env("FILE_PEEKER_HOME", &self.host)
                .output();
        }
    }
}

fn stdout(output: &Output) -> String {
    assert!(
        output.status.success(),
        "fpk failed: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout.clone()).unwrap()
}

fn saved(client: &Path) -> String {
    fs::read_to_string(client.join("machines.toml")).unwrap_or_default()
}

#[test]
fn a_host_without_fpk_is_not_saved_unless_the_user_agrees() {
    let world = World::new();
    let output = world.fpk(&["machine", "add", "box", "me@box"]);
    assert!(!output.status.success());
    let said = String::from_utf8_lossy(&output.stdout);
    assert!(said.contains("nothing is installed"), "{said}");
    assert!(said.contains("pass --yes"), "{said}");
    assert!(saved(&world.client).is_empty());
    assert!(!world.host_dir().join("bin/fpk").exists());
}

#[test]
fn a_host_with_this_build_is_saved_and_its_server_started() {
    let world = World::new();
    world.install_on_host();
    let added = stdout(&world.fpk(&["machine", "add", "box", "me@box"]));
    assert!(added.contains("me@box has dev, source"), "{added}");
    assert!(added.contains("saved `box`"), "{added}");
    assert!(world.host_server_running());
    assert!(saved(&world.client).contains("ssh = \"me@box\""));

    // Again: nothing to do, nothing saved twice.
    let again = stdout(&world.fpk(&["machine", "add", "box", "me@box"]));
    assert!(again.contains("up to date"), "{again}");
    assert_eq!(saved(&world.client).matches("[[machine]]").count(), 1);

    let list = stdout(&world.fpk(&["machine", "list"]));
    assert_eq!(list.trim(), "box  me@box");
}

#[test]
fn machine_runs_a_command_on_the_host() {
    let world = World::new();
    world.install_on_host();
    stdout(&world.fpk(&["machine", "add", "box", "me@box"]));

    let status = stdout(&world.fpk(&["--machine", "box", "status"]));
    assert!(status.contains("status: running"), "{status}");
    assert!(
        status.contains(&format!("home: {}", world.host_dir().display())),
        "the status came from the client, not the host: {status}"
    );

    let demo = stdout(&world.fpk(&["task", "demo", "--seconds", "0", "--machine", "box"]));
    assert!(demo.starts_with("started demo.count task "), "{demo}");

    assert_eq!(
        stdout(&world.fpk(&["--machine", "box", "server", "stop"])),
        "server stopped\n"
    );
    assert!(!world.host_server_running());

    let unknown = world.fpk(&["--machine", "nowhere", "status"]);
    assert!(!unknown.status.success());
    assert!(String::from_utf8_lossy(&unknown.stderr).contains("no machine is saved as `nowhere`"));
}

#[test]
fn removing_a_machine_leaves_its_server_running() {
    let world = World::new();
    world.install_on_host();
    stdout(&world.fpk(&["machine", "add", "box", "me@box"]));
    let removed = stdout(&world.fpk(&["machine", "remove", "box"]));
    assert!(removed.contains("keep running"), "{removed}");
    assert!(world.host_server_running());
    assert!(!saved(&world.client).contains("box"));
    assert!(!world.fpk(&["machine", "remove", "box"]).status.success());
}

#[test]
fn a_label_is_one_word_and_cannot_move_hosts() {
    let world = World::new();
    world.install_on_host();
    assert!(
        !world
            .fpk(&["machine", "add", "local", "me@box"])
            .status
            .success()
    );
    stdout(&world.fpk(&["machine", "add", "box", "me@box"]));
    let moved = world.fpk(&["machine", "add", "box", "you@elsewhere"]);
    assert!(!moved.status.success());
    assert!(String::from_utf8_lossy(&moved.stderr).contains("already saved for me@box"));
}
