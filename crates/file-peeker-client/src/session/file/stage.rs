use std::{
    ffi::OsStr,
    io,
    path::{Path, PathBuf},
};

use futures::StreamExt as _;
use tokio::{
    fs::{self, OpenOptions},
    io::AsyncWriteExt as _,
};
use uuid::Uuid;

use super::with_context;
use crate::session::backend::ReadStream;

#[derive(Debug)]
pub(crate) struct FileStager {
    root: PathBuf,
}

impl FileStager {
    /// Creates a stager whose root is owned exclusively by File Peeker.
    ///
    /// Staging creates the root lazily and restricts it to the current user on
    /// Unix.
    pub(crate) fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub(crate) async fn stage_download(
        &self,
        session_id: &str,
        source_path: &str,
        mut stream: ReadStream,
    ) -> io::Result<PathBuf> {
        if !self.root.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("cache root `{}` is not absolute", self.root.display()),
            ));
        }
        let basename = usable_basename(source_path)?;
        ensure_private_directory(&self.root).await?;
        let session_dir = self.root.join(session_id);
        ensure_private_directory(&session_dir).await?;
        let destination_dir = session_dir.join(Uuid::new_v4().to_string());
        ensure_private_directory(&destination_dir).await?;

        let final_path = destination_dir.join(basename);
        let partial_path = destination_dir.join(format!(
            ".{}.{}.partial",
            basename.to_string_lossy(),
            Uuid::new_v4()
        ));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        set_private_mode(&mut options);
        let mut file = options.open(&partial_path).await.map_err(|error| {
            with_context(
                &error,
                format!("cannot create partial file `{}`", partial_path.display()),
            )
        })?;
        let mut partial = PartialFileGuard::new(partial_path.clone());

        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|error| {
                with_context(
                    &error,
                    format!("cannot download `{source_path}` into the application cache"),
                )
            })?;
            file.write_all(&chunk).await.map_err(|error| {
                with_context(
                    &error,
                    format!("cannot write partial file `{}`", partial_path.display()),
                )
            })?;
        }
        file.flush().await.map_err(|error| {
            with_context(
                &error,
                format!("cannot flush partial file `{}`", partial_path.display()),
            )
        })?;
        drop(file);

        fs::rename(&partial_path, &final_path)
            .await
            .map_err(|error| {
                with_context(
                    &error,
                    format!("cannot publish downloaded file `{}`", final_path.display()),
                )
            })?;
        partial.disarm();
        Ok(final_path)
    }
}

impl Default for FileStager {
    fn default() -> Self {
        Self::new(default_cache_root())
    }
}

fn default_cache_root() -> PathBuf {
    cache_root_from(dirs::cache_dir(), &std::env::temp_dir())
}

fn cache_root_from(platform_cache_base: Option<PathBuf>, temp_base: &Path) -> PathBuf {
    match platform_cache_base.filter(|path| path.is_absolute()) {
        Some(base) => base.join(application_cache_namespace()).join("open-files"),
        None => temp_base.join("file-peeker").join("open-files"),
    }
}

fn application_cache_namespace() -> &'static str {
    if cfg!(target_os = "macos") {
        "FilePeeker"
    } else {
        "file-peeker"
    }
}

async fn ensure_private_directory(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path).await.map_err(|error| {
        with_context(
            &error,
            format!("cannot create cache directory `{}`", path.display()),
        )
    })?;
    set_private_directory_mode(path).await
}

#[cfg(unix)]
async fn set_private_directory_mode(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .await
        .map_err(|error| {
            with_context(
                &error,
                format!(
                    "cannot set private permissions on cache directory `{}`",
                    path.display()
                ),
            )
        })
}

#[cfg(not(unix))]
async fn set_private_directory_mode(_path: &Path) -> io::Result<()> {
    Ok(())
}

