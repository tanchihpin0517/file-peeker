use file_peeker_protocol::v1::CurrentRootResponse;
use tonic::Status;

pub(super) fn current_root() -> Result<CurrentRootResponse, Status> {
    let path = std::env::current_dir()
        .map_err(|error| Status::internal(format!("cannot read current directory: {error}")))?;
    let path = path
        .to_str()
        .ok_or_else(|| Status::invalid_argument("Current directory is not valid UTF-8"))?;
    Ok(CurrentRootResponse {
        path: path.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::current_root;

    #[test]
    fn current_root_returns_the_process_directory() {
        let expected = std::env::current_dir()
            .expect("current directory should be available")
            .to_str()
            .expect("test directory should be UTF-8")
            .to_owned();

        assert_eq!(current_root().unwrap().path, expected);
    }
}
