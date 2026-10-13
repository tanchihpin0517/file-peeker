//! A client of the public API socket.

use std::{io, time::Duration};

use serde_json::Value;
use thiserror::Error;
use tokio::{
    io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader},
    net::{UnixStream, unix::OwnedReadHalf, unix::OwnedWriteHalf},
};

use crate::{
    home::Home,
    protocol::api::{ApiError, METHOD_STATUS, METHOD_STOP, Request, Response, ServerStatus},
};

/// How long `stop` waits for the server to remove its socket.
const STOP_TIMEOUT: Duration = Duration::from_secs(10);
const STOP_POLL: Duration = Duration::from_millis(25);

#[derive(Debug, Error)]
pub(crate) enum ApiClientError {
    #[error("no server is running")]
    NotRunning,
    #[error("API connection failed: {0}")]
    Io(#[from] io::Error),
    #[error("the server sent an unreadable response: {0}")]
    Decode(#[from] serde_json::Error),
    #[error("the server closed the connection without answering")]
    NoResponse,
    #[error("{}: {}", .0.code, .0.message)]
    Api(ApiError),
    #[error("the server did not stop within {}s", STOP_TIMEOUT.as_secs())]
    StopTimeout,
}

pub(crate) struct ApiClient {
    lines: tokio::io::Lines<BufReader<OwnedReadHalf>>,
    write: OwnedWriteHalf,
    next_id: u64,
}

impl ApiClient {
    /// Connects to `home`'s API socket. A missing or refusing socket is
    /// [`ApiClientError::NotRunning`], not an I/O failure.
    pub(crate) async fn connect(home: &Home) -> Result<Self, ApiClientError> {
        let stream = UnixStream::connect(home.api_socket())
            .await
            .map_err(|error| match error.kind() {
                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused => {
                    ApiClientError::NotRunning
                }
                _ => ApiClientError::Io(error),
            })?;
        let (read, write) = stream.into_split();
        Ok(Self {
            lines: BufReader::new(read).lines(),
            write,
            next_id: 1,
        })
    }

    pub(crate) async fn call(&mut self, method: &str) -> Result<Value, ApiClientError> {
        let id = self.next_id;
        self.next_id += 1;
        let mut line = serde_json::to_vec(&Request {
            id: Some(id),
            method: method.to_owned(),
            params: Value::Null,
        })?;
        line.push(b'\n');
        self.write.write_all(&line).await?;
        let line = self
            .lines
            .next_line()
            .await?
            .ok_or(ApiClientError::NoResponse)?;
        let response: Response = serde_json::from_str(&line)?;
        match (response.result, response.error) {
            (_, Some(error)) => Err(ApiClientError::Api(error)),
            (Some(result), None) => Ok(result),
            (None, None) => Err(ApiClientError::NoResponse),
        }
    }

    pub(crate) async fn status(&mut self) -> Result<ServerStatus, ApiClientError> {
        Ok(serde_json::from_value(self.call(METHOD_STATUS).await?)?)
    }
}

/// Asks the server for `home` to stop, and waits until it has removed its
/// client socket, so a following `fpk` starts a fresh one rather than racing the
/// old one's shutdown.
///
/// Returns whether a server was running.
pub(crate) async fn stop(home: &Home) -> Result<bool, ApiClientError> {
    let mut client = match ApiClient::connect(home).await {
        Ok(client) => client,
        Err(ApiClientError::NotRunning) => return Ok(false),
        Err(error) => return Err(error),
    };
    client.call(METHOD_STOP).await?;
    let socket = home.client_socket();
    let deadline = tokio::time::Instant::now() + STOP_TIMEOUT;
    while tokio::time::Instant::now() < deadline {
        if !socket.exists() {
            return Ok(true);
        }
        tokio::time::sleep(STOP_POLL).await;
    }
    Err(ApiClientError::StopTimeout)
}