fn usable_basename(source_path: &str) -> io::Result<&OsStr> {
    Path::new(source_path)
        .file_name()
        .filter(|name| !name.is_empty() && *name != OsStr::new(".") && *name != OsStr::new(".."))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("source path `{source_path}` has no usable filename"),
            )
        })
}

#[cfg(unix)]
fn set_private_mode(options: &mut OpenOptions) {
    options.mode(0o600);
}

#[cfg(not(unix))]
fn set_private_mode(_options: &mut OpenOptions) {}

#[derive(Debug)]
struct PartialFileGuard {
    path: Option<PathBuf>,
}

impl PartialFileGuard {
    fn new(path: PathBuf) -> Self {
        Self { path: Some(path) }
    }

    fn disarm(&mut self) {
        self.path = None;
    }
}

impl Drop for PartialFileGuard {
    fn drop(&mut self) {
        if let Some(path) = self.path.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        io,
        path::{Path, PathBuf},
        sync::Arc,
    };

    use bytes::Bytes;
    use futures::stream;
    use tokio::sync::Notify;

    use super::{FileStager, application_cache_namespace, cache_root_from};

    fn chunks(items: Vec<io::Result<&'static [u8]>>) -> crate::session::backend::ReadStream {
        stream::iter(items.into_iter().map(|item| item.map(Bytes::from_static))).boxed()
    }

    use futures::StreamExt as _;

    #[tokio::test]
    async fn stages_complete_files_and_preserves_the_basename() {
        let fixture = tempfile::tempdir().unwrap();
        let stager = FileStager::new(fixture.path().to_path_buf());

        let path = stager
            .stage_download(
                "session",
                "/remote/reports/example.data",
                chunks(vec![Ok(b"first "), Ok(b"second")]),
            )
            .await
            .unwrap();

        assert_eq!(path.file_name().unwrap(), "example.data");
        assert_eq!(tokio::fs::read(path).await.unwrap(), b"first second");
    }

    #[tokio::test]
    async fn creates_a_missing_private_cache_hierarchy() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("missing/cache/root");
        let stager = FileStager::new(root.clone());

        let path = stager
            .stage_download(
                "session",
                "/remote/reports/example.data",
                chunks(vec![Ok(b"private")]),
            )
            .await
            .unwrap();

        let destination = path.parent().unwrap();
        assert_eq!(destination.parent().unwrap(), root.join("session"));
        assert_eq!(path.file_name().unwrap(), "example.data");
        assert_eq!(tokio::fs::read(&path).await.unwrap(), b"private");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;

