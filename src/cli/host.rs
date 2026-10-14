//! The commands a client runs on a host over SSH: `fpk probe` and `fpk bridge`.
//! Hidden from `--help`, because a person never needs to type them.

use std::{fs, process::ExitCode};

use tokio::net::UnixStream;

use crate::{
    build_info,
    client::{self, remote::Wanted},
    home::Home,
    protocol::{CLIENT_PROTOCOL, VERSION, probe::Probe},
};

/// The exit code of a bridge that found no server to relay to, so the client
/// can tell "stopped" from "unreachable".
const NO_SERVER: u8 = 3;

/// Prints what is installed here as one line of JSON.
pub(super) async fn probe(home: &Home) -> Result<(), String> {
    let report = Probe {
        installed: true,
        version: VERSION.to_owned(),
        client_protocol: CLIENT_PROTOCOL,
        build: build_info::kind().to_owned(),
        source_hash: source_hash(),
        server_running: client::is_running(home).await,
    };
    let line = serde_json::to_string(&report).map_err(|error| error.to_string())?;
    println!("{line}");
    Ok(())
}

/// What this client installs on a machine: `{"wanted": "dev, source a1b2…",
/// "source_hash": "…"}`. For debugging an install, and for tests that set up a
/// host as if one had run.
pub(super) fn build_info() -> Result<(), String> {
    let wanted = Wanted::for_this_build()?;
    let hash = match &wanted {
        Wanted::Release { .. } => None,
        Wanted::Dev { source } => Some(source.hash.clone()),
    };
    let line = serde_json::json!({
        "wanted": wanted.describe(),
        "source_hash": hash,
    });
    println!("{line}");
    Ok(())
}

/// The hash a dev install recorded beside this binary, if it is one.
fn source_hash() -> Option<String> {
    let binary = std::env::current_exe().ok()?;
    let recorded = fs::read_to_string(binary.with_file_name("fpk.source")).ok()?;
    Some(recorded.trim().to_owned()).filter(|hash| !hash.is_empty())
}

/// Relays standard input and output to this host's client socket until either
/// side closes. Never starts a server: a reconnect must not bring back one the
/// user stopped.
pub(super) async fn bridge(home: &Home) -> Result<ExitCode, String> {
    let Ok(mut socket) = UnixStream::connect(home.client_socket()).await else {
        eprintln!("fpk: no server is running on this host");
        return Ok(ExitCode::from(NO_SERVER));
    };
    let mut stdio = tokio::io::join(tokio::io::stdin(), tokio::io::stdout());
    match tokio::io::copy_bidirectional(&mut stdio, &mut socket).await {
        Ok(_) => Ok(ExitCode::SUCCESS),
        // The far end going away mid-stream is how a bridge usually ends.
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::ConnectionReset
            ) =>
        {
            Ok(ExitCode::SUCCESS)
        }
        Err(error) => Err(format!("the bridge failed: {error}")),
    }
}
