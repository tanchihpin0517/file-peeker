use std::io;

use file_peeker_protocol::{
    ClientMessage, PROTOCOL_VERSION, ServerMessage,
    io::{read_message, send_message},
};
use serde::Deserialize;
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout},
};

use super::{BufTcpStream, ConnectionInfo};

const SERVER_STARTUP_PREFIX: &str = "FILE_PEEKER_SERVER_STARTUP=";
pub(super) const SERVER_READY_PREFIX: &str = "FILE_PEEKER_SERVER_READY=";
pub(super) const SERVER_ERROR_PREFIX: &str = "FILE_PEEKER_SERVER_ERROR=";
pub(super) const ENSURE_SERVER_SCRIPT: &str = include_str!("ensure-server.sh");
const ENSURE_SERVER_HEREDOC: &str = "FILE_PEEKER_ENSURE_SERVER_SCRIPT";

#[derive(Deserialize)]
struct ServerStartupResponse {
    port: u16,
    token: String,
}

pub(super) async fn read_server_startup(
    server_stdout: &mut (impl AsyncBufRead + Unpin),
) -> io::Result<ConnectionInfo> {
    let startup_json = loop {
        let mut line = String::new();
        if server_stdout.read_line(&mut line).await? == 0 || !line.ends_with('\n') {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "server closed stdout before reporting startup",
            ));
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if let Some(json) = line.strip_prefix(SERVER_STARTUP_PREFIX) {
            break json.to_owned();
        }
    };
    let startup: ServerStartupResponse = serde_json::from_str(&startup_json).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("server reported invalid startup JSON: {error}"),
        )
    })?;
    if startup.token.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "server reported an empty token",
        ));
    }

    Ok(ConnectionInfo {
        server_port: startup.port,
        token: startup.token,
    })
}

pub(super) async fn ensure_server_executable(
    server_stdin: &mut (impl AsyncWrite + Unpin),
    server_stdout: &mut (impl AsyncBufRead + Unpin),
    force_install: bool,
    command_output: &mut (impl AsyncWrite + Unpin),
) -> io::Result<String> {
    server_stdin
        .write_all(ensure_server_command(force_install).as_bytes())
        .await?;
    server_stdin.flush().await?;

    loop {
        let mut line = String::new();
        if server_stdout.read_line(&mut line).await? == 0 || !line.ends_with('\n') {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "server installer closed stdout before reporting a result",
            ));
        }
        let status_line = line.trim_end_matches(['\r', '\n']);
        if let Some(executable) = status_line.strip_prefix(SERVER_READY_PREFIX) {
            return Ok(executable.to_owned());
        }
        if let Some(message) = status_line.strip_prefix(SERVER_ERROR_PREFIX) {
            return Err(io::Error::other(message.to_owned()));
        }
        command_output.write_all(line.as_bytes()).await?;
        command_output.flush().await?;
    }
}

pub(super) fn ensure_server_command(force_install: bool) -> String {
    let version = env!("CARGO_PKG_VERSION");
    let force_install = if force_install { "true" } else { "false" };
    format!(
        "sh -s -- '{version}' '{force_install}' <<'{ENSURE_SERVER_HEREDOC}'\n{ENSURE_SERVER_SCRIPT}{ENSURE_SERVER_HEREDOC}\n"
    )
}

pub(super) fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

pub(super) async fn authenticate_stream<W>(stream: &mut W, token: &str) -> io::Result<()>
where
    W: AsyncWrite + Unpin,
{
    send_message(
        stream,
        &ClientMessage::Auth {
            token: token.to_owned(),
        },
    )
    .await
}

pub(super) async fn initialize_control(stream: &mut BufTcpStream, token: &str) -> io::Result<()> {
    authenticate_stream(stream, token).await?;
    hello_control(stream).await?;
    heartbeat_control(stream).await
}

pub(super) async fn heartbeat_server(control: Option<&mut BufTcpStream>) -> io::Result<()> {
    let control = control.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotConnected,
            "control stream is not connected",
        )
    })?;
    heartbeat_control(control).await
}

pub(super) async fn hello_control(stream: &mut BufTcpStream) -> io::Result<()> {
    let response = exchange_control(
        stream,
        &ClientMessage::Hello {
            version: PROTOCOL_VERSION,
        },
    )
    .await?;
    match response {
        ServerMessage::HelloOk { version } if version == PROTOCOL_VERSION => Ok(()),
        response => Err(unexpected_control_response("hello", response)),
    }
}

pub(super) async fn heartbeat_control(stream: &mut BufTcpStream) -> io::Result<()> {
    match exchange_control(stream, &ClientMessage::Heartbeat).await? {
        ServerMessage::HeartbeatOk => Ok(()),
        response => Err(unexpected_control_response("heartbeat", response)),
    }
}

pub(super) async fn shutdown_control(stream: &mut BufTcpStream) -> io::Result<()> {
    match exchange_control(stream, &ClientMessage::Shutdown).await? {
        ServerMessage::ShutdownOk => Ok(()),
        response => Err(unexpected_control_response("shutdown", response)),
    }
}

