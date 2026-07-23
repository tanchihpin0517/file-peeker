use std::{io, path::Path};

use file_peeker_server::protocol::v1::{ResolvePathRequest, file_peeker_client::FilePeekerClient};

use super::error::operation_status_error;
use crate::session::backend::connection::RemoteConnection;

pub(super) async fn resolve_path(connection: &RemoteConnection, path: &str) -> io::Result<String> {
    let channel = connection.channel()?;
    let request = connection.request(ResolvePathRequest {
        path: path.to_owned(),
    })?;

    let path = FilePeekerClient::new(channel)
        .resolve_path(request)
        .await
        .map_err(|status| operation_status_error(&status))?
        .into_inner()
        .path;
    validate_absolute_path(path, "resolved path")
}

fn validate_absolute_path(path: String, description: &str) -> io::Result<String> {
    if Path::new(&path).is_absolute() {
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
        let error = validate_absolute_path("relative/path".into(), "resolved path").unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(
            error.to_string(),
            "server returned a non-absolute resolved path"
        );
    }
}
