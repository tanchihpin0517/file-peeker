use std::path::Path;

use tokio::process::Command;

use crate::{FilePeekerError, SessionTarget};

const SYSTEM_OPENER: &str = "/usr/bin/open";

pub(crate) async fn open(target: &SessionTarget, path: String) -> Result<(), FilePeekerError> {
    match target {
        SessionTarget::Local => open_local(Path::new(SYSTEM_OPENER), path).await,
        SessionTarget::Remote { .. } => Ok(()),
    }
}

async fn open_local(opener: &Path, path: String) -> Result<(), FilePeekerError> {
    let status = Command::new(opener)
        .arg("--")
        .arg(&path)
        .status()
        .await
        .map_err(|error| FilePeekerError::Io {
            message: format!("cannot open `{path}` with the system application: {error}"),
        })?;

    if status.success() {
        Ok(())
    } else {
        Err(FilePeekerError::Io {
            message: format!("system application failed to open `{path}`: {status}"),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        path::{Path, PathBuf},
    };

    use super::{open, open_local};
    use crate::{FilePeekerError, SessionTarget};

    #[tokio::test]
    async fn ssh_open_is_a_successful_no_op() {
        open(
            &SessionTarget::Remote {
                destination: "example.com".into(),
            },
            "/remote/report.txt".into(),
        )
        .await
        .expect("SSH open should succeed without launching a process");
    }

    #[tokio::test]
    async fn local_open_passes_option_separator_and_path_verbatim() {
        let fixture = tempfile::tempdir().expect("fixture should be created");
        let record = fixture.path().join("arguments");
        let opener = fixture.path().join("opener");
        write_executable(
            &opener,
            &format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > {}\n",
                shell_quote(&record.to_string_lossy())
            ),
        );
        let path = "-report with spaces.txt";

        open_local(&opener, path.into())
            .await
            .expect("fixture opener should succeed");

        assert_eq!(
            fs::read_to_string(record).expect("arguments should be recorded"),
            format!("--\n{path}\n")
        );
    }

    #[tokio::test]
    async fn local_open_maps_spawn_failure_to_io() {
        let error = open_local(
            Path::new("/definitely/missing/file-peeker-opener"),
            "file.txt".into(),
        )
        .await
        .expect_err("missing opener should fail");

        assert!(matches!(error, FilePeekerError::Io { .. }));
    }

    #[tokio::test]
    async fn local_open_maps_unsuccessful_exit_to_io() {
        let error = open_local(Path::new("/usr/bin/false"), "file.txt".into())
            .await
            .expect_err("unsuccessful opener should fail");

        assert!(matches!(error, FilePeekerError::Io { .. }));
    }

    fn write_executable(path: &PathBuf, contents: &str) {
        fs::write(path, contents).expect("fixture executable should be written");
        let mut permissions = fs::metadata(path)
            .expect("fixture executable metadata should exist")
            .permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(path, permissions).expect("fixture should be executable");
    }

    fn shell_quote(value: &str) -> String {
        format!("'{}'", value.replace('\'', "'\"'\"'"))
    }
}
