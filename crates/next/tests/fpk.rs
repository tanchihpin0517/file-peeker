//! `fpk` as a person runs it: the real binary, a real detached server.

use std::{
    path::Path,
    process::{Command, Output},
};

const HOME_ENV: &str = "FILE_PEEKER_HOME";

fn fpk(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_fpk"))
        .args(args)
        .env(HOME_ENV, home)
        .output()
        .unwrap()
}

fn stdout(output: &Output) -> String {
    assert!(
        output.status.success(),
        "fpk failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout.clone()).unwrap()
}

/// Stops the detached server however the test ends, so a failed assertion does
/// not leave one running.
struct StopOnDrop<'a>(&'a Path);

impl Drop for StopOnDrop<'_> {
    fn drop(&mut self) {
        let _ = fpk(self.0, &["server", "stop"]);
    }
}

#[test]
fn the_server_outlives_the_client_that_started_it() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let _guard = StopOnDrop(&home);

    let first = stdout(&fpk(&home, &[]));
    assert!(first.starts_with("started and attached to"), "{first}");

    // That `fpk` has exited; the server it started has not.
    let second = stdout(&fpk(&home, &[]));
    assert!(second.starts_with("attached to"), "{second}");
    let pid = |line: &str| line.rsplit("pid ").next().unwrap().to_owned();
    assert_eq!(pid(&first), pid(&second));

    let status = stdout(&fpk(&home, &["status"]));
    assert!(status.contains("status: running"), "{status}");
    assert!(status.contains("(compatible)"), "{status}");

    assert_eq!(stdout(&fpk(&home, &["server", "stop"])), "server stopped\n");
    let status = stdout(&fpk(&home, &["status"]));
    assert!(status.contains("status: not running"), "{status}");
    assert!(home.join("logs").join("server.log").exists());
}

#[test]
fn stop_then_attach_starts_a_fresh_server() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let _guard = StopOnDrop(&home);

    stdout(&fpk(&home, &[]));
    stdout(&fpk(&home, &["server", "stop"]));
    let again = stdout(&fpk(&home, &[]));
    assert!(again.starts_with("started and attached to"), "{again}");
}

#[test]
fn concurrent_starts_share_one_server() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let _guard = StopOnDrop(&home);

    // Spawned before any is waited on, so all four race for the lock.
    let starts: Vec<_> = (0..4)
        .map(|_| {
            Command::new(env!("CARGO_BIN_EXE_fpk"))
                .env(HOME_ENV, &home)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    let mut pids: Vec<String> = starts
        .into_iter()
        .map(|child| {
            let line = stdout(&child.wait_with_output().unwrap());
            line.trim_end().rsplit("pid ").next().unwrap().to_owned()
        })
        .collect();
    pids.dedup();
    assert_eq!(pids.len(), 1, "{pids:?}");
}

#[test]
fn stopping_with_no_server_is_not_an_error() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    assert_eq!(
        stdout(&fpk(&home, &["server", "stop"])),
        "no server is running\n"
    );
}
