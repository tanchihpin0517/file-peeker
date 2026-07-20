use std::{io, path::Path, process::Stdio};

use file_peeker_client::server::{LocalServer, LocalServerConfig, RemoteServer};
use tokio::{
    io::{AsyncWriteExt as _, copy},
    process::{Child, Command},
};

pub(crate) const DEFAULT_REMOTE_PROJECT_DIR: &str = ".file-peeker/debug/repo";

pub async fn run(
    destination: Option<&str>,
    force_install: bool,
    source: Option<&str>,
) -> io::Result<()> {
    let executable = if let Some(destination) = destination {
        let remote_project_dir = source.unwrap_or(DEFAULT_REMOTE_PROJECT_DIR);
        upload_and_install(destination, force_install, remote_project_dir).await?
    } else {
        let config = local_install_config(force_install, source);
        LocalServer::get_server_executable(&config)
            .await?
            .to_string_lossy()
            .into_owned()
    };
    println!("{executable}");
    Ok(())
}

fn local_install_config(force_install: bool, source: Option<&str>) -> LocalServerConfig {
    LocalServerConfig {
        force_install,
        local_source_path: Some(match source {
            Some(source) => Path::new(source).to_path_buf(),
            None => project_dir().to_path_buf(),
        }),
    }
}

pub(crate) async fn upload_and_install(
    destination: &str,
    force_install: bool,
    remote_project_dir: &str,
) -> io::Result<String> {
    upload_current_project(destination, remote_project_dir).await?;
    install_remote_server(destination, force_install, remote_project_dir).await
}

pub(crate) async fn upload_current_project(
    remote_server: &str,
    remote_project_dir: &str,
) -> io::Result<()> {
    upload_project_dir(project_dir(), remote_server, remote_project_dir).await
}

pub(crate) fn project_dir() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("client crate should be inside the workspace crates directory")
}

async fn upload_project_dir(
    project_dir: &Path,
    remote_server: &str,
    remote_project_dir: &str,
) -> io::Result<()> {
    upload_project_dir_with_commands(
        project_dir,
        remote_server,
        remote_project_dir,
        Path::new("git"),
        Path::new("tar"),
        Path::new("ssh"),
    )
    .await
}

