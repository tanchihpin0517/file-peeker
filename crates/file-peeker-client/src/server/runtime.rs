use std::{
    fs::{self, OpenOptions},
    io::ErrorKind,
    os::unix::ffi::OsStrExt as _,
    os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt},
    path::{Path, PathBuf},
};

use uuid::Uuid;

use crate::FilePeekerError;

const MAX_SOCKET_PATH_BYTES: usize = 100;
const RETAINED_LOGS: usize = 10;

#[derive(Debug)]
pub(super) struct SessionDirectory {
    path: PathBuf,
    id: String,
    log_path: PathBuf,
}

impl SessionDirectory {
    pub(super) fn create() -> Result<Self, FilePeekerError> {
        Self::create_in(&home_directory()?)
    }

    fn create_in(home: &Path) -> Result<Self, FilePeekerError> {
        let home_metadata =
            fs::symlink_metadata(home).map_err(|error| FilePeekerError::ServerStart {
                message: format!(
                    "cannot inspect home directory `{}`: {error}",
                    home.display()
                ),
            })?;
        if !home_metadata.is_dir() || home_metadata.file_type().is_symlink() {
            return Err(FilePeekerError::ServerStart {
                message: format!("home path `{}` is not a real directory", home.display()),
            });
        }
        let owner = home_metadata.uid();
        let file_peeker = home.join(".file-peeker");
        ensure_owned_directory(&file_peeker, owner, None)?;
        let run_root = file_peeker.join("run");
        ensure_owned_directory(&run_root, owner, Some(0o700))?;
        let logs_root = file_peeker.join("logs");
        ensure_owned_directory(&logs_root, owner, Some(0o700))?;

        let id = Uuid::new_v4().simple().to_string();
        let path = run_root.join(&id);
        fs::create_dir(&path).map_err(|error| FilePeekerError::ServerStart {
            message: format!(
                "cannot create session directory `{}`: {error}",
                path.display()
            ),
        })?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).map_err(|error| {
            FilePeekerError::ServerStart {
                message: format!(
                    "cannot secure session directory `{}`: {error}",
                    path.display()
                ),
            }
        })?;

        let log_path = logs_root.join(format!("{id}.log"));
        OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&log_path)
            .map_err(|error| FilePeekerError::ServerStart {
                message: format!(
                    "cannot create session log `{}`: {error}",
                    log_path.display()
                ),
            })?;
        rotate_logs(&logs_root, &log_path);

        let endpoint = Self { path, id, log_path };
        validate_socket_path(&endpoint.socket_path())?;
        Ok(endpoint)
    }

    pub(super) fn id(&self) -> &str {
        &self.id
    }

    pub(super) fn log_path(&self) -> &Path {
        &self.log_path
    }

    pub(super) fn socket_path(&self) -> PathBuf {
        self.path.join("server.sock")
    }

    pub(super) fn control_socket_path(&self) -> PathBuf {
        self.path.join("cm.sock")
    }
}

impl Drop for SessionDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[cfg(test)]
#[allow(clippy::unnecessary_wraps)]
fn home_directory() -> Result<PathBuf, FilePeekerError> {
    use std::sync::OnceLock;
    static TEST_HOME: OnceLock<PathBuf> = OnceLock::new();
    let path = TEST_HOME.get_or_init(|| {
        let id = Uuid::new_v4().simple().to_string();
        let path = Path::new("/tmp").join(format!("fpt-{}", &id[..8]));
        fs::create_dir(&path).expect("test home should be created");
        path
    });
    Ok(path.clone())
}

#[cfg(not(test))]
fn home_directory() -> Result<PathBuf, FilePeekerError> {
    let home = std::env::var_os("HOME").ok_or_else(|| FilePeekerError::ServerStart {
        message: "HOME is required to create file-peeker runtime files".into(),
    })?;
    let path = PathBuf::from(home);
    if !path.is_absolute() {
        return Err(FilePeekerError::ServerStart {
            message: "HOME must be an absolute path".into(),
        });
    }
    Ok(path)
}

