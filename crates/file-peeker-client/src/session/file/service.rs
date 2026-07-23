use std::{io, path::PathBuf, sync::Arc};

use super::{
    opener::{FileOpener, SystemFileOpener},
    stage::FileStager,
    with_context,
};
use crate::session::{SessionTarget, backend::ReadStream};

#[derive(Debug)]
pub(crate) struct FileService {
    stager: FileStager,
    opener: Arc<dyn FileOpener>,
}

impl FileService {
    pub(crate) async fn open(
        &self,
        target: &SessionTarget,
        session_id: &str,
        resolved_path: &str,
        stream: ReadStream,
    ) -> io::Result<()> {
        let local_path = match target {
            SessionTarget::Local => {
                drop(stream);
                PathBuf::from(resolved_path)
            }
            SessionTarget::Remote { .. } => self
                .stager
                .stage_download(session_id, resolved_path, stream)
                .await
                .map_err(|error| {
                    with_context(
                        &error,
                        format!("cannot stage remote file `{resolved_path}`"),
                    )
                })?,
        };

        self.opener.open(&local_path).await.map_err(|error| {
            with_context(
                &error,
                format!("cannot open file `{}`", local_path.display()),
            )
        })
    }
}

impl Default for FileService {
    fn default() -> Self {
        Self {
            stager: FileStager::default(),
            opener: Arc::new(SystemFileOpener),
        }
    }
}

#[cfg(test)]
impl FileService {
    pub(super) fn for_test(cache_root: PathBuf, opener: Arc<dyn FileOpener>) -> Self {
        Self {
            stager: FileStager::new(cache_root),
            opener,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{io, path::PathBuf, sync::Arc};

    use bytes::Bytes;
    use futures::{StreamExt as _, stream};

    use super::FileService;
    use crate::session::{
        SessionTarget,
        backend::ReadStream,
        file::opener::{FailingOpener, RecordingOpener},
    };

    fn chunks(items: Vec<io::Result<&'static [u8]>>) -> ReadStream {
        stream::iter(items.into_iter().map(|item| item.map(Bytes::from_static))).boxed()
    }

    #[tokio::test]
    async fn local_files_are_opened_without_copying() {
        let cache = tempfile::tempdir().unwrap();
        let opener = Arc::new(RecordingOpener::default());
        let service = FileService::for_test(cache.path().to_path_buf(), opener.clone());

        service
            .open(
                &SessionTarget::Local,
                "local",
                "/existing/report.txt",
                chunks(vec![Ok(b"unused")]),
            )
            .await
            .unwrap();

        assert_eq!(opener.paths(), vec![PathBuf::from("/existing/report.txt")]);
        assert!(std::fs::read_dir(cache.path()).unwrap().next().is_none());
    }

    #[tokio::test]
    async fn completed_remote_files_survive_file_service_destruction() {
        let cache = tempfile::tempdir().unwrap();
        let opener = Arc::new(RecordingOpener::default());
        let service = FileService::for_test(cache.path().to_path_buf(), opener.clone());

        service
            .open(
                &SessionTarget::Remote {
                    destination: "fixture".into(),
                },
                "remote",
                "/remote/report.txt",
                chunks(vec![Ok(b"first "), Ok(b"second")]),
            )
            .await
            .unwrap();

        let path = opener.paths()[0].clone();
        assert_eq!(path.file_name().unwrap(), "report.txt");
        drop(service);
        assert_eq!(tokio::fs::read(path).await.unwrap(), b"first second");
    }

    #[tokio::test]
    async fn stream_failures_do_not_invoke_the_opener() {
        let cache = tempfile::tempdir().unwrap();
        let opener = Arc::new(RecordingOpener::default());
        let service = FileService::for_test(cache.path().to_path_buf(), opener.clone());

        let error = service
            .open(
                &SessionTarget::Remote {
                    destination: "fixture".into(),
                },
                "remote-stream-error",
                "/remote/broken.txt",
                chunks(vec![
                    Ok(b"prefix"),
                    Err(io::Error::new(
                        io::ErrorKind::ConnectionReset,
                        "fixture stream reset",
                    )),
                ]),
            )
            .await
            .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::ConnectionReset);
        assert!(opener.paths().is_empty());
    }

    #[tokio::test]
    async fn opener_failures_leave_completed_remote_files_staged() {
        let cache = tempfile::tempdir().unwrap();
        let service = FileService::for_test(cache.path().to_path_buf(), Arc::new(FailingOpener));

        let error = service
            .open(
                &SessionTarget::Remote {
                    destination: "fixture".into(),
                },
                "remote-failing-opener",
                "/remote/report.txt",
                chunks(vec![Ok(b"complete")]),
            )
            .await
            .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(error.to_string().contains("fixture opener denied"));
        let staged = std::fs::read_dir(cache.path().join("remote-failing-opener"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path()
            .join("report.txt");
        assert_eq!(tokio::fs::read(staged).await.unwrap(), b"complete");
    }
}
