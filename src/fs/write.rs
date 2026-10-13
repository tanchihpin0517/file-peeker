use std::{
    ffi::{OsStr, OsString},
    io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use futures_util::StreamExt as _;
use tokio::{
    fs::{self, File, OpenOptions},
    io::AsyncWriteExt as _,
};

use super::{FsError, FsErrorKind, ReadStream, absolute::absolute};
use crate::path_string::PathString;

static NEXT_PARTIAL: AtomicU64 = AtomicU64::new(1);

/// What a write does when its destination already exists.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WriteConflict {
    /// Fails, and leaves the existing file as it was — even one that appears
    /// while the write is under way.
    FailIfExists,
    Replace,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Written {
    pub path: PathString,
    pub bytes: u64,
}

/// Writes `source` into a new file `name` in `directory`, published whole or not
/// at all.
///
/// The bytes go to a hidden partial file beside the destination, which is
/// renamed or linked into place only once the source has ended. A failure, or
/// the future being dropped, removes the partial file before it returns or
/// drops: nothing half-written is ever left under either name.
///
/// # Errors
///
/// For a name that is not one name, a destination that is not a directory, a
/// conflict, a failing source, or a failing write.
pub(crate) async fn write_file(
    directory: &PathString,
    name: &PathString,
    conflict: WriteConflict,
    mut source: ReadStream,
) -> Result<Written, FsError> {
    if !name.is_name() {
        return Err(FsError::new(
            FsErrorKind::InvalidArgument,
            format!(
                "`{}` is not a file name: it must be one component, neither `.` nor `..`",
                name.display()
            ),
        ));
    }
    let directory = absolute(directory)?;
    let name = name.to_path_buf()?.into_os_string();
    let destination = directory.join(&name);

    let metadata = fs::metadata(&directory)
        .await
        .map_err(|error| FsError::at("cannot inspect destination directory", &directory, &error))?;
    if !metadata.is_dir() {
        return Err(FsError::new(
            FsErrorKind::NotDirectory,
            format!("destination `{}` is not a directory", directory.display()),
        ));
    }
    if conflict == WriteConflict::FailIfExists {
        match fs::symlink_metadata(&destination).await {
            Ok(_) => return Err(already_exists(&destination)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(FsError::at(
                    "cannot inspect destination",
                    &destination,
                    &error,
                ));
            }
        }
    }

    let (mut file, partial) = create_partial(&directory, &name).await?;
    let mut bytes = 0_u64;
    while let Some(chunk) = source.next().await {
        let chunk = chunk?;
        file.write_all(&chunk)
            .await
            .map_err(|error| FsError::at("cannot write", partial.path(), &error))?;
        bytes += chunk.len() as u64;
    }
    file.flush()
        .await
        .map_err(|error| FsError::at("cannot flush", partial.path(), &error))?;
    drop(file);

    match conflict {
        WriteConflict::Replace => {
            fs::rename(partial.path(), &destination)
                .await
                .map_err(|error| FsError::at("cannot publish", &destination, &error))?;
            partial.published();
        }
        // A link fails rather than replace a file that appeared since the check
        // above; the partial name is then removed as the guard drops.
        WriteConflict::FailIfExists => {
            fs::hard_link(partial.path(), &destination)
                .await
                .map_err(|error| {
                    if error.kind() == io::ErrorKind::AlreadyExists {
                        already_exists(&destination)
                    } else {
                        FsError::at("cannot publish", &destination, &error)
                    }
                })?;
        }
    }
    Ok(Written {
        path: PathString::from_path(&destination),
        bytes,
    })
}

/// The partial file, removed when this drops unless it was renamed into place.
///
/// Removed synchronously: an unlink is one quick syscall, and doing it here
/// rather than on a spawned task means the file is gone by the time a dropped
/// write's task reports itself cancelled.
struct Partial {
    path: PathBuf,
    published: bool,
}

impl Partial {
    fn path(&self) -> &Path {
        &self.path
    }

    fn published(mut self) {
        self.published = true;
    }
}

impl Drop for Partial {
    fn drop(&mut self) {
        if !self.published {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

async fn create_partial(directory: &Path, name: &OsStr) -> Result<(File, Partial), FsError> {
    for _ in 0..128 {
        let id = NEXT_PARTIAL.fetch_add(1, Ordering::Relaxed);
        // Built as an `OsString`, so a name held as bytes keeps them here too.
        let mut partial_name = OsString::from(".");
        partial_name.push(name);
        partial_name.push(format!(".fpk-{}-{id}.partial", std::process::id()));
        let path = directory.join(partial_name);
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .await
        {
            Ok(file) => {
                let partial = Partial {
                    path,
                    published: false,
                };
                return Ok((file, partial));
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(FsError::at("cannot create", &path, &error)),
        }
    }
    Err(FsError::new(
        FsErrorKind::Internal,
        format!(
            "cannot find a free partial file name in `{}`",
            directory.display()
        ),
    ))
}

fn already_exists(path: &Path) -> FsError {
    FsError::new(
        FsErrorKind::AlreadyExists,
        format!("`{}` already exists", path.display()),
    )
}
