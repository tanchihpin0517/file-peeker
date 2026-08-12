use std::io;

use tonic::Code;

pub(super) fn operation_status_error(status: &tonic::Status) -> io::Error {
    let kind = match status.code() {
        Code::NotFound => io::ErrorKind::NotFound,
        Code::PermissionDenied => io::ErrorKind::PermissionDenied,
        Code::InvalidArgument => io::ErrorKind::InvalidInput,
        Code::AlreadyExists => io::ErrorKind::AlreadyExists,
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

    use tonic::{Code, Status};

    use super::{operation_status_error, read_status_error};

    /// This is the second half of the only path an `FsError` now takes to a
    /// caller. `server::ops::status` owns the first half — `FsErrorKind` to
    /// `Code` — and pins it in `preserves_core_error_codes_and_messages`, so the
    /// `Code` column below is what that test guarantees the server sends. The
    /// `io::ErrorKind` column is what callers saw before every operation went
    /// over the wire, and must not drift from it.
    #[test]
    fn server_codes_map_back_to_the_original_error_kinds() {
        let cases = [
            // FsErrorKind::InvalidArgument
            (Code::InvalidArgument, io::ErrorKind::InvalidInput),
            // FsErrorKind::AlreadyExists
            (Code::AlreadyExists, io::ErrorKind::AlreadyExists),
            // FsErrorKind::NotFound
            (Code::NotFound, io::ErrorKind::NotFound),
            // FsErrorKind::PermissionDenied
            (Code::PermissionDenied, io::ErrorKind::PermissionDenied),
            // FsErrorKind::NotDirectory
            (Code::FailedPrecondition, io::ErrorKind::NotADirectory),
            // FsErrorKind::Cancelled, which is what a closing connection produces
            (Code::Cancelled, io::ErrorKind::Interrupted),
            // FsErrorKind::Internal
            (Code::Internal, io::ErrorKind::Other),
        ];
        for (code, expected) in cases {
            let error = operation_status_error(&Status::new(code, "fixture failure"));
            assert_eq!(error.kind(), expected, "for {code:?}");
            assert_eq!(error.to_string(), "fixture failure");
        }
    }

    /// `FsErrorKind::NotDirectory` and `NotFile` share `FailedPrecondition`, so
    /// only the operation can tell them apart: a failed read means the path was
    /// not a file.
    #[test]
    fn reads_report_a_wrong_type_path_as_a_directory() {
        let status = Status::failed_precondition("fixture failure");

        let error = read_status_error(&status);

        assert_eq!(error.kind(), io::ErrorKind::IsADirectory);
        assert_eq!(error.to_string(), "fixture failure");
    }
}
