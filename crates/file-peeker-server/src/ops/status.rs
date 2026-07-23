use file_peeker_core::{FsError, FsErrorKind};
use tonic::Status;

pub(super) fn fs_status(error: &FsError) -> Status {
    match error.kind() {
        FsErrorKind::InvalidArgument => Status::invalid_argument(error.message()),
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
    use file_peeker_core::{FsError, FsErrorKind};
    use tonic::Code;

    use super::fs_status;

    #[test]
    fn preserves_core_error_codes_and_messages() {
        let cases = [
            (FsErrorKind::InvalidArgument, Code::InvalidArgument),
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
