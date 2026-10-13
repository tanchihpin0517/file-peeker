use std::path::PathBuf;

use futures_util::{StreamExt as _, stream, stream::BoxStream};
use tokio::fs;

use super::{DirectoryEntry, FsError, absolute::absolute, entry::inspect};
use crate::path_string::PathString;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct WalkEntry {
    /// From the walk's root, escaped as names are.
    pub relative_path: PathString,
    pub entry: DirectoryEntry,
    /// The root's children are 1.
    pub depth: usize,
}

/// Everything below a directory, pre-order depth first. Ends after the last
/// entry, or after the first error opening or iterating a directory.
pub(crate) type WalkStream = BoxStream<'static, Result<WalkEntry, FsError>>;

struct Walk {
    stack: Vec<Frame>,
    /// The directory just emitted, opened before the next entry is read: a
    /// descendant's failure follows the entry it belongs to.
    descend_into: Option<(PathBuf, PathString, usize)>,
}

struct Frame {
    entries: fs::ReadDir,
    relative_dir: PathString,
    child_depth: usize,
}

/// Opens a directory for walking. The root is not emitted, siblings come in the
/// filesystem's order, and a symlink is emitted but never followed.
///
/// # Errors
///
/// When the root is relative or is not a directory this process may read;
/// later failures are the stream's last item.
pub(crate) async fn walk_dir(path: &PathString) -> Result<WalkStream, FsError> {
    let root = absolute(path)?;
    let walk = Walk {
        stack: vec![Frame {
            entries: open(&root).await?,
            relative_dir: PathString::from_literal(""),
            child_depth: 1,
        }],
        descend_into: None,
    };
    Ok(stream::unfold(Some(walk), |walk| async move {
        let mut walk = walk?;
        if let Some((path, relative_dir, child_depth)) = walk.descend_into.take() {
            match open(&path).await {
                Ok(entries) => walk.stack.push(Frame {
                    entries,
                    relative_dir,
                    child_depth,
                }),
                Err(error) => return Some((Err(error), None)),
            }
        }
        loop {
            let frame = walk.stack.last_mut()?;
            let entry = match frame.entries.next_entry().await {
                Ok(Some(entry)) => entry,
                Ok(None) => {
                    walk.stack.pop();
                    continue;
                }
                Err(error) => return Some((Err(FsError::from_io(&error)), None)),
            };
            // Infallible: an entry it could not describe is emitted degraded, and
            // a directory whose stat was refused is not gone into.
            let inspected = inspect(&entry).await;
            // Joining escaped halves is escaping the joined path, so a name that
            // is not UTF-8 is walked through like any other.
            let relative_path = frame.relative_dir.join(&inspected.entry.name);
            let depth = frame.child_depth;
            if inspected.descend {
                walk.descend_into = Some((inspected.path, relative_path.clone(), depth + 1));
            }
            let item = WalkEntry {
                relative_path,
                entry: inspected.entry,
                depth,
            };
            return Some((Ok(item), Some(walk)));
        }
    })
    .boxed())
}

async fn open(path: &std::path::Path) -> Result<fs::ReadDir, FsError> {
    fs::read_dir(path)
        .await
        .map_err(|error| FsError::at("cannot walk", path, &error))
}
