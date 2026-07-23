use std::io;

use tonic::Code;

pub(super) fn operation_status_error(status: &tonic::Status) -> io::Error {
    let kind = match status.code() {
        Code::NotFound => io::ErrorKind::NotFound,
        Code::PermissionDenied => io::ErrorKind::PermissionDenied,
        Code::InvalidArgument => io::ErrorKind::InvalidInput,
        Code::FailedPrecondition => io::ErrorKind::NotADirectory,
        Code::Cancelled => io::ErrorKind::Interrupted,
        Code::DeadlineExceeded => io::ErrorKind::TimedOut,
        Code::Unavailable => io::ErrorKind::ConnectionAborted,
        _ => io::ErrorKind::Other,
    };
    io::Error::new(kind, status.message().to_owned())
}

pub(super) fn read_status_error(status: &tonic::Status) -> io::Error {
    if status.code() == Code::FailedPrecondition {
        io::Error::new(io::ErrorKind::IsADirectory, status.message().to_owned())
    } else {
        operation_status_error(status)
    }
}

#[cfg(test)]
mod tests {
    use std::io;

    use file_peeker_core::{FsError, FsErrorKind};
    use tonic::{Code, Status};

    use super::{operation_status_error, read_status_error};
    use crate::session::backend::error::fs_error;

    #[test]
    fn native_and_remote_fs_errors_have_identical_public_results() {
        let cases = [
            (FsErrorKind::InvalidArgument, Code::InvalidArgument),
            (FsErrorKind::NotFound, Code::NotFound),
            (FsErrorKind::PermissionDenied, Code::PermissionDenied),
            (FsErrorKind::NotDirectory, Code::FailedPrecondition),
            (FsErrorKind::Cancelled, Code::Cancelled),
            (FsErrorKind::Internal, Code::Internal),
        ];
        for (core_kind, status_code) in cases {
            let core_error = FsError::new(core_kind, "fixture failure");
            let status = Status::new(status_code, "fixture failure");
            let native = fs_error(&core_error);
            let remote = operation_status_error(&status);
            assert_eq!(native.kind(), remote.kind());
            assert_eq!(native.to_string(), remote.to_string());
        }
    }

    #[test]
    fn native_and_remote_read_wrong_type_errors_match() {
        let core_error = FsError::new(FsErrorKind::NotFile, "fixture failure");
        let status = Status::failed_precondition("fixture failure");

        let native = fs_error(&core_error);
        let remote = read_status_error(&status);

        assert_eq!(native.kind(), io::ErrorKind::IsADirectory);
        assert_eq!(native.kind(), remote.kind());
        assert_eq!(native.to_string(), remote.to_string());
    }
}
