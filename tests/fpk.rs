//! `fpk` as a script runs it: the real binary, a real detached server, and no
//! terminal. The terminal UI itself is tested through its state and a test
//! backend inside the crate.

use std::{
    path::Path,
    process::{Command, Output, Stdio},
};

const HOME_ENV: &str = "FILE_PEEKER_HOME";

fn fpk(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_fpk"))
        .args(args)
        .env(HOME_ENV, home)
        .stdin(Stdio::null())
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

/// The `pid:` line of a status report.
fn pid(status: &str) -> String {
    status
        .lines()
        .find_map(|line| line.trim().strip_prefix("pid: "))
        .unwrap_or_else(|| panic!("no pid in {status}"))
        .to_owned()
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

    let first = stdout(&fpk(&home, &["server", "start"]));
    assert!(first.starts_with("started the server\n"), "{first}");

    // That `fpk` has exited; the server it started has not.
    let second = stdout(&fpk(&home, &["server", "start"]));
    assert!(
        second.starts_with("the server was already running\n"),
        "{second}"
    );
    assert_eq!(pid(&first), pid(&second));

    let status = stdout(&fpk(&home, &["status"]));
    assert!(status.contains("status: running"), "{status}");
    assert!(status.contains("(compatible)"), "{status}");
    assert_eq!(pid(&status), pid(&first));

    assert_eq!(stdout(&fpk(&home, &["server", "stop"])), "server stopped\n");
    let status = stdout(&fpk(&home, &["status"]));
    assert!(status.contains("status: not running"), "{status}");
    assert!(home.join("logs").join("server.log").exists());
}

#[test]
fn stop_then_start_starts_a_fresh_server() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let _guard = StopOnDrop(&home);

    stdout(&fpk(&home, &["server", "start"]));
    stdout(&fpk(&home, &["server", "stop"]));
    let again = stdout(&fpk(&home, &["server", "start"]));
    assert!(again.starts_with("started the server\n"), "{again}");
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
                .args(["server", "start"])
                .env(HOME_ENV, &home)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    let mut pids: Vec<String> = starts
        .into_iter()
        .map(|child| pid(&stdout(&child.wait_with_output().unwrap())))
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

#[test]
fn the_terminal_ui_refuses_without_a_terminal_and_starts_nothing() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let output = fpk(&home, &[]);
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("fpk server start"), "{stderr}");
    assert!(!home.join("file-peeker-client.sock").exists());
}

#[test]
fn a_demo_task_starts_the_server_and_answers_with_its_id() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let _guard = StopOnDrop(&home);

    let started = stdout(&fpk(&home, &["task", "demo", "--seconds", "1"]));
    let id = started
        .strip_prefix("started demo.count task ")
        .unwrap_or_else(|| panic!("{started}"))
        .trim();
    assert_eq!(id.len(), 36, "{id}");
    let status = stdout(&fpk(&home, &["status"]));
    assert!(status.contains("status: running"), "{status}");
}