async fn exchange_control(
    stream: &mut BufTcpStream,
    message: &ClientMessage,
) -> io::Result<ServerMessage> {
    send_message(stream, message).await?;
    read_message(stream).await
}

fn unexpected_control_response(operation: &str, response: ServerMessage) -> io::Error {
    match response {
        ServerMessage::Error { code, message } => {
            io::Error::other(format!("server rejected {operation} ({code:?}): {message}"))
        }
        response => io::Error::new(
            io::ErrorKind::InvalidData,
            format!("server returned unexpected {operation} response: {response:?}"),
        ),
    }
}

pub(super) async fn stop_child(child: &mut Child) {
    let _ = child.start_kill();
    let _ = child.wait().await;
}

pub(super) async fn shutdown_server_process(
    control: &mut Option<BufTcpStream>,
    stdin: &mut Option<ChildStdin>,
    stdout: &mut Option<BufReader<ChildStdout>>,
    child: &mut Option<Child>,
) -> io::Result<()> {
    let shutdown = if let Some(mut control) = control.take() {
        shutdown_control(&mut control).await
    } else {
        Ok(())
    };
    drop(stdin.take());
    drop(stdout.take());
    if let Some(mut child) = child.take() {
        stop_child(&mut child).await;
    }
    shutdown
}

pub(super) fn stop_server_process(
    stdin: &mut Option<ChildStdin>,
    stdout: &mut Option<BufReader<ChildStdout>>,
    child: &mut Option<Child>,
) {
    stdin.take();
    stdout.take();
    if let Some(mut child) = child.take() {
        let _ = child.start_kill();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                let _ = child.wait().await;
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::{
        ENSURE_SERVER_SCRIPT, authenticate_stream, ensure_server_command, read_server_startup,
        stop_child,
    };
    use crate::connection::ConnectionInfo;

    #[tokio::test]
    async fn startup_ignores_output_before_prefixed_result() {
        let mut output = Cursor::new(
            "diagnostic\nFILE_PEEKER_SERVER_STARTUP={\"port\":43827,\"token\":\"test-token\"}\n",
        );

        assert_eq!(
            read_server_startup(&mut output).await.unwrap(),
            ConnectionInfo {
                server_port: 43827,
                token: "test-token".into(),
            }
        );
    }

    #[tokio::test]
    async fn startup_rejects_empty_token() {
        let mut output =
            Cursor::new("FILE_PEEKER_SERVER_STARTUP={\"port\":43827,\"token\":\"\"}\n");

        assert_eq!(
            read_server_startup(&mut output).await.unwrap_err().kind(),
            std::io::ErrorKind::InvalidData
        );
    }

    #[tokio::test]
    async fn authentication_is_sent_without_waiting_for_response() {
        use tokio::io::{AsyncReadExt as _, duplex};

        let (mut client, mut server) = duplex(128);

        authenticate_stream(&mut client, "test-token")
            .await
            .unwrap();
        drop(client);
        let mut output = Vec::new();
        server.read_to_end(&mut output).await.unwrap();

        assert_eq!(output, b"{\"type\":\"auth\",\"token\":\"test-token\"}\n");
    }

    #[tokio::test]
    async fn stopping_child_reaps_process() {
        let mut command = tokio::process::Command::new("sh");
        command.arg("-c").arg("sleep 30").kill_on_drop(true);
        let mut child = command.spawn().unwrap();

        stop_child(&mut child).await;

        assert!(child.try_wait().unwrap().is_some());
    }

    #[test]
    fn ensure_command_checks_then_installs_versioned_server() {
        let command = ensure_server_command(false);
        let forced_command = ensure_server_command(true);
        let version = env!("CARGO_PKG_VERSION");

        assert!(command.starts_with(&format!("sh -s -- '{version}' 'false' <<")));
        assert!(forced_command.starts_with(&format!("sh -s -- '{version}' 'true' <<")));
        assert!(command.contains(ENSURE_SERVER_SCRIPT));
        assert!(ENSURE_SERVER_SCRIPT.contains("[ -x \"$server_executable\" ]"));
        assert!(ENSURE_SERVER_SCRIPT.contains("cargo install"));
        assert!(ENSURE_SERVER_SCRIPT.contains("--force"));
        assert!(ENSURE_SERVER_SCRIPT.contains("--root \"$server_root\""));
        assert!(ENSURE_SERVER_SCRIPT.contains("--version \"$server_version\""));
        assert!(ENSURE_SERVER_SCRIPT.contains("--bin file-peeker-server"));
        assert!(
            ENSURE_SERVER_SCRIPT
                .contains("--git https://github.com/tanchihpin0517/file-peeker.git")
        );
        assert!(!ENSURE_SERVER_SCRIPT.contains("--path"));
        assert!(ENSURE_SERVER_SCRIPT.contains("$HOME/.file-peeker/servers/$server_version"));
        assert!(ENSURE_SERVER_SCRIPT.contains(super::SERVER_READY_PREFIX));
        assert!(ENSURE_SERVER_SCRIPT.contains(super::SERVER_ERROR_PREFIX));
    }
}
