use std::path::{Component, Path, PathBuf};

use crate::{FsError, FsErrorKind};

pub(crate) fn resolve_path(path: &str) -> Result<PathBuf, FsError> {
    let expanded = shellexpand::path::full(Path::new(path))
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
    use crate::FsErrorKind;

    #[test]
    fn resolves_dot_to_the_absolute_working_directory() {
        assert_eq!(resolve_path(".").unwrap(), std::env::current_dir().unwrap());
    }

    #[test]
    fn expands_tilde_and_environment_variables() {
        assert!(resolve_path("~").unwrap().is_absolute());
        assert!(resolve_path("$PATH").unwrap().is_absolute());
    }

    #[test]
    fn normalizes_relative_components_without_requiring_existence() {
        let expected = std::env::current_dir().unwrap().join("missing-child");
        assert_eq!(
            resolve_path("missing-parent/../missing-child/.").unwrap(),
            expected
        );
    }

    #[test]
    fn resolved_paths_are_idempotent() {
        let resolved = std::env::current_dir().unwrap().join("already-resolved");
        assert_eq!(resolve_path(resolved.to_str().unwrap()).unwrap(), resolved);
    }

    #[test]
    fn does_not_walk_above_the_filesystem_root() {
        assert_eq!(resolve_path("/../../").unwrap(), Path::new("/"));
    }

    #[test]
    fn reports_missing_environment_variables() {
        let error = resolve_path("$FILE_PEEKER_TEST_MISSING_PATH_VARIABLE_8ED5C718")
            .expect_err("missing variable should fail");
        assert_eq!(error.kind(), FsErrorKind::InvalidArgument);
    }
}