async fn upload_project_dir_with_commands(
    project_dir: &Path,
    remote_server: &str,
    remote_project_dir: &str,
    git_executable: &Path,
    tar_executable: &Path,
    ssh_executable: &Path,
) -> io::Result<()> {
    let mut git = Command::new(git_executable);
    git.arg("-C")
        .arg(project_dir)
        .args([
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ])
        .kill_on_drop(true);
    let project_files = git.output().await?;
    if !project_files.status.success() {
        return Err(io::Error::other(format!(
            "cannot list project files: {}",
            String::from_utf8_lossy(&project_files.stderr).trim()
        )));
    }
    let project_files = filter_existing_project_files(project_dir, &project_files.stdout)?;

    let mut archive_command = Command::new(tar_executable);
    archive_command
        .env("COPYFILE_DISABLE", "1")
        .args(["--no-xattrs", "-czf", "-", "-C"])
        .arg(project_dir)
        .args(["--null", "-T", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .kill_on_drop(true);
    let mut archive = archive_command.spawn()?;
    let Some(mut archive_stdin) = archive.stdin.take() else {
        stop_process(&mut archive).await;
        return Err(io::Error::other("tar standard input is unavailable"));
    };
    let Some(mut archive_stdout) = archive.stdout.take() else {
        drop(archive_stdin);
        stop_process(&mut archive).await;
        return Err(io::Error::other("tar standard output is unavailable"));
    };

    let mut upload_command = Command::new(ssh_executable);
    upload_command
        .arg(remote_server)
        .arg(remote_extract_command(remote_project_dir))
        .stdin(Stdio::piped())
        .kill_on_drop(true);
    let mut upload = match upload_command.spawn() {
        Ok(upload) => upload,
        Err(error) => {
            stop_process(&mut archive).await;
            return Err(error);
        }
    };
    let Some(mut upload_stdin) = upload.stdin.take() else {
        stop_process(&mut upload).await;
        stop_process(&mut archive).await;
        return Err(io::Error::other("SSH standard input is unavailable"));
    };

    let feed_archive = async move {
        archive_stdin.write_all(&project_files).await?;
        archive_stdin.shutdown().await
    };
    let stream_archive = async move {
        copy(&mut archive_stdout, &mut upload_stdin).await?;
        upload_stdin.shutdown().await
    };
    if let Err(error) = tokio::try_join!(feed_archive, stream_archive) {
        stop_process(&mut upload).await;
        stop_process(&mut archive).await;
        return Err(error);
    }

    let (ssh_status, archive_status) = tokio::try_join!(upload.wait(), archive.wait())?;

    if !ssh_status.success() {
        return Err(io::Error::other(format!(
            "SSH project upload failed with {ssh_status}"
        )));
    }
    if !archive_status.success() {
        return Err(io::Error::other(format!(
            "project archive failed with {archive_status}"
        )));
    }
    Ok(())
}

fn filter_existing_project_files(project_dir: &Path, files: &[u8]) -> io::Result<Vec<u8>> {
    let mut existing_files = Vec::new();
    for file in files
        .split(|byte| *byte == 0)
        .filter(|file| !file.is_empty())
    {
        let file = std::str::from_utf8(file)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        if project_dir.join(file).symlink_metadata().is_ok() {
            existing_files.extend_from_slice(file.as_bytes());
            existing_files.push(0);
        }
    }
    Ok(existing_files)
}

fn remote_extract_command(remote_project_dir: &str) -> String {
    let remote_project_dir = shell_quote(remote_project_dir);
    format!("mkdir -p {remote_project_dir} && tar -xzf - -C {remote_project_dir}")
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

async fn install_remote_server(
    destination: &str,
    force_install: bool,
    local_source_path: &str,
) -> io::Result<String> {
    let (_port, mut child, mut ssh_stdin, mut ssh_stdout) =
        RemoteServer::create_ssh_connection(Path::new("ssh"), destination).await?;
    let mut command_output = tokio::io::stdout();
    let executable = RemoteServer::get_server_executable(
        &mut ssh_stdin,
        &mut ssh_stdout,
        force_install,
        Some(local_source_path),
        &mut command_output,
    )
    .await;

    ssh_stdin.write_all(b"exit\n").await?;
    ssh_stdin.flush().await?;
    drop(ssh_stdin);
    child.wait().await?;

    executable
}

async fn stop_process(child: &mut Child) {
    let _ = child.start_kill();
    let _ = child.wait().await;
}

#[cfg(test)]
mod tests {
    use std::{fs, os::unix::fs::PermissionsExt as _, path::Path};

    use super::{
        filter_existing_project_files, local_install_config, project_dir, remote_extract_command,
        upload_project_dir_with_commands,
    };

    #[test]
    fn local_install_defaults_to_current_workspace() {
        let config = local_install_config(false, None);

        assert!(!config.force_install);
        assert_eq!(config.local_source_path.as_deref(), Some(project_dir()));
    }

    #[test]
    fn local_install_uses_explicit_source_and_force() {
        let config = local_install_config(true, Some("/tmp/file-peeker-source"));

        assert!(config.force_install);
        assert_eq!(
            config.local_source_path.as_deref(),
            Some(Path::new("/tmp/file-peeker-source"))
        );
    }

    #[test]
    fn remote_extract_command_quotes_project_dir() {
        assert_eq!(
            remote_extract_command("source dir's copy"),
            "mkdir -p 'source dir'\"'\"'s copy' && tar -xzf - -C 'source dir'\"'\"'s copy'"
        );
    }

    #[test]
    fn project_file_filter_removes_deleted_paths() {
        let files = b"Cargo.toml\0definitely-not-a-project-file\0";

        assert_eq!(
            filter_existing_project_files(project_dir(), files)
                .expect("project file list should be valid"),
            b"Cargo.toml\0"
        );
    }

    #[tokio::test]
    async fn upload_streams_archive_into_ssh() {
        let fixture = tempfile::tempdir().unwrap();
        let project = fixture.path().join("project");
        fs::create_dir(&project).unwrap();
        fs::write(project.join("payload.txt"), "payload").unwrap();
        let uploaded = fixture.path().join("uploaded");
        let git = fixture.path().join("git");
        let tar = fixture.path().join("tar");
        let ssh = fixture.path().join("ssh");
        write_executable(&git, "#!/bin/sh\nprintf 'payload.txt\\0'\n");
        write_executable(&tar, "#!/bin/sh\ncat\n");
        write_executable(
            &ssh,
            &format!("#!/bin/sh\ncat > {}\n", shell_quote(&uploaded)),
        );

        upload_project_dir_with_commands(
            &project,
            "example.test",
            ".file-peeker/repo",
            &git,
            &tar,
            &ssh,
        )
        .await
        .unwrap();

        assert_eq!(fs::read(uploaded).unwrap(), b"payload.txt\0");
    }

    #[tokio::test]
    async fn upload_reports_unsuccessful_ssh_status() {
        let fixture = tempfile::tempdir().unwrap();
        let project = fixture.path().join("project");
        fs::create_dir(&project).unwrap();
        fs::write(project.join("payload.txt"), "payload").unwrap();
        let git = fixture.path().join("git");
        let tar = fixture.path().join("tar");
        let ssh = fixture.path().join("ssh");
        write_executable(&git, "#!/bin/sh\nprintf 'payload.txt\\0'\n");
        write_executable(&tar, "#!/bin/sh\ncat\n");
        write_executable(&ssh, "#!/bin/sh\ncat >/dev/null\nexit 7\n");

        let error = upload_project_dir_with_commands(
            &project,
            "example.test",
            ".file-peeker/repo",
            &git,
            &tar,
            &ssh,
        )
        .await
        .unwrap_err();

        assert!(error.to_string().contains("SSH project upload failed"));
    }

    fn write_executable(path: &Path, contents: &str) {
        fs::write(path, contents).unwrap();
        let mut permissions = fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(path, permissions).unwrap();
    }

    fn shell_quote(path: &Path) -> String {
        format!("'{}'", path.to_string_lossy().replace('\'', "'\"'\"'"))
    }
}
