use std::path::PathBuf;

use super::{FsError, FsErrorKind};
use crate::path_string::PathString;

/// The real path an operation opens, exactly as given: where a path leaves its
/// escaped form.
///
/// Nothing is interpreted. `~` is a name like any other, and `.` and `..` go to
/// the kernel, which follows symlinks on the way — folding them as text would
/// not: `/a/link/..` is the parent of where `link` points, not `/a`. Reading what
/// a person typed is the job of whatever took it in, which knows the tab it was
/// typed in.
///
/// # Errors
///
/// [`FsErrorKind::InvalidArgument`] for a relative path: relative to something
/// only the caller knows, and the server's own working directory is wherever the
/// client that started it happened to be.
pub(super) fn absolute(path: &PathString) -> Result<PathBuf, FsError> {
    let real = path.to_path_buf()?;
    if real.is_absolute() {
        Ok(real)
    } else {
        Err(FsError::new(
            FsErrorKind::InvalidArgument,
            format!(
                "`{}` is relative; a path must start with `/`",
                path.display()
            ),
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::{ffi::OsStr, os::unix::ffi::OsStrExt as _, path::Path};

    use super::{FsErrorKind, absolute};
    use crate::path_string::PathString;

    #[test]
    fn an_absolute_path_is_taken_exactly() {
        for path in [
            "/",
            "/srv/x",
            "/srv/../x/./y",
            "/srv/~",
            "/srv/$HOME",
            "//a//b/",
        ] {
            assert_eq!(
                absolute(&PathString::from_literal(path)).unwrap(),
                Path::new(path),
                "{path:?}"
            );
        }
    }

    #[test]
    fn a_relative_path_is_refused_tilde_included() {
        for path in ["", ".", "..", "notes", "./notes", "~", "~/notes", "$HOME/x"] {
            assert_eq!(
                absolute(&PathString::from_literal(path))
                    .unwrap_err()
                    .kind(),
                FsErrorKind::InvalidArgument,
                "{path:?}"
            );
        }
    }

    #[test]
    fn escapes_are_decoded_once() {
        let path = PathString::from_escaped("/srv/caf%80/100%%.txt".to_owned()).unwrap();
        assert_eq!(
            absolute(&path).unwrap(),
            Path::new(OsStr::from_bytes(b"/srv/caf\x80/100%.txt"))
        );
    }
}
