//! File Peeker v2: a persistent server per machine, run by tasks.
//!
//! This crate is the rewrite proposed in `docs/design/v2-architecture.md`. It
//! grows one build step at a time; it depends on nothing in the shipping crate in
//! `old/`, and replaces it at cutover.
//!
//! One binary, `fpk`, runs in one of three roles:
//!
//! - [`server`] is the persistent per-user server: `fpk server`.
//! - [`tui`] is the terminal UI, a client of that server: `fpk`.
//! - [`cli`] is every other command, also a client of it.
//!
//! Both clients reach the server through [`client`].
//!
//! They share only [`home`], which says where the server's files live, and
//! [`protocol`], the two surfaces between them. `client` never names `server`:
//! the only way from one to the other is a socket.

mod cli;
mod client;
mod home;
mod protocol;
mod server;
mod tui;

use std::process::ExitCode;

/// Runs `fpk` with the process's arguments.
#[must_use]
pub fn run() -> ExitCode {
    let invocation = cli::parse();
    let home = match home::Home::from_env() {
        Ok(home) => home,
        Err(error) => {
            eprintln!("fpk: {error}");
            return ExitCode::FAILURE;
        }
    };
    match invocation {
        cli::Invocation::Server => server::run(&home),
        cli::Invocation::Tui => tui::run(&home),
        cli::Invocation::Client(command) => cli::execute(&home, command),
    }
}
