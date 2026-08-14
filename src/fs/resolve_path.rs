use std::path::{Component, Path, PathBuf};

use crate::{
    fs::{FsError, FsErrorKind},
    host_path::HostPath,
};

/// Turns an escaped selected-host path into the real path it names.
///
/// This is the single place a path leaves its escaped form: `list_dir`, `read_file`,
/// `write_file` and `walk_dir` all resolve before they touch the filesystem, so
/// unescaping here covers every one of them.
///
/// `shellexpand::path` works over the raw OS bytes rather than a `str` — it converts
/// the input with `RawOsStr::new` and accumulates into an `OsString` — so `~` and
/// `$VARIABLE` expansion survives a path that is not UTF-8.
pub(crate) fn resolve_path(path: &HostPath) -> Result<PathBuf, FsError> {
    let path = path.to_path_buf()?;
    let expanded = shellexpand::path::full(&path)
        .map(std::borrow::Cow::into_owned)
        .map_err(|error| FsError::new(FsErrorKind::InvalidArgument, error.to_string()))?;
    let absolute = if expanded.is_absolute() {
        expanded
    } else {
        std::env::current_dir()
            .map_err(|error| {
                FsError::new(
                    FsErrorKind::Internal,
                    format!("cannot read current directory: {error}"),
                )
            })?
            .join(expanded)
    };
    Ok(normalize_absolute(&absolute))
}

fn normalize_absolute(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(component) => normalized.push(component),
        }
    }
    normalized
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::resolve_path;
    use crate::{fs::FsErrorKind, host_path::HostPath};

    /// Resolves a path written the way a person would write it.
    fn resolve(path: &str) -> Result<std::path::PathBuf, crate::fs::FsError> {
        resolve_path(&HostPath::from_literal(path))
    }

    #[test]
    fn resolves_dot_to_the_absolute_working_directory() {
        assert_eq!(resolve(".").unwrap(), std::env::current_dir().unwrap());
    }

    #[test]
    fn expands_tilde_and_environment_variables() {
        assert!(resolve("~").unwrap().is_absolute());
        assert!(resolve("$PATH").unwrap().is_absolute());
    }

    #[test]
    fn normalizes_relative_components_without_requiring_existence() {
        let expected = std::env::current_dir().unwrap().join("missing-child");
        assert_eq!(
            resolve("missing-parent/../missing-child/.").unwrap(),
            expected
        );
    }

    #[test]
    fn resolved_paths_are_idempotent() {
        let resolved = std::env::current_dir().unwrap().join("already-resolved");
        assert_eq!(
            resolve_path(&HostPath::from_path(&resolved)).unwrap(),
            resolved
        );
    }

    #[test]
    fn does_not_walk_above_the_filesystem_root() {
        assert_eq!(resolve("/../../").unwrap(), Path::new("/"));
    }

    #[test]
    fn reports_missing_environment_variables() {
        let error = resolve("$FILE_PEEKER_TEST_MISSING_PATH_VARIABLE_8ED5C718")
            .expect_err("missing variable should fail");
        assert_eq!(error.kind(), FsErrorKind::InvalidArgument);
    }

    /// The point of the escape: a path no `str` can hold still resolves, and still
    /// gets `~` and `$VARIABLE` expansion on the way through. The inputs here are
    /// escaped form, so they are built with `from_escaped` rather than
    /// `from_literal`, which would escape the marker a second time.
    #[cfg(unix)]
    #[test]
    fn resolves_a_path_that_is_not_utf8() {
        use std::{ffi::OsStr, os::unix::ffi::OsStrExt as _};

        let working_directory = std::env::current_dir().unwrap();
        let expected = working_directory.join(OsStr::from_bytes(b"caf\x80"));
        let escaped = |path: &str| HostPath::from_escaped(path.to_owned()).unwrap();

        assert_eq!(resolve_path(&escaped("caf%80")).unwrap(), expected);
        assert_eq!(
            resolve_path(&escaped(&format!(
                "{}/caf%80",
                HostPath::from_path(&working_directory).as_escaped()
            )))
            .unwrap(),
            expected
        );
    }

    /// A literal marker survives resolution as one marker, which is what makes the
    /// escape invertible for ordinary names too.
    #[test]
    fn resolves_a_path_holding_a_literal_marker() {
        let expected = std::env::current_dir().unwrap().join("100%.txt");
        assert_eq!(resolve("100%.txt").unwrap(), expected);
    }

    /// A path that did not come out of the escape is the caller's mistake, and it is
    /// refused rather than guessed at.
    #[test]
    fn rejects_a_path_that_is_not_correctly_escaped() {
        let error = resolve_path(&HostPath::from_unvalidated_for_test("a%2Fb"))
            .expect_err("a smuggled separator should fail");
        assert_eq!(error.kind(), FsErrorKind::InvalidArgument);
    }
}
