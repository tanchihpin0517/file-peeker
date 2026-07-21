use std::{io, path::Path};

use file_peeker_protocol::v1::{CurrentRootRequest, file_peeker_client::FilePeekerClient};
use thiserror::Error;
use tonic::{Request, transport::Channel};

use crate::connection::status_error;

#[derive(Clone, Debug, Eq, Error, PartialEq, uniffi::Error)]
pub enum CurrentRootError {
    #[error("current-root operation failed: {message}")]
    Operation { message: String },
}

impl From<io::Error> for CurrentRootError {
    fn from(error: io::Error) -> Self {
        Self::Operation {
            message: error.to_string(),
        }
    }
}

pub(crate) async fn current_root(
    channel: Channel,
    request: Request<CurrentRootRequest>,
) -> io::Result<String> {
    let path = FilePeekerClient::new(channel)
        .current_root(request)
        .await
        .map_err(status_error)?
        .into_inner()
        .path;
    validate_current_root(path)
}

fn validate_current_root(path: String) -> io::Result<String> {
    if Path::new(&path).is_absolute() {
        Ok(path)
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "server returned a non-absolute current root",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::validate_current_root;

    #[test]
    fn accepts_only_absolute_current_roots() {
        assert_eq!(
            validate_current_root("/remote/home".into()).unwrap(),
            "/remote/home"
        );
        assert_eq!(
            validate_current_root("relative/root".into())
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::InvalidData
        );
    }
}
