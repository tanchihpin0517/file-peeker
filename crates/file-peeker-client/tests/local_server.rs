use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

use file_peeker_client::{BrowserClient, ClientConfig, EntryKind, ServerTarget};
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

    let client = BrowserClient::start(ClientConfig {
        target: ServerTarget::Local {
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

    let listing = client
        .start_listing(browse_root.to_string_lossy().into_owned())
        .await
        .expect("listing should start");
    let mut entries = Vec::new();
    while let Some(entry) = listing
        .next_entry()
        .await
        .expect("listing should stream successfully")
    {
        entries.push(entry);
    }
    assert!(entries.iter().any(|entry| {
        entry.name == "notes.txt" && entry.kind == EntryKind::File && !entry.navigable
    }));
    assert!(entries.iter().any(|entry| {
        entry.name == "docs" && entry.kind == EntryKind::Directory && entry.navigable
    }));
    assert!(entries.iter().any(|entry| {
        entry.name == "docs-link" && entry.kind == EntryKind::Symlink && entry.navigable
    }));

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

async fn verify_shared_tree(client: &BrowserClient, browse_root: &Path) {
    let root_rows = client
        .load_tree(browse_root.to_string_lossy().into_owned())
        .await
        .expect("shared tree should load");
    let docs_path = browse_root.join("docs").to_string_lossy().into_owned();
    assert!(root_rows.iter().any(|row| row.entry.path == docs_path));

    let expanded_rows = client
        .expand_tree(docs_path.clone())
        .await
        .expect("nested directory should expand");
    assert!(expanded_rows.iter().any(|row| {
        row.parent_path.as_deref() == Some(docs_path.as_str()) && row.entry.name == "first.txt"
    }));
    let collapsed_rows = client
        .collapse_tree(docs_path.clone())
        .expect("nested directory should collapse");
    assert!(
        collapsed_rows
            .iter()
            .all(|row| row.parent_path.as_deref() != Some(docs_path.as_str()))
    );

    fs::write(browse_root.join("docs/added-later.txt"), "later")
        .expect("new nested fixture file should be written");
    let reexpanded_rows = client
        .expand_tree(docs_path.clone())
        .await
        .expect("nested directory should freshly re-expand");
    let nested_names: std::collections::HashSet<&str> = reexpanded_rows
        .iter()
        .filter(|row| row.parent_path.as_deref() == Some(docs_path.as_str()))
        .map(|row| row.entry.name.as_str())
        .collect();
    assert_eq!(
        nested_names,
        std::collections::HashSet::from(["first.txt", "added-later.txt"])
    );
    assert_eq!(client.tree_rows(), reexpanded_rows);
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