            let mode = |path: &std::path::Path| {
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777
            };
            assert_eq!(mode(&root), 0o700);
            assert_eq!(mode(&root.join("session")), 0o700);
            assert_eq!(mode(destination), 0o700);
            assert_eq!(mode(&path), 0o600);
        }
    }

    #[test]
    fn absolute_platform_cache_bases_use_the_platform_namespace() {
        let base = PathBuf::from("/platform/cache");

        assert_eq!(
            cache_root_from(Some(base.clone()), Path::new("/temporary")),
            base.join(application_cache_namespace()).join("open-files")
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_preserves_the_capitalized_cache_namespace() {
        assert_eq!(
            cache_root_from(
                Some(PathBuf::from("/Users/fixture/Library/Caches")),
                Path::new("/temporary")
            ),
            PathBuf::from("/Users/fixture/Library/Caches/FilePeeker/open-files")
        );
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn non_macos_platforms_use_the_lowercase_cache_namespace() {
        assert_eq!(
            cache_root_from(
                Some(PathBuf::from("/home/fixture/.cache")),
                Path::new("/temporary")
            ),
            PathBuf::from("/home/fixture/.cache/file-peeker/open-files")
        );
    }

    #[test]
    fn missing_platform_cache_bases_use_the_temp_fallback() {
        assert_eq!(
            cache_root_from(None, Path::new("/temporary")),
            PathBuf::from("/temporary/file-peeker/open-files")
        );
    }

    #[test]
    fn relative_platform_cache_bases_use_the_temp_fallback() {
        assert_eq!(
            cache_root_from(
                Some(PathBuf::from("relative-cache")),
                Path::new("/temporary")
            ),
            PathBuf::from("/temporary/file-peeker/open-files")
        );
    }

    #[tokio::test]
    async fn stages_empty_files_and_avoids_basename_collisions() {
        let fixture = tempfile::tempdir().unwrap();
        let stager = FileStager::new(fixture.path().to_path_buf());

        let first = stager
            .stage_download("session", "/one/same.txt", chunks(vec![]))
            .await
            .unwrap();
        let second = stager
            .stage_download("session", "/two/same.txt", chunks(vec![Ok(b"two")]))
            .await
            .unwrap();

        assert_ne!(first, second);
        assert!(tokio::fs::read(first).await.unwrap().is_empty());
        assert_eq!(tokio::fs::read(second).await.unwrap(), b"two");
    }

    #[tokio::test]
    async fn removes_partial_file_after_a_terminal_stream_error() {
        let fixture = tempfile::tempdir().unwrap();
        let stager = FileStager::new(fixture.path().to_path_buf());

        let error = stager
            .stage_download(
                "session",
                "/remote/broken.txt",
                chunks(vec![Ok(b"prefix"), Err(io::Error::other("disconnected"))]),
            )
            .await
            .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::Other);
        let download_dir = std::fs::read_dir(fixture.path().join("session"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        assert!(std::fs::read_dir(download_dir).unwrap().next().is_none());
    }

    #[tokio::test]
    async fn rejects_paths_without_a_filename() {
        let fixture = tempfile::tempdir().unwrap();
        let error = FileStager::new(fixture.path().to_path_buf())
            .stage_download("session", "/", chunks(vec![]))
            .await
            .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(std::fs::read_dir(fixture.path()).unwrap().next().is_none());
    }

    #[tokio::test]
    async fn rename_failures_remove_the_partial_file() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().to_path_buf();
        let stream_root = root.clone();
        let stream = stream::once(async move {
            let destination = std::fs::read_dir(stream_root.join("session"))
                .unwrap()
                .next()
                .unwrap()
                .unwrap()
                .path();
            std::fs::create_dir(destination.join("blocked.txt")).unwrap();
            Ok(Bytes::from_static(b"payload"))
        })
        .boxed();

        let error = FileStager::new(root.clone())
            .stage_download("session", "/remote/blocked.txt", stream)
            .await
            .unwrap_err();

        assert!(error.to_string().contains("cannot publish downloaded file"));
        let destination = std::fs::read_dir(root.join("session"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let entries = std::fs::read_dir(destination)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].is_dir());
    }

    #[tokio::test]
    async fn aborting_an_in_progress_download_removes_the_partial_file() {
        let fixture = tempfile::tempdir().unwrap();
        let blocked = Arc::new(Notify::new());
        let stream_blocked = blocked.clone();
        let stream = stream::unfold(0_u8, move |state| {
            let blocked = stream_blocked.clone();
            async move {
                match state {
                    0 => Some((Ok(Bytes::from_static(b"prefix")), 1)),
                    1 => {
                        blocked.notify_one();
                        std::future::pending().await
                    }
                    _ => None,
                }
            }
        })
        .boxed();
        let root = fixture.path().to_path_buf();
        let operation = tokio::spawn(async move {
            FileStager::new(root)
                .stage_download("session", "/remote/pending.txt", stream)
                .await
        });
        blocked.notified().await;

        operation.abort();
        assert!(operation.await.unwrap_err().is_cancelled());

        let destination = std::fs::read_dir(fixture.path().join("session"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        assert!(std::fs::read_dir(destination).unwrap().next().is_none());
    }

    #[tokio::test]
    async fn rejects_relative_cache_roots() {
        let error = FileStager::new("relative-cache".into())
            .stage_download("session", "/remote/file.txt", chunks(vec![]))
            .await
            .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }
}
