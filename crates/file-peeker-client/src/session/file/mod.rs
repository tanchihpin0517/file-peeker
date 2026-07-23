mod open;
mod opener;
mod service;
mod stage;

pub(crate) use service::FileService;

fn with_context(error: &std::io::Error, context: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::new(error.kind(), format!("{context}: {error}"))
}
