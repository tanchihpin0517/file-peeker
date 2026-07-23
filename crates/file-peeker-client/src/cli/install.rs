use std::{io, path::Path, process::Stdio};

use file_peeker_client::session::backend::connection::remote;
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt as _, AsyncWrite, AsyncWriteExt as _, copy},
    process::{Child, Command},
};

const DEFAULT_REMOTE_PROJECT_DIR: &str = ".file-peeker/debug/repo";
const SERVER_READY_PREFIX: &str = "FILE_PEEKER_SERVER_READY=";
const SERVER_ERROR_PREFIX: &str = "FILE_PEEKER_SERVER_ERROR=";
const SOURCE_INSTALL_SCRIPT: &str = include_str!("install-server-from-source.sh");
const SOURCE_INSTALL_HEREDOC: &str = "FILE_PEEKER_SOURCE_INSTALL_SCRIPT";

pub async fn run(destination: &str, force_install: bool, source: Option<&Path>) -> io::Result<()> {
    let executable = match source {
        Some(source) => {
            upload_project_dir(source, destination, DEFAULT_REMOTE_PROJECT_DIR).await?;
            install_remote_server_from_source(
                destination,
                force_install,
                DEFAULT_REMOTE_PROJECT_DIR,
            )
            .await?
        }
        None => install_remote_server(destination, force_install).await?,
    };
    println!("{executable}");
    Ok(())
}

async fn run_source_installer(
    server_stdin: &mut (impl AsyncWrite + Unpin),
    server_stdout: &mut (impl AsyncBufRead + Unpin),
    force_install: bool,
    source: &str,
    command_output: &mut (impl AsyncWrite + Unpin),
) -> io::Result<String> {
    server_stdin
        .write_all(source_install_command(force_install, source).as_bytes())
        .await?;
    server_stdin.flush().await?;

    loop {
        let mut line = String::new();
        if server_stdout.read_line(&mut line).await? == 0 || !line.ends_with('\n') {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "source installer closed stdout before reporting a result",
            ));
        }
        let status_line = line.trim_end_matches(['\r', '\n']);
        if let Some(executable) = status_line.strip_prefix(SERVER_READY_PREFIX) {
            return Ok(executable.to_owned());
        }
        if let Some(message) = status_line.strip_prefix(SERVER_ERROR_PREFIX) {
            return Err(io::Error::other(message.to_owned()));
        }
        command_output.write_all(line.as_bytes()).await?;
        command_output.flush().await?;
    }
}

fn source_install_command(force_install: bool, source: &str) -> String {
    let version = env!("CARGO_PKG_VERSION");
    let force_install = if force_install { "true" } else { "false" };
    format!(
        "sh -s -- '{version}' '{force_install}' {} <<'{SOURCE_INSTALL_HEREDOC}'\n{SOURCE_INSTALL_SCRIPT}{SOURCE_INSTALL_HEREDOC}\n",
        shell_quote(source)
    )
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

async fn install_remote_server(destination: &str, force_install: bool) -> io::Result<String> {
    let (_port, mut child, mut ssh_stdin, mut ssh_stdout) =
        remote::create_ssh_connection(Path::new("ssh"), destination).await?;
    let mut command_output = tokio::io::stdout();
    let executable = remote::get_server_executable(
        &mut ssh_stdin,
        &mut ssh_stdout,
        force_install,
        &mut command_output,
    )
    .await;

    ssh_stdin.write_all(b"exit\n").await?;
    ssh_stdin.flush().await?;
    drop(ssh_stdin);
    child.wait().await?;

    executable
}

async fn install_remote_server_from_source(
    destination: &str,
    force_install: bool,
    remote_source: &str,
) -> io::Result<String> {
    let (_port, mut child, mut ssh_stdin, mut ssh_stdout) =
        remote::create_ssh_connection(Path::new("ssh"), destination).await?;
    let mut command_output = tokio::io::stdout();
    let executable = run_source_installer(
        &mut ssh_stdin,
        &mut ssh_stdout,
        force_install,
        remote_source,
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
        SOURCE_INSTALL_SCRIPT, filter_existing_project_files, remote_extract_command,
        source_install_command, upload_project_dir_with_commands,
    };

    fn project_dir() -> &'static Path {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("client crate should be inside the workspace crates directory")
    }

    #[test]
    fn source_install_command_quotes_path_and_preserves_force() {
        let command = source_install_command(false, "/tmp/file peeker's source");
        let forced_command = source_install_command(true, "/tmp/file-peeker");
        let version = env!("CARGO_PKG_VERSION");

        assert!(command.starts_with(&format!(
            "sh -s -- '{version}' 'false' '/tmp/file peeker'\"'\"'s source' <<"
        )));
        assert!(forced_command.starts_with(&format!(
            "sh -s -- '{version}' 'true' '/tmp/file-peeker' <<"
        )));
        assert!(command.contains(SOURCE_INSTALL_SCRIPT));
        assert!(
            SOURCE_INSTALL_SCRIPT.contains("--path \"$source_root/crates/file-peeker-server\"")
        );
        assert!(SOURCE_INSTALL_SCRIPT.contains("--root \"$server_root\""));
        assert!(SOURCE_INSTALL_SCRIPT.contains("--force"));
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
