//! The `file-peeker-client` binary: a test-only CLI for exercising
//! [`Client`](crate::Client) and [`Session`](crate::Session) workflows.

mod command;
pub(crate) mod connect;
pub(crate) mod install;
pub(crate) mod list;
pub(crate) mod open;
pub(crate) mod ssh_connection;
pub(crate) mod start_server;
pub(crate) mod walk;

pub use command::run;
