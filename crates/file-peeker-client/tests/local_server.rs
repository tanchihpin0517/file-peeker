use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

use file_peeker_client::{Client, Session, SessionConfig, SessionTarget};
use tempfile::TempDir;
use tokio::time::sleep;

#[tokio::test]
#[ignore = "run through scripts/test-local-client-server.sh"]
async fn starts_and_stops_the_real_local_server() {
    let real_server = PathBuf::from(
        std::env::var_os("FILE_PEEKER_TEST_SERVER")
            .expect("FILE_PEEKER_TEST_SERVER must point to the built server executable"),
    );
    let fixture = tempfile::tempdir().expect("fixture directory should be created");
    let wrapper = create_wrapper(&fixture, &real_server);

    let client = Client::new()
        .connect(SessionConfig {
            target: SessionTarget::Local {
                server_executable_path: wrapper.to_string_lossy().into_owned(),
            },
        })
        .await
        .expect("client startup should complete");

    let browse_root = fixture.path().join("browse");
    fs::create_dir(&browse_root).expect("browse directory should be created");
    fs::write(browse_root.join("notes.txt"), "hello").expect("fixture file should be written");
    fs::create_dir(browse_root.join("docs")).expect("fixture directory should be created");
    fs::write(browse_root.join("docs/first.txt"), "first")
        .expect("nested fixture file should be written");
    std::os::unix::fs::symlink("docs", browse_root.join("docs-link"))
        .expect("fixture symlink should be created");

    verify_shared_tree(&client, &browse_root).await;

    let socket_record = fixture.path().join("socket-path");
    wait_until(
        || socket_record.is_file(),
        "server wrapper did not record its socket",
    )
    .await;
    let socket_path = PathBuf::from(
        fs::read_to_string(&socket_record)
            .expect("socket record should be readable")
            .trim(),
    );
    assert!(
        socket_path.exists(),
        "server socket should exist after startup"
    );

    let current_root = client
        .current_root()
        .await
        .expect("server current root should be available");
    assert!(Path::new(&current_root).is_absolute());

    client
        .close()
        .await
        .expect("explicit client shutdown should complete");
    client
        .close()
        .await
        .expect("explicit client shutdown should be idempotent");

    let pid = fs::read_to_string(fixture.path().join("pid"))
        .expect("PID record should be readable")
        .trim()
        .to_owned();
    wait_until(
        || !process_exists(&pid) && !socket_path.exists(),
        "server process or socket remained after dropping the client",
    )
    .await;
    assert!(
        !socket_path
            .parent()
            .expect("socket should have a parent")
            .exists(),
        "private endpoint directory should be removed"
    );
}

#[tokio::test]
#[ignore = "run through scripts/test-local-client-server.sh"]
async fn separate_sessions_have_independent_lifecycles() {
    let real_server = PathBuf::from(
        std::env::var_os("FILE_PEEKER_TEST_SERVER")
            .expect("FILE_PEEKER_TEST_SERVER must point to the built server executable"),
    );
    let first_fixture = tempfile::tempdir().expect("first fixture should be created");
    let second_fixture = tempfile::tempdir().expect("second fixture should be created");
    let first_wrapper = create_wrapper(&first_fixture, &real_server);
    let second_wrapper = create_wrapper(&second_fixture, &real_server);

    let client = Client::new();
    let first = client
        .connect(SessionConfig {
            target: SessionTarget::Local {
                server_executable_path: first_wrapper.to_string_lossy().into_owned(),
            },
        })
        .await
        .expect("first session should start");
    let second = client
        .connect(SessionConfig {
            target: SessionTarget::Local {
                server_executable_path: second_wrapper.to_string_lossy().into_owned(),
            },
        })
        .await
        .expect("second session should start");
    let second_state = std::sync::Arc::clone(&second)
        .open_state(second_fixture.path().to_string_lossy().into_owned())
        .await
        .expect("second session should open a state");

    first.close().await.expect("first session should close");
    assert!(second.current_root().await.is_ok());
    assert_eq!(
        second_state.snapshot().path,
        second_fixture.path().to_string_lossy()
    );
    second.close().await.expect("second session should close");
}

async fn verify_shared_tree(client: &std::sync::Arc<Session>, browse_root: &Path) {
    let state = std::sync::Arc::clone(client)
        .open_state(browse_root.to_string_lossy().into_owned())
        .await
        .expect("browsing state should open");
    let independent_state = std::sync::Arc::clone(client)
        .open_state(browse_root.to_string_lossy().into_owned())
        .await
        .expect("an independent browsing state should open");
    let root_snapshot = state.snapshot();
    let docs_path = browse_root.join("docs").to_string_lossy().into_owned();
    assert!(
        root_snapshot
            .rows
            .iter()
            .any(|row| row.entry.path == docs_path)
    );

    let expanded_snapshot = state
        .expand(docs_path.clone())
        .await
        .expect("nested directory should expand");
    assert!(expanded_snapshot.rows.iter().any(|row| {
        row.parent_path.as_deref() == Some(docs_path.as_str()) && row.entry.name == "first.txt"
    }));
    assert_eq!(independent_state.snapshot().rows, root_snapshot.rows);

    let collapsed_snapshot = state
        .collapse(docs_path.clone())
        .expect("nested directory should collapse");
    assert!(
        collapsed_snapshot
            .rows
            .iter()
            .all(|row| row.parent_path.as_deref() != Some(docs_path.as_str()))
    );

    fs::write(browse_root.join("docs/added-later.txt"), "later")
        .expect("new nested fixture file should be written");
    let reexpanded_snapshot = state
        .expand(docs_path.clone())
        .await
        .expect("nested directory should freshly re-expand");
    let nested_names: std::collections::HashSet<&str> = reexpanded_snapshot
        .rows
        .iter()
        .filter(|row| row.parent_path.as_deref() == Some(docs_path.as_str()))
        .map(|row| row.entry.name.as_str())
        .collect();
    assert_eq!(
        nested_names,
        std::collections::HashSet::from(["first.txt", "added-later.txt"])
    );
    assert_eq!(state.snapshot(), reexpanded_snapshot);
}

fn create_wrapper(fixture: &TempDir, real_server: &Path) -> PathBuf {
    let wrapper = fixture.path().join("server-wrapper");
    let script = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$$\" > {}\nprintf '%s\\n' \"$3\" > {}\nexec {} \"$@\"\n",
        shell_quote(&fixture.path().join("pid").to_string_lossy()),
        shell_quote(&fixture.path().join("socket-path").to_string_lossy()),
        shell_quote(&real_server.to_string_lossy())
    );
    fs::write(&wrapper, script).expect("wrapper should be written");
    let mut permissions = fs::metadata(&wrapper)
        .expect("wrapper metadata should exist")
        .permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&wrapper, permissions).expect("wrapper should be executable");
    wrapper
}

async fn wait_until(mut condition: impl FnMut() -> bool, failure: &str) {
    for _ in 0..200 {
        if condition() {
            return;
        }
        sleep(Duration::from_millis(10)).await;
    }
    panic!("{failure}");
}

fn process_exists(pid: &str) -> bool {
    Command::new("/bin/kill")
        .args(["-0", pid])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}
