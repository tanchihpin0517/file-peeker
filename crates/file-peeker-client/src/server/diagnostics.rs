use std::{
    io::Write as _,
    path::{Path, PathBuf},
    process::ExitStatus,
};

use tokio::{
    io::{AsyncRead, AsyncReadExt},
    task::JoinHandle,
};

use crate::FilePeekerError;

const DIAGNOSTIC_LIMIT: usize = 64 * 1024;

pub(super) fn log_event(path: &Path, message: &str) {
    let Ok(mut file) = std::fs::OpenOptions::new().append(true).open(path) else {
        return;
    };
    let _ = writeln!(file, "[{message}]");
}

pub(super) fn append_log_bytes(path: &Path, label: &str, bytes: &[u8]) {
    if bytes.is_empty() {
        return;
    }
    let Ok(mut file) = std::fs::OpenOptions::new().append(true).open(path) else {
        return;
    };
    let _ = writeln!(file, "[{label}]");
    let _ = file.write_all(bytes);
    if !bytes.ends_with(b"\n") {
        let _ = writeln!(file);
    }
}

pub(super) async fn read(
    mut reader: impl AsyncRead + Unpin,
    log_path: PathBuf,
    label: &'static str,
) -> Result<Vec<u8>, std::io::Error> {
    let mut retained = Vec::new();
    let mut buffer = [0_u8; 8 * 1024];
    let mut log = std::fs::OpenOptions::new().append(true).open(log_path).ok();
    if let Some(log) = &mut log {
        let _ = writeln!(log, "[{label}]");
    }
    loop {
        let count = reader.read(&mut buffer).await?;
        if count == 0 {
            break;
        }
        if let Some(log) = &mut log {
            let _ = log.write_all(&buffer[..count]);
        }
        let remaining = DIAGNOSTIC_LIMIT.saturating_sub(retained.len());
        retained.extend_from_slice(&buffer[..count.min(remaining)]);
    }
    if !retained.ends_with(b"\n")
        && let Some(log) = &mut log
    {
        let _ = writeln!(log);
    }
    Ok(retained)
}

pub(super) async fn join(task: JoinHandle<Result<Vec<u8>, std::io::Error>>) -> Option<Vec<u8>> {
    task.await.ok().and_then(Result::ok)
}

pub(super) fn add_stderr_context(error: FilePeekerError, stderr: Option<&[u8]>) -> FilePeekerError {
    add_named_stderr_context(error, "server", stderr)
}

pub(super) fn add_named_stderr_context(
    error: FilePeekerError,
    source: &str,
    stderr: Option<&[u8]>,
) -> FilePeekerError {
    let Some(stderr) = stderr.filter(|output| !output.is_empty()) else {
        return error;
    };
    let suffix = format!(
        "; {source} stderr: {}",
        String::from_utf8_lossy(stderr).trim()
    );

    match error {
        FilePeekerError::ServerStart { mut message } => {
            message.push_str(&suffix);
            FilePeekerError::ServerStart { message }
        }
        FilePeekerError::ServerExited { mut message } => {
            message.push_str(&suffix);
            FilePeekerError::ServerExited { message }
        }
        other => other,
    }
}

pub(super) fn server_exited_error(status: ExitStatus, stderr: Option<&[u8]>) -> FilePeekerError {
    add_stderr_context(
        FilePeekerError::ServerExited {
            message: format!("server exited with {status}"),
        },
        stderr,
    )
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    #[tokio::test]
    async fn diagnostic_reader_retains_all_small_output() {
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let log_path = directory.path().join("session.log");
        std::fs::write(&log_path, []).expect("log should be created");
        let (mut writer, reader) = tokio::io::duplex(64);
        let task = tokio::spawn(super::read(reader, log_path.clone(), "test stderr"));
        writer
            .write_all(b"abcdefgh")
            .await
            .expect("fixture output should be written");
        drop(writer);

        let bytes = task
            .await
            .expect("reader task should finish")
            .expect("reader should succeed");
        assert_eq!(bytes, b"abcdefgh");

        let mut logged = Vec::new();
        tokio::fs::File::open(log_path)
            .await
            .expect("log should open")
            .read_to_end(&mut logged)
            .await
            .expect("log should be readable");
        assert!(logged.ends_with(b"abcdefgh\n"));
    }
}
