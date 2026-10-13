use bytes::Bytes;
use futures_util::{StreamExt as _, stream::BoxStream};
use tokio::fs::File;
use tokio_util::io::ReaderStream;

use super::{FsError, FsErrorKind, absolute::absolute};
use crate::path_string::PathString;

/// A file's bytes from the start, in non-empty chunks whose sizes mean nothing.
/// An empty file is an empty stream; a failure part-way is the last item.
pub(crate) type ReadStream = BoxStream<'static, Result<Bytes, FsError>>;

/// Opens a regular file for reading.
///
/// # Errors
///
/// When the path is relative, cannot be opened, or is not a regular file.
pub(crate) async fn read_file(path: &PathString) -> Result<ReadStream, FsError> {
    let path = absolute(path)?;
    let file = File::open(&path)
        .await
        .map_err(|error| FsError::at("cannot open", &path, &error))?;
    let metadata = file
        .metadata()
        .await
        .map_err(|error| FsError::at("cannot inspect", &path, &error))?;
    if !metadata.is_file() {
        return Err(FsError::new(
            FsErrorKind::NotFile,
            format!("cannot read `{}`: not a regular file", path.display()),
        ));
    }
    Ok(ReaderStream::new(file)
        .map(|chunk| chunk.map_err(|error| FsError::from_io(&error)))
        .boxed())
}
