use std::{
    ffi::OsStr,
    path::{Component, Path, PathBuf},
};

use super::{FsError, FsErrorKind};
use crate::path_string::PathString;

/// Turns a path a person or a client wrote into the absolute path it names, as a
/// `PathString`, without touching the filesystem: the path need not exist, and
/// symlinks are left as they are.
///
/// - A leading `~` is this server's `HOME`, and only a leading one: a directory
///   really named `~` further along stays itself, and `~user` is refused.
/// - `.` and `..` are folded lexically, and `..` stops at `/`.
/// - Nothing else is expanded. A `$VARIABLE` would read the server's
///   environment — whatever the client that started it had — not the user's.
///
/// # Errors
///
/// [`FsErrorKind::InvalidArgument`] for a relative path or one that is not
/// correctly escaped; [`FsErrorKind::Internal`] when `~` is used and the server
/// has no absolute `HOME`.
pub(crate) fn resolve_path(path: &PathString) -> Result<PathString, FsError> {
    Ok(PathString::from_path(&real_path(path)?))
}

/// The real path an operation opens: where a path leaves its escaped form.
pub(super) fn real_path(path: &PathString) -> Result<PathBuf, FsError> {
    resolve_in(path, std::env::var_os("HOME").as_deref().map(Path::new))
}

/// [`real_path`] against a given `HOME`, so tests need not set the process's.
fn resolve_in(path: &PathString, home: Option<&Path>) -> Result<PathBuf, FsError> {
    let path = path.to_path_buf()?;
    let mut components = path.components();
    let absolute = match components.clone().next() {
        Some(Component::Normal(first)) if first == OsStr::new("~") => {
            components.next();
            let home = home.filter(|home| home.is_absolute()).ok_or_else(|| {
                FsError::new(
                    FsErrorKind::Internal,
                    "cannot expand `~`: the server has no absolute HOME",
                )
            })?;
            home.join(components.as_path())
        }
        _ if path.is_absolute() => path,
        _ => {
            return Err(FsError::new(
                FsErrorKind::InvalidArgument,
                format!(
                    "`{}` is relative; a path must start with `/` or `~`",
                    path.display()
                ),
            ));
        }
    };
    Ok(normalize(&absolute))
}

fn normalize(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => normalized.push(component),
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(name) => normalized.push(name),
        }
    }
    normalized
}

#[cfg(test)]
mod tests {
    use std::{ffi::OsStr, os::unix::ffi::OsStrExt as _, path::Path};

    use super::{FsErrorKind, resolve_in};
    use crate::path_string::PathString;

    const HOME: &str = "/home/me";

    fn resolve(path: &str) -> Result<std::path::PathBuf, FsErrorKind> {
        resolve_in(&PathString::from_literal(path), Some(Path::new(HOME)))
            .map_err(|error| error.kind())
    }

    #[test]
    fn a_leading_tilde_is_home() {
        assert_eq!(resolve("~"), Ok(HOME.into()));
        assert_eq!(resolve("~/"), Ok(HOME.into()));
        assert_eq!(resolve("~/notes/a.txt"), Ok("/home/me/notes/a.txt".into()));
        assert_eq!(resolve("~/../other"), Ok("/home/other".into()));
    }

    #[test]
    fn a_tilde_anywhere_else_is_a_name() {
        assert_eq!(resolve("/srv/~/x"), Ok("/srv/~/x".into()));
        assert_eq!(resolve("/srv/~"), Ok("/srv/~".into()));
    }

    #[test]
    fn a_relative_path_is_refused() {
        for path in [
            "", ".", "notes", "./notes", "../notes", "~user", "~user/x", "$HOME",
        ] {
            assert_eq!(resolve(path), Err(FsErrorKind::InvalidArgument), "{path:?}");
        }
    }

    #[test]
    fn nothing_but_a_leading_tilde_is_expanded() {
        assert_eq!(resolve("/srv/$HOME"), Ok("/srv/$HOME".into()));
    }

    #[test]
    fn dots_fold_without_the_path_existing() {
        assert_eq!(
            resolve("/missing-parent/../missing-child/./x"),
            Ok("/missing-child/x".into())
        );
        assert_eq!(resolve("/../../"), Ok("/".into()));
        assert_eq!(resolve("/a//b/"), Ok("/a/b".into()));
    }

    #[test]
    fn resolving_twice_changes_nothing() {
        let once = resolve("~/a/../b").unwrap();
        assert_eq!(
            resolve_in(&PathString::from_path(&once), Some(Path::new(HOME))).unwrap(),
            once
        );
    }

    #[test]
    fn a_tilde_needs_an_absolute_home() {
        let tilde = PathString::from_literal("~/x");
        for home in [None, Some(Path::new("relative"))] {
            assert_eq!(
                resolve_in(&tilde, home).unwrap_err().kind(),
                FsErrorKind::Internal
            );
        }
        // A path with no `~` never asks.
        assert!(resolve_in(&PathString::from_literal("/x"), None).is_ok());
    }

    #[test]
    fn bytes_outside_utf8_and_literal_markers_survive() {
        let escaped = PathString::from_escaped("~/caf%80/100%%.txt".to_owned()).unwrap();
        assert_eq!(
            resolve_in(&escaped, Some(Path::new(HOME))).unwrap(),
            Path::new(OsStr::from_bytes(b"/home/me/caf\x80/100%.txt"))
        );
    }

    #[test]
    fn the_public_form_comes_back_escaped() {
        let path = PathString::from_escaped("/srv/caf%80/./x".to_owned()).unwrap();
        assert_eq!(super::resolve_path(&path).unwrap(), "/srv/caf%80/x");
    }
}
