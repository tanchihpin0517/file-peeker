use futures_util::{StreamExt as _, stream, stream::BoxStream};
use tokio::fs;

use super::{DirectoryEntry, FsError, entry::inspect, resolve::real_path};
use crate::path_string::PathString;

/// One directory's entries, in the order the filesystem gives them. Ends after
/// the last entry, or after an error iterating the directory.
pub(crate) type EntryStream = BoxStream<'static, Result<DirectoryEntry, FsError>>;

/// Opens a directory for listing.
///
/// # Errors
///
/// When the path does not resolve or is not a directory this process may read;
/// later failures are the stream's last item.
pub(crate) async fn list_dir(path: &PathString) -> Result<EntryStream, FsError> {
    let path = real_path(path)?;
    let entries = fs::read_dir(&path)
        .await
        .map_err(|error| FsError::at("cannot list", &path, &error))?;
    Ok(stream::unfold(Some(entries), |entries| async move {
        let mut entries = entries?;
        match entries.next_entry().await {
            // `inspect` cannot fail: an entry it could not describe comes back
            // degraded, so one bad entry hides none of its siblings.
            Ok(Some(entry)) => Some((Ok(inspect(&entry).await.entry), Some(entries))),
            Ok(None) => None,
            Err(error) => Some((Err(FsError::from_io(&error)), None)),
        }
    })
    .boxed())
}
