//! The `file-peeker-client` binary: a test-only CLI for exercising
//! [`Client`](crate::Client) workflows.

mod command;
pub(crate) mod connect;
pub(crate) mod install;
pub(crate) mod list;
pub(crate) mod ssh_connection;
pub(crate) mod start_server;
pub(crate) mod walk;

pub use command::run;

use std::{
    io::{self, Write},
    path::Path,
};

/// Writes one selected-host path, followed by a newline, as the bytes the host
/// holds.
///
/// Not `Path::display`: that replaces whatever is not UTF-8, and a shell cannot
/// address the replacement. This output is meant to survive a pipe, so a filename
/// copied out of it and passed back in names the same file.
pub(crate) fn write_path(output: &mut impl Write, path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;

        output.write_all(path.as_os_str().as_bytes())?;
    }
    // Off Unix there are no bytes to write; see `HostPath::to_path_buf`.
    #[cfg(not(unix))]
    {
        write!(output, "{}", path.display())?;
    }
    output.write_all(b"\n")
}
