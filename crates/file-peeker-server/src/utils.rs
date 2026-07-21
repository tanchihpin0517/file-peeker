use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
};

pub(crate) fn resolve_path(path: &str) -> Result<PathBuf, &'static str> {
    let home = std::env::var_os("HOME");
    resolve_path_with_home(path, home.as_deref())
}

fn resolve_path_with_home(path: &str, home: Option<&OsStr>) -> Result<PathBuf, &'static str> {
    if Path::new(path).is_absolute() {
        return Ok(PathBuf::from(path));
    }

    let relative = if path == "~" {
        ""
    } else if let Some(relative) = path.strip_prefix("~/") {
        relative.trim_start_matches('/')
    } else {
        return Err("Listing path must be absolute or start with `~/`");
    };
    let home = home
        .map(Path::new)
        .filter(|home| home.is_absolute())
        .ok_or("Cannot resolve `~`: server HOME must be an absolute path")?;
    Ok(home.join(relative))
}

#[cfg(test)]
mod tests {
    use std::{ffi::OsStr, path::Path};

    use super::resolve_path_with_home;

    #[test]
    fn resolves_tilde_against_the_server_home() {
        let home = OsStr::new("/home/example");

        assert_eq!(
            resolve_path_with_home("~", Some(home)).unwrap(),
            Path::new("/home/example")
        );
        assert_eq!(
            resolve_path_with_home("~/Documents", Some(home)).unwrap(),
            Path::new("/home/example/Documents")
        );
        assert_eq!(
            resolve_path_with_home("/tmp/example", Some(home)).unwrap(),
            Path::new("/tmp/example")
        );
    }

    #[test]
    fn rejects_unsupported_or_unresolvable_paths() {
        assert!(resolve_path_with_home("relative", Some(OsStr::new("/home/example"))).is_err());
        assert!(resolve_path_with_home("~alice", Some(OsStr::new("/home/example"))).is_err());
        assert!(resolve_path_with_home("~", None).is_err());
        assert!(resolve_path_with_home("~", Some(OsStr::new("relative/home"))).is_err());
    }
}
