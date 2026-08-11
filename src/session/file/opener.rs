use std::{fmt::Debug, io, path::Path};

use async_trait::async_trait;

#[async_trait]
pub(crate) trait FileOpener: Debug + Send + Sync {
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
