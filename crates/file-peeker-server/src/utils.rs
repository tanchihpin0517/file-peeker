use std::path::{Path, PathBuf};

pub(crate) fn resolve_path(path: &str) -> Result<PathBuf, String> {
    shellexpand::path::full(Path::new(path))
        .map(std::borrow::Cow::into_owned)
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::resolve_path;

    #[test]
    fn passes_paths_without_expressions_through() {
        assert_eq!(
            resolve_path("relative/reports").unwrap(),
            Path::new("relative/reports")
        );
        assert_eq!(
            resolve_path("/tmp/example").unwrap(),
            Path::new("/tmp/example")
        );
    }

    #[test]
    fn expands_tilde_and_environment_variables() {
        assert_ne!(resolve_path("~").unwrap(), Path::new("~"));
        assert_ne!(resolve_path("$PATH").unwrap(), Path::new("$PATH"));
    }

    #[test]
    fn reports_missing_environment_variables() {
        assert!(resolve_path("$FILE_PEEKER_TEST_MISSING_PATH_VARIABLE_8ED5C718").is_err());
    }
}
