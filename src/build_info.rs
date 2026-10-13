//! What kind of build this binary is, which decides how it installs itself on
//! another host.
//!
//! Rust fixes the profile at compile time, so the rule needs no flag: a release
//! build installs a published release of its own version, and a dev build
//! installs the source tree it was compiled from.

/// `cargo build --release`, and every published binary.
pub(crate) const RELEASE: bool = !cfg!(debug_assertions);

/// The directory this binary was compiled in. A dev build uploads the source
/// under it to a remote host and builds it there.
pub(crate) const SOURCE_DIR: &str = env!("CARGO_MANIFEST_DIR");

/// `release` or `dev`, as a probe reports a host's build.
pub(crate) fn kind() -> &'static str {
    if RELEASE { "release" } else { "dev" }
}
