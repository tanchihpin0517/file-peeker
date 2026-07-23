use std::{fmt::Debug, io, path::Path};

use async_trait::async_trait;

#[async_trait]
pub(super) trait FileOpener: Debug + Send + Sync {
    async fn open(&self, path: &Path) -> io::Result<()>;
}

#[derive(Debug)]
pub(super) struct SystemFileOpener;

#[async_trait]
impl FileOpener for SystemFileOpener {
    async fn open(&self, path: &Path) -> io::Result<()> {
        #[cfg(target_os = "macos")]
        {
            let status = tokio::process::Command::new("/usr/bin/open")
                .arg(path)
                .status()
                .await?;
            if !status.success() {
                return Err(io::Error::other(format!(
                    "/usr/bin/open exited unsuccessfully with {status}"
                )));
            }
            Ok(())
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = path;
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "opening files is supported only on macOS",
            ))
        }
    }
}

#[cfg(test)]
#[derive(Debug, Default)]
pub(super) struct RecordingOpener {
    paths: std::sync::Mutex<Vec<std::path::PathBuf>>,
}

#[cfg(test)]
impl RecordingOpener {
    pub(super) fn paths(&self) -> Vec<std::path::PathBuf> {
        self.paths.lock().unwrap().clone()
    }
}

#[cfg(test)]
#[async_trait]
impl FileOpener for RecordingOpener {
    async fn open(&self, path: &Path) -> io::Result<()> {
        self.paths.lock().unwrap().push(path.to_path_buf());
        Ok(())
    }
}

#[cfg(test)]
#[derive(Debug)]
pub(super) struct FailingOpener;

#[cfg(test)]
#[async_trait]
impl FileOpener for FailingOpener {
    async fn open(&self, _path: &Path) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "fixture opener denied the request",
        ))
    }
}