fn ensure_owned_directory(
    path: &Path,
    expected_owner: u32,
    mode: Option<u32>,
) -> Result<(), FilePeekerError> {
    loop {
        match fs::symlink_metadata(path) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.is_dir() {
                    return Err(FilePeekerError::ServerStart {
                        message: format!(
                            "runtime path `{}` is not a real directory",
                            path.display()
                        ),
                    });
                }
                if metadata.uid() != expected_owner {
                    return Err(FilePeekerError::ServerStart {
                        message: format!(
                            "runtime path `{}` has unexpected ownership",
                            path.display()
                        ),
                    });
                }
                break;
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {
                if let Err(error) = fs::create_dir(path)
                    && error.kind() != ErrorKind::AlreadyExists
                {
                    return Err(FilePeekerError::ServerStart {
                        message: format!(
                            "cannot create runtime directory `{}`: {error}",
                            path.display()
                        ),
                    });
                }
            }
            Err(error) => {
                return Err(FilePeekerError::ServerStart {
                    message: format!(
                        "cannot inspect runtime directory `{}`: {error}",
                        path.display()
                    ),
                });
            }
        }
    }
    if let Some(mode) = mode {
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).map_err(|error| {
            FilePeekerError::ServerStart {
                message: format!(
                    "cannot secure runtime directory `{}`: {error}",
                    path.display()
                ),
            }
        })?;
    }
    Ok(())
}

fn rotate_logs(logs_root: &Path, current: &Path) {
    let Ok(entries) = fs::read_dir(logs_root) else {
        return;
    };
    let mut logs = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.path() != current)
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            entry.file_type().is_ok_and(|kind| kind.is_file())
                && name.len() == 36
                && name.ends_with(".log")
                && name.as_bytes()[..32].iter().all(u8::is_ascii_hexdigit)
        })
        .filter_map(|entry| {
            let modified = entry.metadata().ok()?.modified().ok()?;
            Some((modified, entry.path()))
        })
        .collect::<Vec<_>>();
    logs.sort_by_key(|(modified, _)| *modified);
    let remove_count = logs.len().saturating_sub(RETAINED_LOGS - 1);
    for (_, path) in logs.into_iter().take(remove_count) {
        let _ = fs::remove_file(path);
    }
}

pub(super) fn validate_socket_path(path: &Path) -> Result<(), FilePeekerError> {
    let length = path.as_os_str().as_bytes().len();
    if length > MAX_SOCKET_PATH_BYTES {
        return Err(FilePeekerError::ServerStart {
            message: format!(
                "Unix socket path `{}` is {length} bytes; maximum supported length is {MAX_SOCKET_PATH_BYTES}",
                path.display()
            ),
        });
    }
    if path.as_os_str().as_bytes().contains(&b':') {
        return Err(FilePeekerError::ServerStart {
            message: format!(
                "Unix socket path `{}` contains `:`, which SSH StreamLocal forwarding cannot encode",
                path.display()
            ),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        os::unix::fs::{PermissionsExt as _, symlink},
        path::Path,
    };

    use tempfile::Builder;

    use super::{SessionDirectory, validate_socket_path};

    #[test]
    fn session_files_live_under_the_file_peeker_home() {
        let home = Builder::new()
            .prefix("fph-")
            .tempdir_in("/tmp")
            .expect("short test home should be created");
        let endpoint =
            SessionDirectory::create_in(home.path()).expect("endpoint should be created");
        let session_path = endpoint.path.clone();

        assert!(
            endpoint
                .socket_path()
                .starts_with(home.path().join(".file-peeker/run"))
        );
        assert_eq!(
            fs::metadata(&session_path)
                .expect("session metadata should exist")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&endpoint.log_path)
                .expect("log metadata should exist")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );

        drop(endpoint);
        assert!(!session_path.exists());
    }

    #[test]
    fn rejects_a_symlinked_runtime_root() {
        let home = Builder::new()
            .prefix("fph-")
            .tempdir_in("/tmp")
            .expect("short test home should be created");
        let file_peeker = home.path().join(".file-peeker");
        fs::create_dir(&file_peeker).expect("file-peeker root should be created");
        symlink("/tmp", file_peeker.join("run")).expect("runtime symlink should be created");

        assert!(SessionDirectory::create_in(home.path()).is_err());
    }

    #[test]
    fn rejects_socket_paths_that_are_too_long() {
        let long = Path::new("/tmp").join("x".repeat(super::MAX_SOCKET_PATH_BYTES));
        assert!(validate_socket_path(&long).is_err());
    }
}
