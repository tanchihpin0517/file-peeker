pub(crate) mod copy;

fn with_context(error: &std::io::Error, context: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::new(error.kind(), format!("{context}: {error}"))
}
