use crate::fs::{FsError, FsErrorKind};
use crate::host_path::HostPath;
use tonic::Status;

/// Takes a selected-host path off the wire.
///
/// Paths cross as an escaped form, which is how a name the host holds as bytes fits
/// a protobuf `string` without losing one. A path that does not decode therefore did
/// not come from a peer speaking this protocol, and is refused as the invalid
/// argument it is rather than guessed at.
pub(super) fn wire_path(path: String) -> Result<HostPath, Status> {
    HostPath::from_escaped(path).map_err(|error| fs_status(&FsError::from(error)))
}

pub(super) fn fs_status(error: &FsError) -> Status {
    match error.kind() {
        FsErrorKind::InvalidArgument => Status::invalid_argument(error.message()),
        FsErrorKind::AlreadyExists => Status::already_exists(error.message()),
        FsErrorKind::NotFound => Status::not_found(error.message()),
        FsErrorKind::PermissionDenied => Status::permission_denied(error.message()),
        FsErrorKind::NotDirectory | FsErrorKind::NotFile => {
            Status::failed_precondition(error.message())
        }
        FsErrorKind::Cancelled => Status::cancelled(error.message()),
        FsErrorKind::Internal => Status::internal(error.message()),
    }
}

#[cfg(test)]
mod tests {
    use crate::fs::{FsError, FsErrorKind};
    use tonic::Code;

    use super::fs_status;

    #[test]
    fn preserves_core_error_codes_and_messages() {
        let cases = [
            (FsErrorKind::InvalidArgument, Code::InvalidArgument),
            (FsErrorKind::AlreadyExists, Code::AlreadyExists),
            (FsErrorKind::NotFound, Code::NotFound),
            (FsErrorKind::PermissionDenied, Code::PermissionDenied),
            (FsErrorKind::NotDirectory, Code::FailedPrecondition),
            (FsErrorKind::NotFile, Code::FailedPrecondition),
            (FsErrorKind::Cancelled, Code::Cancelled),
            (FsErrorKind::Internal, Code::Internal),
        ];
        for (kind, code) in cases {
            let error = FsError::new(kind, "fixture failure");
            let status = fs_status(&error);
            assert_eq!(status.code(), code);
            assert_eq!(status.message(), "fixture failure");
        }
    }
}
