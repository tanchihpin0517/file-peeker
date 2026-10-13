use std::{io, path::Path};

use crate::protocol::v1::{ResolvePathRequest, file_peeker_client::FilePeekerClient};

use super::{entry::wire_path, error::operation_status_error};
use crate::{client::connection::ServerConnection, host_path::HostPath};

pub(crate) async fn resolve_path(
    connection: &ServerConnection,
    path: &HostPath,
) -> io::Result<HostPath> {
    let channel = connection.channel();
    let request = connection.request(ResolvePathRequest {
        path: path.as_escaped().to_owned(),
    })?;

    let path = FilePeekerClient::new(channel)
        .resolve_path(request)
        .await
        .map_err(|status| operation_status_error(&status))?
        .into_inner()
        .path;
    validate_absolute_path(wire_path(path, "resolved path")?, "resolved path")
}

/// Checked on the escaped form, which holds the same components the real path does.
fn validate_absolute_path(path: HostPath, description: &str) -> io::Result<HostPath> {
    if Path::new(path.as_escaped()).is_absolute() {
        Ok(path)
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("server returned a non-absolute {description}"),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::validate_absolute_path;

    #[test]
    fn rejects_non_absolute_path_responses() {
        let error = validate_absolute_path(
            crate::host_path::HostPath::from_literal("relative/path"),
            "resolved path",
        )
        .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(
            error.to_string(),
            "server returned a non-absolute resolved path"
        );
    }
}
